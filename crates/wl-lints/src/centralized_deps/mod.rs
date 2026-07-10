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
    let root = fast.root_manifest();
    // Existing `[workspace.dependencies]` entries: name → effective
    // `default-features` (absent key = cargo's default, true). The flag is
    // resolution-relevant — cargo ignores a member's `default-features =
    // false` when the workspace entry doesn't declare it — so both the
    // rewrite gate and the insertion agreement key on it.
    let workspace_deps: std::collections::BTreeMap<String, bool> = root
        .deps(DepSection::WorkspaceDependencies)
        .map(|(name, item)| {
            let df = DepEntry::new(item).default_features().unwrap_or(true);
            (name.to_string(), df)
        })
        .collect();

    // Pass 1 — collect per-crate findings, plus every dep that could seed a
    // missing `[workspace.dependencies]` key. Finalizing waits for the
    // workspace view: the insertion is only auto-applicable when every member
    // wanting the key agrees on the version AND the default-features flag.
    struct PendingSuggestion {
        suggestion: Suggestion,
        /// `Some((name, version, default_features))` when this rewrite's dep
        /// is missing from the workspace table and insertable — upgraded to
        /// `MachineApplicable` iff the workspace-wide declarations agree.
        missing: Option<(String, String, bool)>,
    }
    struct PendingCrate<'a> {
        krate: &'a wl_engine::fast::CrateInfo,
        errors: Vec<String>,
        suggestions: Vec<PendingSuggestion>,
    }
    // Dep name → agreed (version, default-features) (`None` marks a
    // conflict — no auto-insert: seeding either variant would silently
    // change some member's feature resolution).
    let mut insertable: std::collections::BTreeMap<String, Option<(String, bool)>> =
        Default::default();
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
                if let Some(issue) = check_dep(name, item, section, &workspace_deps) {
                    errors.push(issue.message);
                    let applicable_now =
                        workspace_deps.contains_key(name) && !issue.rewrite_blocked;
                    let missing = issue.insertable.map(|(v, df)| (name.to_string(), v, df));
                    if let Some((name, version, df)) = &missing {
                        insertable
                            .entry(name.clone())
                            .and_modify(|agreed| {
                                if agreed.as_ref() != Some(&(version.clone(), *df)) {
                                    *agreed = None; // members disagree — no auto-insert
                                }
                            })
                            .or_insert_with(|| Some((version.clone(), *df)));
                    }
                    if let Some(suggestion) =
                        build_rewrite_suggestion(manifest, section, name, applicable_now)
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

    // Pass 2 — finalize. An agreed missing dep upgrades its member rewrites
    // to `MachineApplicable` (the pair applies atomically in one `--fix`
    // run), and the workspace half is emitted exactly once per key:
    //  - table EXISTS → one per-dep insertion at the sorted position, on the
    //    first diagnostic that wants it (a duplicate would trip the fix
    //    applier's overlap refusal);
    //  - table ABSENT → ONE table-creating insertion carrying EVERY agreed
    //    entry, on the first diagnostic that wants any. Per-dep insertions
    //    each carried their own `[workspace.dependencies]` header here,
    //    stacking N duplicate sections in one `--fix` pass (cargo rejects
    //    the manifest — broke ripgrep in the 2026-07-10 validation).
    let table_exists = root.has_workspace_deps_table();
    let mut to_create: std::collections::BTreeMap<String, (String, bool)> = Default::default();
    if !table_exists {
        for p in &pending {
            for s in &p.suggestions {
                if let Some((name, version, df)) = &s.missing
                    && insertable.get(name).is_some_and(Option::is_some)
                {
                    to_create.insert(name.clone(), (version.clone(), *df));
                }
            }
        }
    }
    let mut inserted: BTreeSet<String> = BTreeSet::new();
    let mut table_created = false;
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
            if let Some((name, version, df)) = s.missing
                && insertable.get(&name).is_some_and(Option::is_some)
            {
                s.suggestion.applicability = Applicability::MachineApplicable;
                if table_exists {
                    if inserted.insert(name.clone())
                        && let Some(sug) = workspace_insertion(fast, &name, &version, df)
                    {
                        builder = builder.suggestion(sug);
                    }
                } else if !table_created {
                    table_created = true;
                    builder = builder.suggestion(workspace_table_seed(fast, &to_create));
                }
            }
            builder = builder.suggestion(s.suggestion);
        }
        diagnostics.push(builder.build());
    }

    diagnostics
}

/// The root-manifest half of the two-file fix when `[workspace.dependencies]`
/// already exists: insert one entry at the sorted position. `None` when the
/// table is absent (that case is [`workspace_table_seed`], once per run).
fn workspace_insertion(
    fast: &FastModel,
    name: &str,
    version: &str,
    default_features: bool,
) -> Option<Suggestion> {
    let root = fast.root_manifest();
    let (line, pos, text) = root.workspace_dep_insertion(name, version, default_features)?;
    Some(Suggestion {
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
    })
}

/// The root-manifest half when the table is ABSENT: one end-of-file insertion
/// creating `[workspace.dependencies]` with every agreed entry, sorted.
fn workspace_table_seed(
    fast: &FastModel,
    entries: &std::collections::BTreeMap<String, (String, bool)>,
) -> Suggestion {
    let root = fast.root_manifest();
    let refs: Vec<(&str, &str, bool)> = entries
        .iter()
        .map(|(n, (v, df))| (n.as_str(), v.as_str(), *df))
        .collect();
    let (line, pos, text) = root.workspace_table_creation(&refs);
    let names = entries.keys().cloned().collect::<Vec<_>>().join("`, `");
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
        message: format!("create [workspace.dependencies] with `{names}`"),
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

/// One non-centralized dep: the help-line message, the `(version,
/// default_features)` pair that can seed a missing `[workspace.dependencies]`
/// key (`None` when the entry isn't auto-insertable: already centralized,
/// renamed via `package`, or git/path), and whether the member rewrite must
/// stay withheld (`rewrite_blocked`: the member's `default-features` differs
/// from the existing workspace entry's — rewriting would silently change the
/// resolved feature set, since cargo honors only the workspace side).
struct DepIssue {
    message: String,
    insertable: Option<(String, bool)>,
    rewrite_blocked: bool,
}

impl DepIssue {
    fn plain(message: String) -> Self {
        Self {
            message,
            insertable: None,
            rewrite_blocked: false,
        }
    }
}

fn check_dep(
    name: &str,
    item: &Item,
    section: DepSection,
    workspace_deps: &std::collections::BTreeMap<String, bool>,
) -> Option<DepIssue> {
    let section_str = section.as_str();
    let entry = DepEntry::new(item);

    if entry.uses_workspace() || entry.is_path() {
        return None; // already centralized / a local path dep
    }

    if let Some(version) = entry.version() {
        let member_df = entry.default_features().unwrap_or(true);
        if let Some(ws_df) = workspace_deps.get(name) {
            if member_df != *ws_df {
                // `{ workspace = true }` would flip the dep to the workspace
                // entry's default-features: cargo ignores a member-side
                // `default-features = false` the workspace entry lacks (and
                // the inherited `false` silently strips features in the
                // other direction). Not auto-fixable until they agree.
                return Some(DepIssue {
                    message: format!(
                        "[{section_str}] {name}: has own version \"{version}\" and its \
                         `default-features` ({member_df}) differs from the \
                         [workspace.dependencies] entry ({ws_df}) — align the two \
                         declarations first, then use {{ workspace = true }}"
                    ),
                    insertable: None,
                    rewrite_blocked: true,
                });
            }
            return Some(DepIssue::plain(format!(
                "[{section_str}] {name}: has own version \"{version}\" — use {{ workspace = true }} instead"
            )));
        }
        // A renamed dep (`{ package = "other", … }`) needs the rename in the
        // workspace entry too — not the simple `name = "version"` insert, so
        // it stays a manual (MaybeIncorrect) case.
        let insertable = (!entry.is_renamed()).then(|| (version.to_string(), member_df));
        return Some(DepIssue {
            message: format!(
                "[{section_str}] {name}: version \"{version}\" not in [workspace.dependencies] — add it there and use {{ workspace = true }}"
            ),
            insertable,
            rewrite_blocked: false,
        });
    }

    if entry.is_git() {
        if workspace_deps.contains_key(name) {
            return Some(DepIssue::plain(format!(
                "[{section_str}] {name}: has own git source — use {{ workspace = true }} instead"
            )));
        }
        return None;
    }

    None
}
