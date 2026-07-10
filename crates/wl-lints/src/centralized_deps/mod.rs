use std::collections::BTreeSet;
use wl_engine::fast::toml_edit::Item;
use wl_engine::fast::{DepEntry, DepSection, FastModel, Manifest};

use wl_diagnostic::{Applicability, Diagnostic, Span, Suggestion};
use wl_lint_api::{LintContext, LintId, LintImpl, Requirements};

#[cfg(test)]
mod tests;

pub struct CentralizedDeps;

impl CentralizedDeps {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CentralizedDeps {
    fn default() -> Self {
        Self::new()
    }
}

impl LintImpl for CentralizedDeps {
    const ID: LintId = LintId::CentralizedDeps;
    const DOC: &'static str = include_str!("DOC.md");
    const REQUIRES: Requirements = Requirements {
        needs_fast: true,
        needs_semantic: false,
    };

    fn run(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        check(cx.fast_model(Self::ID))
    }
}

pub(crate) fn check(fast: &FastModel) -> Vec<Diagnostic> {
    let lint_id = LintId::CentralizedDeps.id();
    let workspace_dep_names = fast.root_manifest().workspace_dep_names();

    // Pass 1 — collect per-crate findings, plus every dep that could seed a
    // missing `[workspace.dependencies]` key. Finalizing waits for the
    // workspace view: the insertion is only auto-applicable when every member
    // wanting the key agrees on the version.
    struct PendingSuggestion {
        suggestion: Suggestion,
        /// `Some(version)` when this rewrite's dep is missing from the
        /// workspace table and insertable — upgraded to `MachineApplicable`
        /// iff the workspace-wide versions agree.
        missing: Option<(String, String)>,
    }
    struct PendingCrate<'a> {
        krate: &'a wl_engine::fast::CrateInfo,
        errors: Vec<String>,
        suggestions: Vec<PendingSuggestion>,
    }
    // Dep name → agreed version (`None` marks a conflict — no auto-insert).
    let mut insertable: std::collections::BTreeMap<String, Option<String>> = Default::default();
    let mut pending: Vec<PendingCrate> = Vec::new();

    for krate in fast.members() {
        let manifest = krate.manifest();
        let mut errors: Vec<String> = Vec::new();
        let mut suggestions: Vec<PendingSuggestion> = Vec::new();

        // A dep can be declared under several `[target.<cfg>.…]` tables in the
        // same section; check each (section, name) once so it isn't reported
        // multiple times.
        let mut seen: BTreeSet<(&str, &str)> = BTreeSet::new();
        for section in DepSection::member_sections() {
            for (name, item) in manifest.deps(section) {
                if !seen.insert((section.as_str(), name)) {
                    continue;
                }
                if let Some(issue) = check_dep(name, item, section, &workspace_dep_names) {
                    errors.push(issue.message);
                    let key_in_workspace = workspace_dep_names.contains(name);
                    let missing = issue.insertable_version.map(|v| (name.to_string(), v));
                    if let Some((name, version)) = &missing {
                        insertable
                            .entry(name.clone())
                            .and_modify(|agreed| {
                                if agreed.as_deref() != Some(version) {
                                    *agreed = None; // members disagree — no auto-insert
                                }
                            })
                            .or_insert_with(|| Some(version.clone()));
                    }
                    if let Some(suggestion) =
                        build_rewrite_suggestion(manifest, section, name, key_in_workspace)
                    {
                        suggestions.push(PendingSuggestion {
                            suggestion,
                            missing,
                        });
                    }
                }
            }
        }
        if !errors.is_empty() {
            pending.push(PendingCrate {
                krate,
                errors,
                suggestions,
            });
        }
    }

    // Pass 2 — finalize: an agreed missing dep gets ONE root-manifest
    // insertion (on the first diagnostic that wants it — a duplicate would
    // trip the fix applier's overlap refusal) and its member rewrites become
    // `MachineApplicable`: the pair applies atomically in one `--fix` run.
    let mut inserted: BTreeSet<String> = BTreeSet::new();
    let mut diagnostics = Vec::new();
    for p in pending {
        let n = p.errors.len();
        let manifest = p.krate.manifest();
        let mut builder = wl_lint_api::util::at_crate_manifest(
            lint_id,
            fast,
            &p.krate.manifest_dir,
            manifest.path(),
            |cargo_path| {
                format!(
                    "{n} dependenc{} in {cargo_path} should use `workspace = true`",
                    if n == 1 { "y" } else { "ies" },
                )
            },
        );
        for err in p.errors {
            builder = builder.help(err);
        }
        for mut s in p.suggestions {
            if let Some((name, version)) = s.missing
                && insertable.get(&name).is_some_and(Option::is_some)
            {
                s.suggestion.applicability = Applicability::MachineApplicable;
                if inserted.insert(name.clone()) {
                    builder = builder.suggestion(workspace_insertion(fast, &name, &version));
                }
            }
            builder = builder.suggestion(s.suggestion);
        }
        diagnostics.push(builder.build());
    }

    diagnostics
}

/// The root-manifest half of the two-file fix: insert `<name> = "<version>"`
/// into `[workspace.dependencies]` at the sorted position (creating the table
/// at end-of-file when absent).
fn workspace_insertion(fast: &FastModel, name: &str, version: &str) -> Suggestion {
    let root = fast.root_manifest();
    let (line, pos, text) = root.workspace_dep_insertion(name, version);
    Suggestion {
        span: Span {
            file: root.path().to_path_buf(),
            line_start: line,
            line_end: line,
            col_start: 1,
            col_end: 1,
            byte_start: pos,
            byte_end: pos,
        },
        message: format!("add `{name}` to [workspace.dependencies]"),
        replacement: text,
        applicability: Applicability::MachineApplicable,
        original: None,
    }
}

/// Build a byte-range replacement that turns `<name> = "..."` or
/// `<name> = { ... }` into `<name> = { workspace = true[, preserved keys] }`.
/// Returns `None` for entries this lint can't locate / rewrite.
///
/// `key_in_workspace` controls applicability. `<name> = { workspace = true }`
/// is only valid when `<name>` already exists as a key in
/// `[workspace.dependencies]` — otherwise cargo rejects the manifest ("no
/// dependency named `<name>` in workspace"). So:
///
///  - dep key IS centralized (the "has own version" case) → `MachineApplicable`;
///    `--fix` applies it. Covered by `fix__centralized_deps`.
///  - dep key is NOT centralized (the "add it there and use { workspace = true }"
///    case) → `MaybeIncorrect`: the suggestion is shown as a preview of the end
///    state, but `--fix` skips it because applying it alone (without the user
///    also adding the dep to `[workspace.dependencies]`) would break
///    `cargo metadata`. Renamed deps (`{ package = "..." }`) usually land here
///    because their local key rarely matches a workspace key.
fn build_rewrite_suggestion(
    manifest: &Manifest,
    section: DepSection,
    dep_name: &str,
    key_in_workspace: bool,
) -> Option<Suggestion> {
    let location = manifest.locate_dep(section, dep_name)?;
    let replacement = manifest.format_workspace_dep(section, dep_name)?;
    let original = &manifest.raw()[location.byte_start as usize..location.byte_end as usize];
    if replacement == original {
        return None;
    }
    let applicability = if key_in_workspace {
        Applicability::MachineApplicable
    } else {
        Applicability::MaybeIncorrect
    };
    Some(Suggestion {
        span: Span {
            file: manifest.path().to_path_buf(),
            line_start: location.line,
            line_end: location.line,
            col_start: 1,
            col_end: 1,
            byte_start: location.byte_start,
            byte_end: location.byte_end,
        },
        message: format!("use {{ workspace = true }} for `{dep_name}`"),
        replacement,
        applicability,
        original: Some(original.to_string()),
    })
}

/// One non-centralized dep: the help-line message, plus the version that can
/// seed a missing `[workspace.dependencies]` key (`None` when the entry isn't
/// auto-insertable: already centralized, renamed via `package`, or git/path).
struct DepIssue {
    message: String,
    insertable_version: Option<String>,
}

impl DepIssue {
    fn plain(message: String) -> Self {
        Self {
            message,
            insertable_version: None,
        }
    }
}

fn check_dep(
    name: &str,
    item: &Item,
    section: DepSection,
    workspace_deps: &BTreeSet<String>,
) -> Option<DepIssue> {
    let section_str = section.as_str();
    let entry = DepEntry::new(item);

    if entry.uses_workspace() || entry.is_path() {
        return None; // already centralized / a local path dep
    }

    if let Some(version) = entry.version() {
        if workspace_deps.contains(name) {
            return Some(DepIssue::plain(format!(
                "[{section_str}] {name}: has own version \"{version}\" — use {{ workspace = true }} instead"
            )));
        }
        // A renamed dep (`{ package = "other", … }`) needs the rename in the
        // workspace entry too — not the simple `name = "version"` insert, so
        // it stays a manual (MaybeIncorrect) case.
        let insertable_version = (!entry.is_renamed()).then(|| version.to_string());
        return Some(DepIssue {
            message: format!(
                "[{section_str}] {name}: version \"{version}\" not in [workspace.dependencies] — add it there and use {{ workspace = true }}"
            ),
            insertable_version,
        });
    }

    if entry.is_git() {
        if workspace_deps.contains(name) {
            return Some(DepIssue::plain(format!(
                "[{section_str}] {name}: has own git source — use {{ workspace = true }} instead"
            )));
        }
        return None;
    }

    None
}
