//! The rustc backend of `unused-deps`: the engine's [`DepsVerdict`] — the
//! single judgement (usage fold, facade closures, and every exemption gate:
//! build/dev-without-test/optional/target-gated/not-compiled — see
//! `wl_engine::semantic::deps`) — joined back onto the manifest's declared
//! entries. The lint layers only what the manifest side owns:
//!
//! - the `ignore` config (filtered before the join, in [`collect_deps`]);
//! - the syntactic used-credits the compiled IR structurally can't carry:
//!   doc-fence refs (`CrateInfo::doctest_dep_refs` — doc-test code is a
//!   separate compilation unit) and feature plumbing
//!   (`Manifest::feature_dep_refs`), with the separator-stripped fallback;
//! - rename resolution back to manifest keys ([`resolve_package_name`] — the
//!   verdict speaks package names, the manifest its local aliases);
//! - the byte-exact deletion suggestions over the `Manifest` locators, and
//!   the diagnostic shaping.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use wl_engine::fast::{CrateInfo, DeclaredDep, DepSection, FastModel, Manifest};
use wl_engine::semantic::{CrateDeps, DepKind, NotJudged, SemanticModel};

use super::UnusedDepsConfig;
use wl_diagnostic::{Diagnostic, Suggestion};
use wl_lint_api::LintId;
use wl_lint_api::config::PerCrate;

pub(super) fn check(
    per_crate: &PerCrate<UnusedDepsConfig>,
    fast: &FastModel,
    model: &SemanticModel,
) -> Vec<Diagnostic> {
    let lint_id = LintId::UnusedDeps.id();
    let verdict = model.deps_verdict();
    let by_crate: BTreeMap<&str, &CrateDeps> = verdict
        .crates
        .iter()
        .map(|c| (c.krate.as_str(), c))
        .collect();
    let mut diagnostics = Vec::new();

    for krate in fast.members() {
        // A per-crate `[crates.<name>.unused-deps]` wholesale-replaces the
        // global params for this crate; otherwise the global config applies.
        let config = per_crate.for_crate(&krate.name);
        let manifest = krate.manifest();
        let deps = collect_deps(manifest, &config.ignore);
        if deps.is_empty() {
            continue;
        }
        let Some(crate_verdict) = by_crate.get(krate.code_name().as_str()) else {
            continue; // no declared deps per cargo metadata — nothing judged
        };

        // The syntactic half of "referenced": doc-fence refs + feature
        // plumbing — usage the compiled IR structurally can't carry.
        let mut syntactic: HashSet<String> = krate
            .doctest_dep_refs()
            .iter()
            .map(|s| s.to_string())
            .collect();
        syntactic.extend(manifest.feature_dep_refs());

        let (unused, not_compiled) = partition_by_verdict(
            deps,
            crate_verdict,
            &syntactic,
            fast.root_manifest(),
            manifest,
        );

        // Zero fragments: no `[engine] config` compiled this member, so every
        // dep reads "unused" only for lack of compiler output. They are
        // unjudgeable, not unused — report the coverage gap instead of
        // false-positive findings. (unused-pub is immune structurally: an
        // uncompiled crate contributes no defs, hence no candidates.)
        if !not_compiled.is_empty() {
            diagnostics.push(build_not_compiled_note(
                fast,
                krate,
                manifest,
                &not_compiled,
            ));
            continue;
        }
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
            if let Some((s, withheld)) = build_delete_suggestion(manifest, entry) {
                builder = builder.suggestion(s);
                if let Some(reason) = withheld {
                    // Every entry shares this manifest file, so the reason
                    // collapses to one note on the rollup diagnostic.
                    builder = builder.note_once(reason);
                }
            }
        }
        diagnostics.push(
            builder
                .note_once("build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives")
                .note_once("verify by removing the dep and running `cargo build --all-targets`")
                .note_once("if the build breaks, add the dep to [unused-deps] ignore in your config")
                .build(),
        );
    }

    diagnostics
}

/// The coverage note for a member no `[engine] config` compiled: its declared
/// deps produced zero fragments, so none can be judged. Surfaced at a pinned
/// `warn` (via `level_explicit`) — a `unused-deps = "deny"` config must not turn
/// a *coverage* shortfall into a build failure (cf. duplicate-code's live-out
/// downgrade). No `Suggestion`: nothing here is machine-fixable.
fn build_not_compiled_note(
    fast: &FastModel,
    krate: &CrateInfo,
    manifest: &Manifest,
    unused: &[DeclaredDep],
) -> Diagnostic {
    let lint_id = LintId::UnusedDeps.id();
    let name = krate.name.clone();
    let n = unused.len();
    let mut builder = wl_lint_api::util::at_crate_manifest(
        lint_id,
        fast,
        &krate.manifest_dir,
        manifest.path(),
        |cargo_path| {
            format!(
                "{n} dependenc{} of `{name}` could not be checked in {cargo_path}",
                if n == 1 { "y" } else { "ies" },
            )
        },
    )
    .level_explicit(wl_diagnostic::Level::Warn);
    for entry in unused {
        builder = builder.help(format!(
            "[{}] {}",
            entry.section.as_str(),
            entry.original_name
        ));
    }
    builder
        .help(format!(
            "compile it under an [engine] config (e.g. \"cargo build --target <triple> -p {name}\"), or add these deps to [unused-deps] ignore"
        ))
        .note_once("this crate produced no compiler output under the current [engine] config matrix")
        .build()
}

/// Join the engine's per-crate verdict onto the manifest's declared entries,
/// subtracting the manifest-side used-evidence (syntactic credits; `ignore`
/// was already filtered in [`collect_deps`]). Returns the entries whose
/// verdict is unused, and those whose verdict is [`NotJudged::NotCompiled`]
/// (the coverage note) — mutually exclusive per crate by engine construction.
/// Manifest order is preserved (it drives help-line order and display names).
pub(super) fn partition_by_verdict(
    deps: BTreeMap<String, Vec<DeclaredDep>>,
    verdict: &CrateDeps,
    syntactic: &HashSet<String>,
    root_manifest: &Manifest,
    manifest: &Manifest,
) -> (Vec<DeclaredDep>, Vec<DeclaredDep>) {
    let unused_set: BTreeSet<(&str, DepKind)> = verdict
        .unused
        .iter()
        .map(|u| (u.name.as_str(), u.kind))
        .collect();
    let not_compiled_set: BTreeSet<(&str, DepKind)> = verdict
        .not_judged
        .iter()
        .filter(|n| n.reason == NotJudged::NotCompiled)
        .map(|n| (n.name.as_str(), n.kind))
        .collect();

    let mut unused = Vec::new();
    let mut not_compiled = Vec::new();
    for (normalized, entries) in deps {
        // Syntactic: doc fences + feature plumbing, with the separator-
        // stripped fallback applied to the manifest side only (`md_5`
        // collapses to `md5` to match a doc fence's `use md5::…`).
        if syntactic.contains(&normalized)
            || syntactic.contains(&wl_lint_api::util::separator_stripped(&normalized))
        {
            continue;
        }
        for entry in entries {
            // The verdict speaks (package code-name, kind); the manifest its
            // local alias — resolve a `package = "…"` rename (local or
            // workspace-inherited) before joining.
            let pkg = resolve_package_name(root_manifest, manifest, &entry).replace('-', "_");
            let key = (pkg.as_str(), kind_of(entry.section));
            if unused_set.contains(&key) {
                unused.push(entry);
            } else if not_compiled_set.contains(&key) {
                not_compiled.push(entry);
            }
            // Anything else is used, or engine-exempt (build / dev-without-
            // test-config / optional / target-gated) — nothing to report.
        }
    }
    (unused, not_compiled)
}

/// The engine [`DepKind`] a manifest section judges as. `declared_deps` only
/// yields the three member sections, never `WorkspaceDependencies`.
fn kind_of(section: DepSection) -> DepKind {
    match section {
        DepSection::DevDependencies => DepKind::Dev,
        DepSection::BuildDependencies => DepKind::Build,
        _ => DepKind::Normal,
    }
}

/// The dep-line deletion suggestion, over the fast tier's `Manifest`
/// (locator-driven byte spans; swallows the trailing (CR)LF so the whole
/// line disappears). Gated per-file like every deletion: `MachineApplicable`
/// only for a git-tracked-clean manifest, otherwise withheld with the reason
/// in the second element.
pub(super) fn build_delete_suggestion(
    manifest: &Manifest,
    entry: &DeclaredDep,
) -> Option<(Suggestion, Option<String>)> {
    let location = manifest
        .locate_dep(entry.section, &entry.original_name)
        .or_else(|| manifest.locate_dep_entry(entry.section, &entry.original_name))?;
    let end = wl_lint_api::surgery::lines::eat_trailing_newline(
        manifest.raw().as_bytes(),
        location.byte_end as usize,
    );
    let deleted = &manifest.raw()[location.byte_start as usize..location.byte_end as usize];
    let line_end = location.line + deleted.bytes().filter(|&b| b == b'\n').count() as u32;
    Some(wl_lint_api::surgery::deletion::gated_deletion_suggestion(
        manifest.path(),
        (location.byte_start as usize, end),
        (location.line, line_end),
        format!("remove unused dependency `{}`", entry.original_name),
        Some(deleted.to_string()),
        wl_lint_api::surgery::deletion::FixFlag::Fix,
    ))
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
