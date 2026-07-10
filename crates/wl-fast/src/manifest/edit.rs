//! The *edit* side of [`Manifest`]: the byte-position + replacement-text
//! builders behind the centralized-deps `--fix`. Split out from the read
//! model (`super`) so the parsed-manifest view stays under the file-size
//! ceiling and the "read vs. rewrite" boundary the module doc names is a
//! structural one, not just a convention. Every fn here still operates on a
//! local scratch and returns a `String`/offset — the stored document is
//! never mutated.

use toml_edit::{Item, Value};

use super::{DepSection, Manifest};

impl Manifest {
    /// Byte position + text for inserting one dep into this (root) manifest's
    /// EXISTING `[workspace.dependencies]` at the alphabetically sorted
    /// position — the workspace half of the centralized-deps two-file
    /// auto-fix (the member half is [`Manifest::format_workspace_dep`]).
    /// Returns `None` when the table is absent: creating it is
    /// [`Manifest::workspace_table_creation`]'s job, once per manifest.
    /// Returns `(line, byte_pos, insert_text)`.
    pub fn workspace_dep_insertion(
        &self,
        name: &str,
        version: &str,
        default_features: bool,
    ) -> Option<(u32, u32, String)> {
        let entry = workspace_entry_text(name, version, default_features);
        let table = self.section_table(DepSection::WorkspaceDependencies)?;
        // The first existing key alphabetically after `name` marks the
        // insertion line; with none, insert after the last entry.
        let mut before: Option<usize> = None;
        let mut last_end: Option<usize> = None;
        for (key, item) in table.iter() {
            let Some(span) = item.span() else { continue };
            if key > name {
                before = Some(before.map_or(span.start, |b: usize| b.min(span.start)));
            } else {
                last_end = Some(last_end.map_or(span.end, |e: usize| e.max(span.end)));
            }
        }
        let pos = match (before, last_end) {
            // Start of the successor entry's line.
            (Some(b), _) => self.raw[..b].rfind('\n').map(|i| i + 1).unwrap_or(0),
            // Just past the last predecessor entry's line.
            (None, Some(e)) => self.raw[e..]
                .find('\n')
                .map(|i| e + i + 1)
                .unwrap_or(self.raw.len()),
            // Empty table: right after its header line.
            (None, None) => {
                let header = self.raw.find("[workspace.dependencies]").unwrap_or(0);
                self.raw[header..]
                    .find('\n')
                    .map(|i| header + i + 1)
                    .unwrap_or(self.raw.len())
            }
        };
        let line = self.raw[..pos].bytes().filter(|&b| b == b'\n').count() as u32 + 1;
        Some((line, pos as u32, entry))
    }

    /// Byte position + text creating a `[workspace.dependencies]` table at
    /// end-of-file with every entry, sorted — ONE insertion no matter how
    /// many deps seed it. The absent-table counterpart of
    /// [`Manifest::workspace_dep_insertion`]: emitting the header per dep
    /// wrote N duplicate `[workspace.dependencies]` sections (`cargo
    /// metadata` rejects the manifest — the 2026-07-10 validation broke
    /// ripgrep with 17 of them). Entries are `(name, version,
    /// default_features)`. Returns `(line, byte_pos, insert_text)`.
    pub fn workspace_table_creation(&self, entries: &[(&str, &str, bool)]) -> (u32, u32, String) {
        debug_assert!(
            !self.has_workspace_deps_table(),
            "workspace_table_creation on a manifest that already has the table"
        );
        let pos = self.raw.len();
        let line = self.raw[..pos].bytes().filter(|&b| b == b'\n').count() as u32 + 1;
        let lead = if self.raw.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let mut sorted: Vec<&(&str, &str, bool)> = entries.iter().collect();
        sorted.sort_by_key(|(name, _, _)| *name);
        let mut text = format!("{lead}[workspace.dependencies]\n");
        for (name, version, default_features) in sorted {
            text.push_str(&workspace_entry_text(name, version, *default_features));
        }
        (line, pos as u32, text)
    }

    /// Build the canonical workspace-form replacement *line* for `dep_name`:
    /// `<indent><name> = { workspace = true[, features = [...], optional = true,
    /// default-features = false] }`.
    ///
    /// Indent is preserved from the original line (read via [`Manifest::raw`]).
    /// Returns `None` if the dep can't be located or the value is a shape we
    /// don't rewrite (multi-line inline table, or a `[dependencies.<name>]`
    /// table block — those aren't single-line replacements).
    ///
    /// `version`/`git`/`registry`/`path` keys are dropped (the workspace
    /// inherit covers them); `features`/`optional`/`default-features` are
    /// preserved alongside `workspace = true`.
    pub fn format_workspace_dep(&self, section: DepSection, dep_name: &str) -> Option<String> {
        let location = self.locate_dep(section, dep_name)?;
        let original_line = &self.raw[location.byte_start as usize..location.byte_end as usize];
        let indent_end = original_line
            .char_indices()
            .find(|(_, c)| !c.is_whitespace())
            .map(|(i, _)| i)
            .unwrap_or(0);
        let indent = &original_line[..indent_end];

        let table = self.section_table(section)?;
        let item = table.get(dep_name)?;
        let new_value = format_workspace_value(item)?;
        Some(format!("{indent}{dep_name} = {new_value}"))
    }
}

/// One `[workspace.dependencies]` entry line. `default-features = false` is
/// carried onto the workspace entry because cargo resolves features from
/// THERE: a member-side `default-features = false` under `workspace = true`
/// is ignored (with a warning) when the workspace entry lacks it — the
/// helix-breaking shape from the 2026-07-10 validation (a hoisted `gix`
/// silently regained its default features and re-resolved onto a yanked
/// transitive dep).
fn workspace_entry_text(name: &str, version: &str, default_features: bool) -> String {
    if default_features {
        format!("{name} = \"{version}\"\n")
    } else {
        format!("{name} = {{ version = \"{version}\", default-features = false }}\n")
    }
}

/// Build the inline-table value `{ workspace = true[, ...preserved] }` for a
/// dep currently shaped as `Value::String("1.0")` or
/// `Value::InlineTable({...})`. Returns `None` for shapes we don't rewrite
/// (e.g. `[dependencies.<name>]` block — those are `Item::Table`, not a
/// `Value`, and don't fit a single-line replacement).
fn format_workspace_value(item: &Item) -> Option<String> {
    let value = item.as_value()?;
    let mut new_table = toml_edit::InlineTable::new();
    new_table.insert("workspace", true.into());

    // Inline-table source: preserve the keys that cargo allows alongside
    // workspace = true.
    if let Value::InlineTable(existing) = value {
        for key in ["features", "optional", "default-features"] {
            if let Some(v) = existing.get(key) {
                let mut cloned = v.clone();
                // Strip decor so the rendered table is canonical (no inherited
                // newlines or trailing comments from the source).
                cloned.decor_mut().clear();
                new_table.insert(key, cloned);
            }
        }
    } else if !value.is_str() {
        // Anything else (datetime, integer, array, …) doesn't fit a dep
        // line — bail rather than emit nonsense.
        return None;
    }

    Some(new_table.to_string())
}
