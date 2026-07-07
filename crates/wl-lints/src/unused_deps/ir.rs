//! The rustc backend of `unused-deps`: declared deps vs the compiler-resolved
//! reference graph (the extracted IR), unioned across the `[engine]` config
//! matrix.
//!
//! What replaces what, relative to `legacy.rs`:
//!
//! - `Workspace::references_from_crate` (syn's resolver) →
//!   [`wl_engine::semantic::DepUsage`]: every target's compiler-true edges,
//!   folded onto the owning package, facade- and lib-rename-aware via the
//!   resolved dependency closure (`clap` credited by a `clap_builder` edge,
//!   `md-5` by an `md5` one — the resolve graph carries lib-target names the
//!   legacy separator-stripping only approximated).
//! - `Workspace::doctest_dep_refs` → `CrateInfo::doctest_dep_refs` (the
//!   FastModel's syntactic doc-fence scan — doc-test code is a separate
//!   compilation unit the IR never sees).
//! - `Manifest::feature_dep_refs` — unchanged (pure manifest data).
//!
//! Judgement scope the compiler view imposes (the syn backend judged
//! everything against its cfg-blind parse of every source file):
//!
//! - **dev deps** are judged only when a test/example/bench target actually
//!   compiled (a `--tests` entry in `[engine] configs`); otherwise their
//!   usage is invisible and they are skipped, never flagged.
//! - **build deps** are never judged. Build scripts ARE lint-passed (their
//!   `<pkg>@build.wlir` fragments back unused-pub's build.rs-consumer
//!   crediting), but `DepUsage` deliberately finds no owner for the shared
//!   `build_script_build` crate name, so their references credit no
//!   package's `[dependencies]` and `[build-dependencies]` stay unjudged.
//!   The legacy backend parsed `build.rs` syntactically and could judge
//!   them; the `ignore` knob remains the answer for link-only deps.
//!
//! The diagnostic shape (message, helps, notes, suggestion bytes) mirrors
//! `legacy.rs` exactly — fixtures pin the two backends to byte-identical
//! output wherever their verdicts agree.

use std::collections::{BTreeMap, HashMap, HashSet};

use wl_engine::fast::{DeclaredDep, DepSection, FastModel, Manifest};
use wl_engine::semantic::{DepUsage, SemanticModel};

use super::UnusedDepsConfig;
use wl_diagnostic::Diagnostic;
use wl_diagnostic::{Applicability, Span, Suggestion};
use wl_lint_api::LintId;

pub(super) fn check(
    global: &UnusedDepsConfig,
    per_crate: &HashMap<String, UnusedDepsConfig>,
    fast: &FastModel,
    model: &SemanticModel,
) -> Vec<Diagnostic> {
    let lint_id = LintId::UnusedDeps.id();
    let usage = model.dep_usage();
    let mut diagnostics = Vec::new();

    for krate in fast.members() {
        // A per-crate `[crates.<name>.unused-deps]` wholesale-replaces the
        // global params for this crate; otherwise the global config applies.
        let config = per_crate.get(&krate.name).unwrap_or(global);
        let manifest = krate.manifest();
        let deps = collect_deps(manifest, &config.ignore);
        if deps.is_empty() {
            continue;
        }

        // The syntactic half of "referenced": doc-fence refs + feature
        // plumbing — usage the compiled IR structurally can't carry.
        let mut syntactic: HashSet<String> = krate
            .doctest_dep_refs()
            .iter()
            .map(|s| s.to_string())
            .collect();
        syntactic.extend(manifest.feature_dep_refs());

        let owner = krate.code_name();
        let unused = find_unused_deps(
            deps,
            &usage,
            model,
            &owner,
            &syntactic,
            fast.root_manifest(),
            manifest,
        );
        if unused.is_empty() {
            continue;
        }

        let n = unused.len();
        let mut builder = wl_lint_api::util::at_crate_manifest(
            lint_id,
            fast,
            &krate.manifest_dir,
            manifest.path(),
            |cargo_path| {
                format!(
                    "{n} possibly unused dependenc{} in {cargo_path}",
                    if n == 1 { "y" } else { "ies" },
                )
            },
        );
        for entry in &unused {
            builder = builder.help(format!(
                "[{}] {}",
                entry.section.as_str(),
                entry.original_name
            ));
            if let Some(s) = build_delete_suggestion(manifest, entry) {
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

#[allow(clippy::too_many_arguments)]
pub(super) fn find_unused_deps(
    deps: BTreeMap<String, Vec<DeclaredDep>>,
    usage: &DepUsage,
    model: &SemanticModel,
    owner: &str,
    syntactic: &HashSet<String>,
    root_manifest: &Manifest,
    manifest: &Manifest,
) -> Vec<DeclaredDep> {
    deps.into_iter()
        .filter(|(normalized, _)| {
            // Syntactic: doc fences + feature plumbing, with the legacy
            // backend's separator-stripped fallback applied to the manifest
            // side only (see legacy.rs for the rationale).
            !syntactic.contains(normalized)
                && !syntactic.contains(&wl_lint_api::util::separator_stripped(normalized))
        })
        .flat_map(|(_, entries)| entries)
        .filter(|entry| {
            // Judgement gates: a dep the compiled configs can't observe is
            // skipped, never flagged.
            if entry.target_gated {
                // Platform-gated: only compiles when its cfg matches the
                // build host — a foreign platform's dep is invisible here.
                return false;
            }
            match entry.section {
                // Build scripts aren't lint-passed — no fragment.
                DepSection::BuildDependencies => return false,
                // Dev deps are judgeable only when a dev target compiled.
                DepSection::DevDependencies if !usage.dev_deps_judged() => return false,
                _ => {}
            }
            // Semantic: seed the closure with the RESOLVED package — a
            // `package = "…"` rename points the declared key at a different
            // package, and both the reference edges and the resolve graph
            // speak package/lib-target names, never the local alias.
            let pkg = resolve_package_name(root_manifest, manifest, entry).replace('-', "_");
            !usage.dep_used(model.meta(), owner, &pkg)
        })
        .collect()
}

/// The dep-line deletion suggestion, over the fast tier's `Manifest`
/// (locator-driven byte spans; swallows the trailing (CR)LF so the whole
/// line disappears).
pub(super) fn build_delete_suggestion(
    manifest: &Manifest,
    entry: &DeclaredDep,
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
    })
}

/// The package name a declared dep resolves to (a `package = "…"` rename,
/// local or workspace-inherited, wins over the dep key).
pub(super) fn resolve_package_name(
    root_manifest: &Manifest,
    manifest: &Manifest,
    entry: &DeclaredDep,
) -> String {
    if let Some(pkg) = manifest.dep_package_name(entry.section, &entry.original_name) {
        return pkg;
    }
    if manifest.dep_uses_workspace(entry.section, &entry.original_name)
        && let Some(pkg) =
            root_manifest.dep_package_name(DepSection::WorkspaceDependencies, &entry.original_name)
    {
        return pkg;
    }
    entry.original_name.clone()
}

pub(super) fn collect_deps(
    manifest: &Manifest,
    ignore: &[String],
) -> BTreeMap<String, Vec<DeclaredDep>> {
    let mut deps: BTreeMap<String, Vec<DeclaredDep>> = BTreeMap::new();
    for dep in manifest.declared_deps() {
        if ignore.iter().any(|i| i == &dep.original_name) {
            continue;
        }
        let entries = deps.entry(dep.normalized_name.clone()).or_default();
        // The same dep can be declared under several `[target.<cfg>.…]` tables
        // in one section; collapse those so it's reported once. Distinct
        // *sections* stay separate entries.
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
