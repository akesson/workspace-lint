//! The post-collection anchor→crate passes: the lint-level cascade
//! ([`apply_lint_levels`]) and the silence-hint form selection
//! ([`mark_marker_availability`]). Both map each diagnostic to the workspace
//! member owning its silence anchor via the same [`CrateDir`] match table —
//! anchors arrive with mixed path bases (workspace-relative and absolute),
//! so every member carries both forms.

use crate::config;
use wl_diagnostic::Diagnostic;
use wl_engine::fast::FastModel;
use wl_lint_api::LintId;

/// Apply the lint-level cascade to the collected diagnostics: **drop** any
/// whose effective level is `allow`, and rewrite the rest to their effective
/// level. The level resolves through the per-crate tier first — each
/// diagnostic is mapped to its owning workspace member (by its silence
/// anchor's path), then leveled via [`config::Config::effective_level`]
/// (per-crate override → per-crate default → global override → global default
/// → built-in `warn`). Diagnostics whose level the lint chose itself
/// (`level_is_explicit`, e.g. an `architecture` rule's `severity`) are left
/// untouched, so a blanket `[lints] <lint> = …` can't silently clobber a
/// deliberate per-rule severity.
pub(crate) fn apply_lint_levels(
    config: &config::Config,
    fast: Option<&FastModel>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let crate_dirs = crate_dirs(fast);
    diagnostics.retain_mut(|d| {
        if d.level_is_explicit {
            return true;
        }
        let Some(id) = LintId::from_short(d.lint_short()) else {
            // A diagnostic carrying an unknown lint id shouldn't happen (they
            // all come from `LintId::*.id()`); keep it rather than drop.
            return true;
        };
        let krate = owning_crate(&crate_dirs, &d.silence_anchor);
        match config.effective_level(id, krate).to_diagnostic_level() {
            None => false, // `allow` → drop before render & exit-code tally
            Some(level) => {
                d.level = level;
                true
            }
        }
    });
}

/// A workspace member's manifest-dir match candidates. Each member carries
/// **both** its workspace-relative and absolute manifest dir, because
/// diagnostics anchor with mixed path bases: `unused-deps` / `file-size` /
/// `crate-size` emit workspace-relative paths, while resolver-span lints
/// (`unused-pub`) emit absolute ones. Matching either form maps any diagnostic
/// to its crate. `depth` is the relative component count, used to match the
/// most specific crate first for nested layouts.
struct CrateDir {
    forms: Vec<std::path::PathBuf>,
    name: String,
    depth: usize,
}

/// Build the per-crate match table. Empty when no [`FastModel`] was loaded —
/// then every diagnostic resolves to the global level.
fn crate_dirs(fast: Option<&FastModel>) -> Vec<CrateDir> {
    let Some(fm) = fast else {
        return Vec::new();
    };
    let mut dirs: Vec<CrateDir> = fm
        .members()
        .iter()
        .map(|c| {
            let rel = fm.crate_relative_path(&c.manifest_dir);
            let depth = rel.components().count();
            CrateDir {
                forms: vec![rel, c.manifest_dir.clone()],
                name: c.name.clone(),
                depth,
            }
        })
        .collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.depth));
    dirs
}

/// The Cargo name of the workspace member that owns `anchor`, found by matching
/// the anchor's path (in either base) against each member's manifest-dir forms.
/// `None` for a workspace-level anchor or a path outside every member. An empty
/// (root) relative form is skipped so it can't match every path.
fn owning_crate<'a>(
    crate_dirs: &'a [CrateDir],
    anchor: &wl_diagnostic::SilenceAnchor,
) -> Option<&'a str> {
    let file = anchor.file()?;
    crate_dirs
        .iter()
        .find(|cd| {
            cd.forms
                .iter()
                .any(|f| !f.as_os_str().is_empty() && file.starts_with(f))
        })
        .map(|cd| cd.name.as_str())
}

/// Set [`Diagnostic::marker_available`] where the crate owning the anchor
/// declares the `workspace-lint-marker` dependency: the silence hint offers
/// the `workspace_lint::expect!` macro only where it actually compiles, and
/// the dependency-free `// workspace-lint: expect(…)` comment everywhere
/// else (pasting a hint must never be a compile error — the 2026-07-10
/// validation's Issue 11). Same anchor→crate match table as
/// [`apply_lint_levels`].
pub(crate) fn mark_marker_availability(fast: Option<&FastModel>, diagnostics: &mut [Diagnostic]) {
    let Some(fm) = fast else { return };
    let with_marker: std::collections::BTreeSet<&str> = fm
        .members()
        .iter()
        .filter(|c| {
            c.manifest()
                .declared_deps()
                .any(|d| d.original_name == "workspace-lint-marker")
        })
        .map(|c| c.name.as_str())
        .collect();
    if with_marker.is_empty() {
        return;
    }
    let crate_dirs = crate_dirs(fast);
    for d in diagnostics.iter_mut() {
        if let Some(name) = owning_crate(&crate_dirs, &d.silence_anchor)
            && with_marker.contains(name)
        {
            d.marker_available = true;
        }
    }
}
