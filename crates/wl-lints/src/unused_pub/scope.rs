//! The ONE implementation of unused-pub's per-crate *scope* gates —
//! exclude-crates, allowlist, exclude-paths, target-dir, generated-file —
//! shared by the plain findings path (`ir::candidate_skipped_by_filters`) and
//! the cascade's scaffold/collateral findings. These gates guard
//! `--fix-auto-delete` deletions, so the three call sites drifting apart is a
//! correctness hazard, not a style one (before this module, the two cascade
//! copies silently lacked the generated-file check).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use globset::GlobSet;
use wl_engine::coverage::ShadowRegion;
use wl_engine::fast::{CrateInfo, FastModel};
use wl_engine::wl_ir;
use wl_lint_api::config::glob_set;

use super::config::UnusedPubConfig;

/// One crate's resolved finding-scope filters. Two deliberate asymmetries,
/// decided here rather than left to drift:
///
/// - **Kind filter: primary findings only** (`kind_filter: None` on the
///   cascade paths). `kinds = […]` selects which *findings the user wants
///   reported*; a scaffold/collateral deletion is the structural consequence
///   of deleting a kind-permitted target. Vetoing a collateral struct because
///   `kinds = ["fn"]` would strand a `dead_code`-tripping orphan, and vetoing
///   a scaffold would block its whole target on an invisible interaction.
/// - **Generated-file check: every path.** A generated file's items are
///   generator-owned — deleting them just gets overwritten. A generated
///   scaffold therefore blocks its target (via `gate_scaffolding`'s `None`
///   handling); a generated collateral simply never seeds.
pub(crate) struct FindingScope<'a> {
    /// Workspace root — joins the IR's workspace-relative span files into the
    /// absolute paths diagnostics anchor to.
    root: &'a Path,
    /// Cargo's target directory — everything under it is build-generated.
    target_directory: &'a Path,
    /// Workspace-relative paths of checked-in generated (`include!`d) files.
    generated: &'a HashSet<PathBuf>,
    /// The owning member's package name (hyphen form) — `exclude-crates`
    /// entries may use either it or the candidate's crate code.
    krate_name: &'a str,
    exclude_crates: &'a [String],
    allowlist: Option<GlobSet>,
    exclude_paths: Option<GlobSet>,
    /// `Some` only on the primary-findings path — see the type docs.
    kind_filter: Option<HashSet<&'static str>>,
}

impl<'a> FindingScope<'a> {
    pub(crate) fn new(
        config: &'a UnusedPubConfig,
        fast: &'a FastModel,
        krate: &'a CrateInfo,
        generated: &'a HashSet<PathBuf>,
        kind_filter: Option<HashSet<&'static str>>,
    ) -> Self {
        Self {
            root: fast.root(),
            target_directory: fast.target_directory(),
            generated,
            krate_name: &krate.name,
            exclude_crates: &config.exclude_crates,
            allowlist: glob_set(&config.allowlist),
            exclude_paths: glob_set(&config.exclude_paths),
            kind_filter,
        }
    }

    /// The IR's workspace-relative span file as the absolute path diagnostics
    /// anchor to (matching the syn backend's historical form).
    pub(crate) fn abs_file(&self, span: &wl_ir::Span) -> PathBuf {
        self.root.join(&span.file)
    }

    /// `exclude-crates`, matched against the member's package name AND the
    /// candidate's crate code — covering the hyphen/underscore duality and
    /// integration-test target names (whose crate code is the test file stem,
    /// not a member name).
    pub(crate) fn crate_excluded(&self, cand_crate_code: &str) -> bool {
        self.exclude_crates
            .iter()
            .any(|c| c == self.krate_name || c == cand_crate_code)
    }

    /// The per-item scope gate: kind filter (when armed), allowlist glob on
    /// the identity, target-dir prefix, generated-file set, and exclude-paths
    /// glob on the absolute file.
    pub(crate) fn skips(&self, id: &str, kind: &str, span: Option<&wl_ir::Span>) -> bool {
        if let Some(kf) = &self.kind_filter
            && !kf.contains(kind)
        {
            return true;
        }
        if let Some(al) = &self.allowlist
            && al.is_match(id)
        {
            return true;
        }
        if let Some(span) = span {
            let abs = self.abs_file(span);
            // Build-generated code (`OUT_DIR` content spliced via `include!`
            // lands under cargo's target dir): analyzed, but never an
            // author-editable finding surface.
            if abs.starts_with(self.target_directory) {
                return true;
            }
            // Same policy for checked-in generated files (`include!`d from
            // the source tree): the generator owns them, so a finding there
            // isn't actionable and a deletion would be overwritten.
            if self.generated.contains(Path::new(&span.file)) {
                return true;
            }
            if let Some(ex) = &self.exclude_paths
                && ex.is_match(abs.to_string_lossy().as_ref())
            {
                return true;
            }
        }
        false
    }
}

/// The shared clause of every cfg-shadow note — one wording for the
/// report-time flavor (`ir.rs`) and the deletion-veto flavor (`cascade.rs`).
fn shadow_clause(region: &ShadowRegion) -> String {
    format!(
        "mentioned under `cfg({})` ({}), which no declared `[engine]` config compiles",
        region.predicate, region.file,
    )
}

/// The report-time cfg-shadow note: the finding may be a false positive.
pub(crate) fn shadow_report_note(region: &ShadowRegion) -> String {
    format!(
        "possibly used: {} — add a matching cargo command to `[engine] configs` to judge \
         that code",
        shadow_clause(region)
    )
}

/// The deletion-veto cfg-shadow note: the cascade keeps the item.
pub(crate) fn shadow_veto_note(region: &ShadowRegion) -> String {
    format!(
        "{} — possibly used on a target the engine never saw; not deleting. Add a matching \
         command to `[engine] configs`, or remove manually",
        shadow_clause(region)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_lint_api::config::GlobPattern;

    fn span(file: &str) -> wl_ir::Span {
        wl_ir::Span {
            file: file.into(),
            lo: 0,
            hi: 1,
            line: 1,
            from_expansion: false,
        }
    }

    /// A scope built from raw parts (no FastModel needed).
    fn scope<'a>(
        generated: &'a HashSet<PathBuf>,
        exclude_crates: &'a [String],
        kind_filter: Option<HashSet<&'static str>>,
    ) -> FindingScope<'a> {
        FindingScope {
            root: Path::new("/ws"),
            target_directory: Path::new("/ws/target"),
            generated,
            krate_name: "my-crate",
            exclude_crates,
            allowlist: glob_set(&[GlobPattern::from_cli("allowed::**")]),
            exclude_paths: glob_set(&[GlobPattern::from_cli("**/skipme/**")]),
            kind_filter,
        }
    }

    #[test]
    fn crate_exclusion_matches_both_name_forms() {
        let generated = HashSet::new();
        let hyphen = vec!["my-crate".to_string()];
        assert!(scope(&generated, &hyphen, None).crate_excluded("my_crate"));
        let code = vec!["my_crate".to_string()];
        assert!(scope(&generated, &code, None).crate_excluded("my_crate"));
        let other = vec!["other".to_string()];
        assert!(!scope(&generated, &other, None).crate_excluded("my_crate"));
    }

    #[test]
    fn skips_generated_target_dir_allowlist_and_excluded_paths() {
        let generated: HashSet<PathBuf> = [PathBuf::from("src/gen.rs")].into();
        let none = Vec::new();
        let s = scope(&generated, &none, None);
        // Generated file (workspace-relative comparison).
        assert!(s.skips("c::item", "fn", Some(&span("src/gen.rs"))));
        // Under cargo's target dir.
        assert!(s.skips("c::item", "fn", Some(&span("target/out.rs"))));
        // Allowlisted identity.
        assert!(s.skips("allowed::thing", "fn", Some(&span("src/lib.rs"))));
        // Excluded path glob (matched on the absolute file).
        assert!(s.skips("c::item", "fn", Some(&span("src/skipme/x.rs"))));
        // In scope.
        assert!(!s.skips("c::item", "fn", Some(&span("src/lib.rs"))));
    }

    #[test]
    fn kind_filter_gates_only_when_armed() {
        let generated = HashSet::new();
        let none = Vec::new();
        let armed = scope(&generated, &none, Some(["fn"].into()));
        assert!(armed.skips("c::S", "struct", Some(&span("src/lib.rs"))));
        assert!(!armed.skips("c::f", "fn", Some(&span("src/lib.rs"))));
        // Cascade paths pass no filter: kind never vetoes a scaffold or a
        // collateral orphan (the documented asymmetry).
        let unarmed = scope(&generated, &none, None);
        assert!(!unarmed.skips("c::S", "struct", Some(&span("src/lib.rs"))));
    }
}
