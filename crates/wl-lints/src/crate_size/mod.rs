use std::path::{Path, PathBuf};
use wl_engine::fast::{FastModel, SourceMeasure, shipped_source};

use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::at_crate;
use wl_lint_api::config::{GlobPattern, glob_set};
use wl_lint_api::util::rule_glob_note;
use wl_lint_api::{LintContext, LintId, LintImpl, Requirements};

pub mod config;
#[cfg(test)]
mod tests;

pub use config::{CrateSizeConfig, CrateSizeRule};

pub struct CrateSize {
    config: CrateSizeConfig,
}

impl CrateSize {
    pub fn new(config: CrateSizeConfig) -> Self {
        Self { config }
    }

    pub fn from_cli(glob: String, max_code_lines: usize, include: Vec<String>) -> Self {
        Self::new(CrateSizeConfig {
            rules: vec![CrateSizeRule {
                glob: GlobPattern::from_cli(&glob),
                max_code_lines,
                include: if include.is_empty() {
                    None
                } else {
                    Some(include.iter().map(|p| GlobPattern::from_cli(p)).collect())
                },
            }],
        })
    }
}

impl LintImpl for CrateSize {
    const ID: LintId = LintId::CrateSize;
    const DOC: &'static str = include_str!("DOC.md");
    const REQUIRES: Requirements = Requirements {
        needs_fast: true,
        needs_semantic: false,
    };

    fn run(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        check(&self.config, cx.fast_model(Self::ID))
    }
}

pub(crate) fn check(config: &CrateSizeConfig, fast: &FastModel) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let measure = fast.source_measure();

    for rule in &config.rules {
        let matcher = rule.glob.compiled().compile_matcher();

        // A crate-size budget is about *shipped code*, not committed data and
        // not tests: JSON oracle snapshots and TOML fixture manifests under a
        // crate dir, the `tests/` / `benches/` / `examples/` dev-target trees,
        // and in-file `#[cfg(test)]` items all sit outside the budget (see
        // `shipped_source`). So only Rust source (`*.rs`) is counted by default.
        // Override per rule with `include` to count other file types or to
        // narrow to specific Rust files. Patterns match the file name (not the
        // full path).
        let rust_only;
        let include_patterns = match &rule.include {
            Some(patterns) => patterns.as_slice(),
            None => {
                rust_only = [GlobPattern::new("*.rs").expect("`*.rs` is a valid glob")];
                &rust_only
            }
        };
        let include_set = glob_set(include_patterns).unwrap_or_default();

        // Every workspace member whose workspace-relative manifest directory
        // matches the rule's glob (`members_matching`): the lint can only
        // target real cargo workspace members (cargo's `members`/`exclude`/
        // glob semantics are honored), and the resulting anchor path is
        // workspace-relative for free — `# workspace-lint: allow(crate-size)`
        // in a member Cargo.toml matches.
        let crate_totals: Vec<(String, usize)> = fast
            .members_matching(|key| matcher.is_match(key))
            .into_iter()
            .map(|(rel, krate)| {
                (
                    rel,
                    count_crate_code(measure, &krate.manifest_dir, &include_set),
                )
            })
            .collect();

        diagnostics.extend(find_crate_violations(rule, &crate_totals));
    }

    diagnostics
}

/// Sum the `*.rs` (or `include`-filtered) *shipped* code lines under a member's
/// on-disk directory — a projection over the FastModel's shared measurement
/// sweep (one tokei walk per run, however many rules and members). `include`
/// patterns match the file *name* only (not the path). Test code is excluded:
/// dev-target dirs wholesale, and in-file test items per `shipped_source`.
/// Non-Rust includes (e.g. `*.json`) keep tokei's host count — syn can't parse
/// them, they carry no test items to remove, and (unlike file-size's regime)
/// embedded doc-fence code is not billed.
fn count_crate_code(
    measure: &SourceMeasure,
    member_dir: &Path,
    include_set: &globset::GlobSet,
) -> usize {
    let mut rust_files: Vec<PathBuf> = Vec::new();
    let mut other_code = 0;
    for f in measure.files() {
        if !f.abs.starts_with(member_dir) {
            continue;
        }
        let name = f.abs.file_name().unwrap_or_default();
        if !include_set.is_match(Path::new(name)) {
            continue;
        }
        if shipped_source::in_dev_target_dir(member_dir, &f.abs) {
            continue;
        }
        if f.is_rust() {
            rust_files.push(f.abs.clone());
        } else {
            other_code += f.code;
        }
    }
    other_code + shipped_source::count_rust_shipped(&rust_files)
}

/// Pure projection: emit a diagnostic for each `(crate_relative_dir, total)`
/// over the rule's `max_code_lines` (strict `>`). Separated from the discovery
/// + tokei walk so the threshold + message logic is unit-testable directly.
fn find_crate_violations(
    rule: &CrateSizeRule,
    crate_totals: &[(String, usize)],
) -> Vec<Diagnostic> {
    let lint_id = LintId::CrateSize.id();
    crate_totals
        .iter()
        .filter(|(_, total)| *total > rule.max_code_lines)
        .map(|(rel, total)| {
            at_crate(
                lint_id,
                format!("crate exceeds {} code lines ({total})", rule.max_code_lines),
                rel.clone(),
            )
            .help("split the crate into smaller, more focused crates")
            // Rule attribution is identical for every crate the rule
            // matches — once per run is enough.
            .note_once(rule_glob_note(LintId::CrateSize, &rule.glob))
            .build()
        })
        .collect()
}
