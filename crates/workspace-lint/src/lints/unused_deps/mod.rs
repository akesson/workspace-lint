//! Resolver-backed unused-dependencies check.
//!
//! For each workspace member, compares declared deps against the set of
//! crate names appearing in the resolver's per-crate reference index. A dep
//! is flagged unused if its underscore-normalized name doesn't appear in
//! that set.
//!
//! Inputs come from three sources, all already loaded on the resolver:
//!
//! - **`Crate::declared_deps`** — the manifest's enumerated dep list across
//!   `[dependencies]`, `[dev-dependencies]`, and `[build-dependencies]`.
//! - **`Workspace::references_from_crate`** — the canonical-path set the
//!   crate touches (use statements + regular code paths + macro-body refs).
//! - **`Workspace::doctest_dep_refs`** — crate names referenced inside
//!   doc-test code fences (a dep used only in a `/// ```rust …` example is
//!   still genuinely used). Kept separate from the reference graph above so it
//!   feeds only this lint, never `unused-pub` / the SCIP projection.
//! - **`Manifest::feature_dep_refs`** — dep names a crate's `[features]` table
//!   forwards (`dep:foo`, `foo?/bar`). A feature-plumbing-only optional dep,
//!   declared just to forward a feature and never named in code, is still
//!   genuinely depended on, so these count as references.
//!
//! Known limitations (documented in tests/cases/unused-deps/):
//!
//! - `build.rs`-generated code and `*-sys` link-only deps still produce false
//!   positives; the existing `ignore` knob suppresses them.

use std::collections::{BTreeMap, HashMap, HashSet};
use syn_workspace::Workspace;
use syn_workspace::manifest::{DeclaredDep, DepSection, Manifest};

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_crate;
use crate::diagnostic::{Applicability, Evidence, Span, Suggestion};
use crate::lints::{Lint, LintContext, LintId, Requirements};

pub mod config;
#[cfg(test)]
mod tests;

pub(crate) use config::UnusedDepsConfig;

pub(crate) struct UnusedDeps {
    /// Workspace-wide params, used for any crate without a per-crate section.
    global: UnusedDepsConfig,
    /// Per-crate params (keyed by Cargo package name), each *wholesale*
    /// replacing the global config for that crate. Empty for CLI single-check
    /// runs, which have no `[crates.*]` tier.
    per_crate: HashMap<String, UnusedDepsConfig>,
}

impl UnusedDeps {
    pub fn new(global: UnusedDepsConfig, per_crate: HashMap<String, UnusedDepsConfig>) -> Self {
        Self { global, per_crate }
    }

    pub fn from_cli(ignore: Vec<String>) -> Self {
        Self::new(UnusedDepsConfig { ignore }, HashMap::new())
    }
}

impl Lint for UnusedDeps {
    fn id(&self) -> LintId {
        LintId::UnusedDeps
    }

    fn requirements(&self) -> Requirements {
        Requirements {
            needs_workspace: true,
        }
    }

    fn check(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let workspace = cx
            .workspace
            .expect("unused-deps lint requires Workspace (Requirements::needs_workspace)");
        check(&self.global, &self.per_crate, workspace)
    }
}

pub(crate) fn check(
    global: &UnusedDepsConfig,
    per_crate: &HashMap<String, UnusedDepsConfig>,
    workspace: &Workspace,
) -> Vec<Diagnostic> {
    let lint_id = LintId::UnusedDeps.id();
    let mut diagnostics = Vec::new();

    for krate in workspace.members() {
        // A per-crate `[crates.<name>.unused-deps]` wholesale-replaces the
        // global params for this crate; otherwise the global config applies.
        let config = per_crate.get(&krate.name).unwrap_or(global);
        let manifest = krate.manifest();
        let deps = collect_deps(manifest, &config.ignore);
        if deps.is_empty() {
            continue;
        }

        let referenced_crates = referenced_crate_names(workspace, krate);

        let unused = find_unused_deps(deps, &referenced_crates);
        if unused.is_empty() {
            continue;
        }

        let n = unused.len();
        // Anchor and message both use the workspace-relative path so a
        // per-Cargo.toml `# workspace-lint: allow(unused-deps)` directive
        // matches the crate-anchored diagnostic shape.
        let manifest_dir_rel = workspace.crate_relative_path(&krate.manifest_dir);
        let manifest_path_rel = workspace.crate_relative_path(manifest.path());
        let cargo_path_str = crate::diagnostic::render::display_path(&manifest_path_rel);
        let mut builder = at_crate(
            lint_id,
            format!(
                "{n} possibly unused dependenc{} in {cargo_path_str}",
                if n == 1 { "y" } else { "ies" },
            ),
            manifest_dir_rel,
        );
        for entry in &unused {
            builder = builder.help(format!(
                "[{}] {}",
                entry.section.as_str(),
                entry.original_name
            ));
            // Deep `--fix` verification matches SCIP symbols by the dep's
            // *package* name (rust-analyzer carries the real package, not the
            // `package = "…"` rename or the lib-name alias), so resolve it now.
            let evidence = Evidence::DepUnused {
                krate_code: krate.code_name(),
                package_name: resolve_package_name(workspace, manifest, entry),
            };
            if let Some(s) = build_delete_suggestion(manifest, entry, Some(evidence)) {
                builder = builder.suggestion(s);
            }
        }
        diagnostics.push(
            builder
                .note("build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives")
                .note("verify by removing the dep and running `cargo build --all-targets`")
                .note("if the build breaks, add the dep to [unused-deps] ignore in your config")
                .build(),
        );
    }

    diagnostics
}

/// Build a `MachineApplicable` suggestion that deletes the entire dep entry
/// (including the trailing newline) from the Cargo.toml. Handles single-line
/// entries via the line locator (which also drops a trailing same-line comment)
/// and the multi-line forms — a wrapped inline table or a `[<section>.dep]`
/// block — via the span locator. Returns `None` only if the entry can't be
/// located at all.
fn build_delete_suggestion(
    manifest: &Manifest,
    entry: &DeclaredDep,
    evidence: Option<Evidence>,
) -> Option<Suggestion> {
    let location = manifest
        .locate_dep(entry.section, &entry.original_name)
        .or_else(|| manifest.locate_dep_entry(entry.section, &entry.original_name))?;
    let mut end = location.byte_end as usize;
    let bytes = manifest.raw().as_bytes();
    if end < bytes.len() && bytes[end] == b'\r' {
        end += 1;
    }
    if end < bytes.len() && bytes[end] == b'\n' {
        end += 1;
    }
    // The deletion may cover several lines (multi-line entry); the fix applies by
    // byte offset, so `line_end` only affects the rendered span. Single-line
    // entries have no interior newline, so this stays `== line_start`.
    let deleted = &manifest.raw()[location.byte_start as usize..location.byte_end as usize];
    let line_end = location.line + deleted.bytes().filter(|&b| b == b'\n').count() as u32;
    Some(Suggestion {
        span: Span {
            file: manifest.path().to_path_buf(),
            line_start: location.line,
            line_end,
            col_start: 1,
            col_end: 1,
            byte_start: location.byte_start,
            byte_end: end as u32,
        },
        message: format!("remove unused dependency `{}`", entry.original_name),
        replacement: String::new(),
        applicability: Applicability::MachineApplicable,
        original: Some(deleted.to_string()),
        evidence,
    })
}

/// The package name a declared dep resolves to, for SCIP symbol matching.
/// A `package = "…"` rename (local or inherited from the root
/// `[workspace.dependencies]`) wins; otherwise the dep key itself is the
/// package. rust-analyzer's SCIP symbols carry the real package name, so this
/// — not the rename alias or a lib-name alias like `md-5`→`md5` — is what the
/// deep verifier matches against.
fn resolve_package_name(workspace: &Workspace, manifest: &Manifest, entry: &DeclaredDep) -> String {
    if let Some(pkg) = manifest.dep_package_name(entry.section, &entry.original_name) {
        return pkg;
    }
    if manifest.dep_uses_workspace(entry.section, &entry.original_name)
        && let Some(pkg) = workspace
            .root_manifest()
            .dep_package_name(DepSection::WorkspaceDependencies, &entry.original_name)
    {
        return pkg;
    }
    entry.original_name.clone()
}

fn referenced_crate_names(workspace: &Workspace, krate: &syn_workspace::Crate) -> HashSet<String> {
    let mut names: HashSet<String> = workspace
        .references_from_crate(krate)
        .map(|refs| {
            refs.iter()
                .filter_map(|p| p.crate_name().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // A dep referenced only inside a doc-test code fence is still genuinely
    // used (the doc-test won't compile without it). These come from a separate
    // channel kept out of the occurrence graph, so union them in here.
    if let Some(doc_refs) = workspace.doctest_dep_refs(krate) {
        names.extend(doc_refs.iter().cloned());
    }
    // A feature-plumbing-only optional dep — declared solely to forward a Cargo
    // feature (`dep:foo`, `foo?/bar`) and never named in code — is still
    // genuinely depended on. Read those names straight from the manifest's
    // `[features]` table (pure manifest data, no resolver model needed).
    names.extend(krate.manifest().feature_dep_refs());
    names
}

fn collect_deps(manifest: &Manifest, ignore: &[String]) -> BTreeMap<String, Vec<DeclaredDep>> {
    let mut deps: BTreeMap<String, Vec<DeclaredDep>> = BTreeMap::new();
    for dep in manifest.declared_deps() {
        if ignore.iter().any(|i| i == &dep.original_name) {
            continue;
        }
        let entries = deps.entry(dep.normalized_name.clone()).or_default();
        // The same dep can be declared under several `[target.<cfg>.…]` tables
        // in one section (e.g. chrono with different features per platform);
        // collapse those so it's reported once. Distinct *sections*
        // (`[dependencies]` vs `[dev-dependencies]`) stay separate entries.
        if entries
            .iter()
            .any(|e| e.section == dep.section && e.original_name == dep.original_name)
        {
            continue;
        }
        entries.push(dep);
    }
    deps
}

fn find_unused_deps(
    deps: BTreeMap<String, Vec<DeclaredDep>>,
    referenced: &HashSet<String>,
) -> Vec<DeclaredDep> {
    // H3 manifest assertion `md5-libname` (DESIGN-ir-pipeline.md §13): a dep
    // also counts as referenced when its *separator-stripped* name matches a
    // referenced crate name verbatim. Package `md-5` ships a lib target named
    // `md5` (https://docs.rs/md-5 — `[lib] name = "md5"`), but `cargo metadata
    // --no-deps` can't surface external lib-target names, so exact normalized
    // matching (`md_5` vs `md5`) misses it.
    //
    // Only the *dep* name is stripped — the referenced set is matched verbatim.
    // Stripping both sides was too loose: a referenced name like a `__axum`
    // macro-interpolation local strips to `axum` and would wrongly vouch for an
    // unrelated `axum` dep. A real lib-name rename always drops a separator the
    // code can't write (`md-5` → `md5`), so stripping the manifest side alone is
    // sufficient. The match is still FP-safe — it only ever suppresses a finding.
    deps.into_iter()
        .filter(|(normalized, _)| {
            !referenced.contains(normalized)
                && !referenced.contains(&crate::util::separator_stripped(normalized))
        })
        .flat_map(|(_, entries)| entries)
        .collect()
}
