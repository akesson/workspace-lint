//! Parsed-and-located `Cargo.toml` view.
//!
//! Owns the raw source bytes plus a `toml_edit::Document` parse for
//! structural reads. Stays *read-only* — the syn-workspace model contract.
//! Mutation needed by callers that want to suggest replacements (e.g.
//! [`Manifest::format_workspace_dep`]) happens on a local `DocumentMut`
//! scratch and returns a `String`; the stored doc is never modified.
//!
//! Typical uses:
//!
//! - Enumerating `[dependencies]` / `[dev-dependencies]` /
//!   `[build-dependencies]` per crate, then checking each against the
//!   workspace's `[workspace.dependencies]` table.
//! - Building delete- or rewrite-line suggestions for `--fix`-style
//!   tooling via [`Manifest::locate_dep`] / [`Manifest::format_workspace_dep`].
//!
//! Locator note: [`Manifest::locate_dep`] uses a line-based byte scanner
//! rather than `toml_edit`'s value-level span info. The line-based form
//! returns the *whole `key = value` line* including indent — which is
//! usually what callers want — without needing to walk decor/whitespace
//! around the value span. It deliberately bails on entries that aren't a
//! single line: an inline table whose value wraps (e.g. a `features = [ … ]`
//! array spanning lines — valid TOML, since the newlines sit inside a value)
//! and the `[dependencies.<name>]` table-block form. For *deletion*,
//! [`Manifest::locate_dep_entry`] covers those via the parsed document's
//! retained spans; only the workspace-form *rewrite*
//! ([`Manifest::format_workspace_dep`]) stays single-line-only.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use toml_edit::{Document, Item, Table, TableLike, Value};

use super::{FastError as Error, Result};

/// Which dependency table a query targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DepSection {
    Dependencies,
    DevDependencies,
    BuildDependencies,
    /// `[workspace.dependencies]` — only valid on the root manifest.
    WorkspaceDependencies,
}

impl DepSection {
    /// Cargo's table-header form (e.g. `"dev-dependencies"`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dependencies => "dependencies",
            Self::DevDependencies => "dev-dependencies",
            Self::BuildDependencies => "build-dependencies",
            Self::WorkspaceDependencies => "workspace.dependencies",
        }
    }

    /// The non-workspace sections in iteration order. Most consumers
    /// want all three; `WorkspaceDependencies` is queried explicitly.
    pub fn member_sections() -> [DepSection; 3] {
        [
            Self::Dependencies,
            Self::DevDependencies,
            Self::BuildDependencies,
        ]
    }
}

/// Byte location of a single dep line within the source manifest.
///
/// Covers the *line* (including leading indent, excluding trailing `\n` /
/// `\r\n`). Suitable for `--fix`-style byte-range replacement against the
/// `Manifest::raw()` content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepLocation {
    /// 1-indexed line number of the dep entry.
    pub line: u32,
    /// Inclusive byte offset of the line start.
    pub byte_start: u32,
    /// Exclusive byte offset of the line end (EOL bytes excluded).
    pub byte_end: u32,
}

/// One declared dependency, as enumerated by [`Manifest::declared_deps`] or
/// `Crate::declared_deps`. Carries enough to match a dep entry against
/// the resolver's per-crate reference index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredDep {
    pub section: DepSection,
    /// Name as written in the Cargo.toml (may contain hyphens).
    pub original_name: String,
    /// Cargo-form name with hyphens replaced by underscores. Matches the
    /// crate-name segment in `ResolvedPath`.
    pub normalized_name: String,
}

/// A parsed Cargo manifest. Pure read-only data on the model side.
#[derive(Debug, Clone)]
pub struct Manifest {
    path: PathBuf,
    raw: String,
    doc: Document<String>,
}

/// A crate's `[package] publish` declaration, read from the raw manifest.
///
/// `cargo metadata` collapses an *absent* `publish` field and an explicit
/// `publish = true` both to "unrestricted" ([`None`]), erasing a distinction
/// some callers need. Reading the parsed document directly preserves it. This
/// type only *reports* what the manifest says — it carries no policy about
/// what an absent field should mean.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Publish {
    /// Explicit `publish = true`.
    ExplicitTrue,
    /// `publish = false` or `publish = []` — never published.
    Disabled,
    /// `publish = ["registry", …]` — restricted to the named (non-empty) registries.
    Registries(Vec<String>),
    /// `publish.workspace = true` — inherits `[workspace.package] publish`.
    /// Resolve against the workspace root manifest's
    /// [`Manifest::workspace_package_publish`].
    Inherited,
    /// No `publish` key (cargo default: publishable to crates.io).
    Absent,
}

/// Classify a `publish` [`Item`] (or its absence) into a [`Publish`].
fn classify_publish_item(item: Option<&Item>) -> Publish {
    let Some(item) = item else {
        return Publish::Absent;
    };
    // `publish.workspace = true` / `publish = { workspace = true }`.
    if let Some(t) = item.as_table_like()
        && t.get("workspace")
            .and_then(|w| w.as_value())
            .and_then(Value::as_bool)
            == Some(true)
    {
        return Publish::Inherited;
    }
    let Some(value) = item.as_value() else {
        return Publish::Absent;
    };
    if let Some(b) = value.as_bool() {
        return if b {
            Publish::ExplicitTrue
        } else {
            Publish::Disabled
        };
    }
    if let Some(arr) = value.as_array() {
        let regs: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        return if regs.is_empty() {
            Publish::Disabled // `publish = []` means "no registries" == forbidden.
        } else {
            Publish::Registries(regs)
        };
    }
    Publish::Absent
}

impl Manifest {
    /// Read and parse `Cargo.toml` at `path`. Errors propagate as
    /// [`FastError::Manifest`](super::FastError::Manifest) with the path embedded.
    pub fn load(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let raw = std::fs::read_to_string(&path).map_err(|e| Error::manifest(&path, e))?;
        let doc: Document<String> =
            Document::parse(raw.clone()).map_err(|e| Error::manifest(&path, e))?;
        Ok(Self { path, raw, doc })
    }

    /// An empty manifest at a synthetic path. Provided for tests and
    /// synthesized `Crate` fixtures — callers building a workspace by hand
    /// (rather than via `Workspace::load`) can use this to satisfy the
    /// `manifest` field without touching disk.
    pub fn empty() -> Self {
        Self {
            path: PathBuf::new(),
            raw: String::new(),
            doc: Document::parse(String::new()).expect("empty toml is parseable"),
        }
    }

    /// Absolute path of the manifest file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Raw source bytes. Useful for byte-addressed fix suggestions and
    /// for callers that scan comment-form directives in the manifest text.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// The parsed read-only document.
    pub fn doc(&self) -> &Document<String> {
        &self.doc
    }

    /// Iterate `(name, item)` pairs in a given section. Returns an empty
    /// iterator if the section is absent.
    ///
    /// For [`DepSection::WorkspaceDependencies`] this drills into
    /// `[workspace.dependencies]`. For the three member sections it reads the
    /// top-level table **and** every `[target.<cfg>.<section>]` table, so a
    /// platform-gated dependency is enumerated just like an unconditional one
    /// (a target-gated dep that's unused / un-centralized is otherwise a silent
    /// false negative for both deps lints). A dep may appear more than once if
    /// declared under several `cfg`s — callers that need a set should dedup.
    pub fn deps(&self, section: DepSection) -> impl Iterator<Item = (&str, &Item)> + '_ {
        self.section_tables(section)
            .into_iter()
            .flat_map(|t| t.iter())
    }

    /// All tables contributing to `section`: the top-level table plus, for the
    /// three member sections, every `[target.<cfg>.<section>]` table. Workspace
    /// deps have no target form.
    fn section_tables(&self, section: DepSection) -> Vec<&dyn TableLike> {
        let mut tables: Vec<&dyn TableLike> = Vec::new();
        if let Some(t) = self.section_table(section) {
            tables.push(t);
        }
        if section != DepSection::WorkspaceDependencies
            && let Some(target) = self
                .doc
                .as_table()
                .get("target")
                .and_then(Item::as_table_like)
        {
            for (_cfg, cfg_item) in target.iter() {
                if let Some(dep_table) = cfg_item
                    .as_table_like()
                    .and_then(|t| t.get(section.as_str()))
                    .and_then(Item::as_table_like)
                {
                    tables.push(dep_table);
                }
            }
        }
        tables
    }

    /// All dep names in `[workspace.dependencies]`. Convenience for
    /// callers that need to test "is this dep centralized?".
    pub fn workspace_dep_names(&self) -> BTreeSet<String> {
        self.section_table(DepSection::WorkspaceDependencies)
            .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
            .unwrap_or_default()
    }

    /// The version string for `dep_name` in `section`, if the entry uses
    /// the plain-string form (`serde = "1.0"`) or an inline table with a
    /// `version` key (`serde = { version = "1.0", ... }`).
    ///
    /// Returns `None` if the entry is absent, uses `workspace = true`, or
    /// is a shape that doesn't carry a version literal (e.g. pure-`git`
    /// or `path` deps).
    ///
    /// Lets callers stay off the raw `toml_edit::Item` API for the common
    /// "read this dep's version" lookup.
    pub fn get_dep_version(&self, section: DepSection, dep_name: &str) -> Option<&str> {
        let table = self.section_table(section)?;
        let item = table.get(dep_name)?;
        let value = item.as_value()?;
        if let Some(s) = value.as_str() {
            return Some(s);
        }
        let table = value.as_inline_table()?;
        table.get("version")?.as_str()
    }

    /// The package a dependency entry actually resolves to, when the entry
    /// renames it via `package = "…"` — `foo = { package = "bar" }` or a
    /// `[section.foo]` block with a `package` key. Returns `None` when the
    /// entry carries no rename (the caller then uses the dep key itself as the
    /// package name) or when the dep is absent.
    ///
    /// Does **not** follow `workspace = true` inheritance — a member entry may
    /// inherit a rename declared in the root `[workspace.dependencies]`. Detect
    /// that with [`Manifest::dep_uses_workspace`] and resolve against
    /// [`FastModel::root_manifest`](crate::fast::FastModel::root_manifest)'s
    /// [`DepSection::WorkspaceDependencies`].
    pub fn dep_package_name(&self, section: DepSection, dep_name: &str) -> Option<String> {
        let table = self
            .section_table(section)?
            .get(dep_name)?
            .as_table_like()?;
        Some(table.get("package")?.as_str()?.to_string())
    }

    /// `true` if the dep entry inherits from `[workspace.dependencies]` via
    /// `foo.workspace = true` / `foo = { workspace = true }`. Such an entry may
    /// pick up a `package = "…"` rename declared centrally — see
    /// [`Manifest::dep_package_name`].
    pub fn dep_uses_workspace(&self, section: DepSection, dep_name: &str) -> bool {
        self.section_table(section)
            .and_then(|t| t.get(dep_name))
            .and_then(Item::as_table_like)
            .and_then(|t| t.get("workspace"))
            .and_then(Item::as_value)
            .and_then(toml_edit::Value::as_bool)
            .unwrap_or(false)
    }

    /// This crate's `[package] publish` declaration. See [`Publish`] for why
    /// this reads the parsed document instead of relying on `cargo metadata`.
    /// Returns [`Publish::Inherited`] for `publish.workspace = true`; resolve
    /// it against the workspace root with
    /// [`Manifest::workspace_package_publish`].
    pub fn publish(&self) -> Publish {
        let item = self
            .doc
            .as_table()
            .get("package")
            .and_then(Item::as_table_like)
            .and_then(|p| p.get("publish"));
        classify_publish_item(item)
    }

    /// `[workspace.package] publish` from a workspace *root* manifest — the
    /// inheritance source for members that declare `publish.workspace = true`.
    /// [`Publish::Absent`] if there's no such key.
    pub fn workspace_package_publish(&self) -> Publish {
        let item = self
            .doc
            .as_table()
            .get("workspace")
            .and_then(Item::as_table_like)
            .and_then(|w| w.get("package"))
            .and_then(Item::as_table_like)
            .and_then(|p| p.get("publish"));
        classify_publish_item(item)
    }

    /// Enumerate declared deps across `[dependencies]`, `[dev-dependencies]`,
    /// and `[build-dependencies]`. Excludes the workspace.dependencies
    /// section (that's the root-only sink, not a "the crate depends on X"
    /// signal).
    pub fn declared_deps(&self) -> impl Iterator<Item = DeclaredDep> + '_ {
        DepSection::member_sections().into_iter().flat_map(|s| {
            self.deps(s).map(move |(name, _)| DeclaredDep {
                section: s,
                original_name: name.to_string(),
                normalized_name: name.replace('-', "_"),
            })
        })
    }

    /// Dependency names referenced from this manifest's `[features]` table,
    /// underscore-normalized.
    ///
    /// A feature value can activate a dependency via `dep:NAME`, `NAME?/feat`
    /// (weak), or `NAME/feat`; a bare `NAME` references another feature (or, in
    /// pre-`dep:` style, implicitly the optional dep of that name). We extract
    /// the leading identifier of every value — stripping a `dep:` prefix and
    /// taking the part before the first `?` or `/` — and `replace('-', "_")`.
    ///
    /// Over-extraction is deliberate and harmless: the unused-deps lint only
    /// ever checks *declared dependency* names against this set, so stray
    /// feature names (`default`, `std`, …) that aren't deps match nothing. This
    /// spares feature-plumbing-only optional deps — declared solely to forward a
    /// feature and never named in code — from false `unused-deps` reports.
    pub fn feature_dep_refs(&self) -> std::collections::HashSet<String> {
        let mut refs = std::collections::HashSet::new();
        let Some(features) = self
            .doc
            .as_table()
            .get("features")
            .and_then(Item::as_table_like)
        else {
            return refs;
        };
        for (_feature, value) in features.iter() {
            let Some(array) = value.as_array() else {
                continue;
            };
            for entry in array.iter() {
                let Some(raw) = entry.as_str() else {
                    continue;
                };
                let name = raw
                    .strip_prefix("dep:")
                    .unwrap_or(raw)
                    .split(['?', '/'])
                    .next()
                    .unwrap_or("")
                    .trim();
                if !name.is_empty() {
                    refs.insert(name.replace('-', "_"));
                }
            }
        }
        refs
    }

    /// Locate the line for a specific dep key inside the given section.
    /// Returns `None` for entries whose value wraps across multiple lines
    /// (multi-line inline table) or is declared as a `[<section>.dep]` table
    /// block — neither is a single-line replacement. For a full-entry span that
    /// *does* cover those forms (used by `--fix` deletion), see
    /// [`Manifest::locate_dep_entry`].
    pub fn locate_dep(&self, section: DepSection, dep_name: &str) -> Option<DepLocation> {
        locate_dep_line(&self.raw, section, dep_name)
    }

    /// Locate the full byte span of a dep entry, including the multi-line forms
    /// [`Manifest::locate_dep`] rejects: a multi-line inline table
    /// (`foo = { …\n… }`), a dotted key (`foo.workspace = true`), and a
    /// `[<section>.foo]` table block. Unlike the line scanner this reads the
    /// parsed document's retained spans, so it covers the whole entry — the
    /// inline table through its closing `}`, or the block from its
    /// `[section.foo]` header through its last body line — without brace- or
    /// section-body guessing. The span starts at the entry's line beginning
    /// (eating leading indent) and ends at the value's last byte (EOL excluded),
    /// matching [`Manifest::locate_dep`]'s contract so the same byte-range
    /// deletion applies.
    ///
    /// This is for *deletion* (the whole entry goes away); it is deliberately
    /// not used by the workspace-form rewrite ([`Manifest::format_workspace_dep`]),
    /// which only reshapes single-line values.
    pub fn locate_dep_entry(&self, section: DepSection, dep_name: &str) -> Option<DepLocation> {
        let item = self.section_table(section)?.get(dep_name)?;
        let span = item.span()?;
        // The end of the entry. For a value (incl. an inline table) `span`
        // already covers it through the closing `}`. For a `[section.foo]` block,
        // toml_edit's table span is unreliable — it sometimes covers only the
        // header line — so extend the end across the block's body by taking the
        // max of its members' value spans. (For an inline table the body values
        // end before the `}`, so the `}`-inclusive `span.end` still wins.)
        let mut end = span.end;
        if let Some(table) = item.as_table_like() {
            for (_key, value) in table.iter() {
                if let Some(member) = value.span() {
                    end = end.max(member.end);
                }
            }
        }
        // Rewind to the start of the entry's first line so leading indent is
        // removed too. `span.start` sits on that line for every real dep form:
        // the `{` shares the key's line for an inline table, and a block's span
        // starts at its `[section.foo]` header.
        let byte_start = self.raw[..span.start]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
        let line = self.raw[..byte_start]
            .bytes()
            .filter(|&b| b == b'\n')
            .count() as u32
            + 1;
        Some(DepLocation {
            line,
            byte_start: byte_start as u32,
            byte_end: end as u32,
        })
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

    fn section_table(&self, section: DepSection) -> Option<&dyn TableLike> {
        let root: &Table = self.doc.as_table();
        match section {
            DepSection::Dependencies => root.get("dependencies").and_then(Item::as_table_like),
            DepSection::DevDependencies => {
                root.get("dev-dependencies").and_then(Item::as_table_like)
            }
            DepSection::BuildDependencies => {
                root.get("build-dependencies").and_then(Item::as_table_like)
            }
            DepSection::WorkspaceDependencies => root
                .get("workspace")
                .and_then(Item::as_table_like)?
                .get("dependencies")
                .and_then(Item::as_table_like),
        }
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

fn locate_dep_line(content: &str, section: DepSection, dep_name: &str) -> Option<DepLocation> {
    let mut current_section: Option<String> = None;
    let mut byte_offset = 0usize;
    for (i, raw_line) in content.split('\n').enumerate() {
        // `split('\n')` leaves a trailing `\r` on CRLF files. Trim it for
        // structural matching (section headers etc.) but keep the original
        // length for byte-offset accounting so callers can still address
        // the file accurately.
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        let line_with_newline_len = raw_line.len() + 1; // raw_line + the \n we split on
        let trimmed = line.trim_start();
        if let Some(name) = parse_section_header(trimmed) {
            current_section = Some(name.to_string());
            byte_offset += line_with_newline_len;
            continue;
        }
        if current_section_matches(current_section.as_deref(), section)
            && line_matches_dep(trimmed, dep_name)
        {
            // Reject multi-line inline tables — if the line starts the
            // entry but doesn't terminate `}` on the same line, bail.
            if has_unbalanced_inline_table(line) {
                return None;
            }
            let start = byte_offset;
            // `end` excludes any trailing CR/LF so the structural rewriter
            // replaces only the dep line's content and leaves the file's
            // EOL bytes untouched.
            let end = byte_offset + line.len();
            return Some(DepLocation {
                line: (i + 1) as u32,
                byte_start: start as u32,
                byte_end: end as u32,
            });
        }
        byte_offset += line_with_newline_len;
    }
    None
}

fn current_section_matches(current: Option<&str>, target: DepSection) -> bool {
    let Some(current) = current else {
        return false;
    };
    current == target.as_str()
}

fn parse_section_header(line: &str) -> Option<&str> {
    let stripped = line.strip_prefix('[')?.strip_suffix(']')?;
    Some(stripped)
}

fn line_matches_dep(line: &str, dep_name: &str) -> bool {
    if !line.starts_with(dep_name) {
        return false;
    }
    let after = &line[dep_name.len()..];
    matches!(after.chars().next(), Some(c) if c == ' ' || c == '\t' || c == '=')
}

fn has_unbalanced_inline_table(line: &str) -> bool {
    let opens = line.matches('{').count();
    let closes = line.matches('}').count();
    opens > closes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(content: &str) -> Manifest {
        Manifest {
            path: PathBuf::from("/tmp/Cargo.toml"),
            raw: content.to_string(),
            doc: Document::parse(content.to_string()).unwrap(),
        }
    }

    #[test]
    fn feature_dep_refs_extracts_dep_weak_and_path_forms() {
        let m = parse(
            "[features]\n\
             perf = [\"dep:aho-corasick\", \"memchr?/avx2\", \"regex-automata/std\"]\n\
             default = [\"perf\"]\n",
        );
        let refs = m.feature_dep_refs();
        assert!(refs.contains("aho_corasick"), "dep: form: {refs:?}");
        assert!(refs.contains("memchr"), "weak ?/ form: {refs:?}");
        assert!(refs.contains("regex_automata"), "path / form: {refs:?}");
        // A bare feature reference is harmlessly included — it matches no
        // declared dep, so it can't suppress a real finding.
        assert!(refs.contains("perf"), "bare feature ref: {refs:?}");
    }

    #[test]
    fn feature_dep_refs_normalizes_hyphens() {
        let refs = parse("[features]\nx = [\"dep:aho-corasick\"]\n").feature_dep_refs();
        assert!(refs.contains("aho_corasick"));
        assert!(!refs.contains("aho-corasick"));
    }

    #[test]
    fn feature_dep_refs_empty_without_features_table() {
        assert!(
            parse("[package]\nname = \"x\"\n")
                .feature_dep_refs()
                .is_empty()
        );
    }

    #[test]
    fn dep_package_name_reads_inline_table_rename() {
        let m = parse("[dependencies]\nfoo = { package = \"bar\", version = \"1\" }\n");
        assert_eq!(
            m.dep_package_name(DepSection::Dependencies, "foo")
                .as_deref(),
            Some("bar")
        );
    }

    #[test]
    fn dep_package_name_reads_block_rename() {
        let m = parse("[dependencies.foo]\npackage = \"bar\"\nversion = \"1\"\n");
        assert_eq!(
            m.dep_package_name(DepSection::Dependencies, "foo")
                .as_deref(),
            Some("bar")
        );
    }

    #[test]
    fn dep_package_name_none_without_rename() {
        let m = parse("[dependencies]\nserde = \"1\"\nfoo = { version = \"1\" }\n");
        assert_eq!(m.dep_package_name(DepSection::Dependencies, "serde"), None);
        assert_eq!(m.dep_package_name(DepSection::Dependencies, "foo"), None);
        assert_eq!(m.dep_package_name(DepSection::Dependencies, "absent"), None);
    }

    #[test]
    fn dep_uses_workspace_detects_inheritance() {
        let m = parse(
            "[dependencies]\nfoo.workspace = true\nbar = { workspace = true }\nbaz = \"1\"\n",
        );
        assert!(m.dep_uses_workspace(DepSection::Dependencies, "foo"));
        assert!(m.dep_uses_workspace(DepSection::Dependencies, "bar"));
        assert!(!m.dep_uses_workspace(DepSection::Dependencies, "baz"));
        assert!(!m.dep_uses_workspace(DepSection::Dependencies, "absent"));
    }

    #[test]
    fn deps_includes_target_specific_tables() {
        let m = parse(
            "[dependencies]\na = \"1\"\n\
             [target.'cfg(windows)'.dependencies]\nb = \"2\"\n\
             [target.'cfg(unix)'.dependencies]\nc = \"3\"\n",
        );
        let mut names: Vec<&str> = m.deps(DepSection::Dependencies).map(|(n, _)| n).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["a", "b", "c"], "target deps must be enumerated");
    }

    #[test]
    fn deps_target_tables_respect_section() {
        let m = parse(
            "[dependencies]\na = \"1\"\n\
             [target.'cfg(unix)'.dev-dependencies]\nd = \"1\"\n\
             [target.'cfg(unix)'.build-dependencies]\nbd = \"1\"\n",
        );
        let deps: Vec<&str> = m.deps(DepSection::Dependencies).map(|(n, _)| n).collect();
        assert_eq!(
            deps,
            vec!["a"],
            "a target dev-dep must not leak into [dependencies]"
        );
        let dev: Vec<&str> = m
            .deps(DepSection::DevDependencies)
            .map(|(n, _)| n)
            .collect();
        assert_eq!(dev, vec!["d"]);
        let build: Vec<&str> = m
            .deps(DepSection::BuildDependencies)
            .map(|(n, _)| n)
            .collect();
        assert_eq!(build, vec!["bd"]);
    }

    #[test]
    fn publish_explicit_true() {
        assert_eq!(
            parse("[package]\nname = \"x\"\npublish = true\n").publish(),
            Publish::ExplicitTrue
        );
    }

    #[test]
    fn publish_false_and_empty_array_are_disabled() {
        assert_eq!(
            parse("[package]\npublish = false\n").publish(),
            Publish::Disabled
        );
        assert_eq!(
            parse("[package]\npublish = []\n").publish(),
            Publish::Disabled
        );
    }

    #[test]
    fn publish_registry_list() {
        assert_eq!(
            parse("[package]\npublish = [\"my-registry\"]\n").publish(),
            Publish::Registries(vec!["my-registry".to_string()])
        );
    }

    #[test]
    fn publish_absent_is_absent_not_true() {
        // The key distinction `cargo metadata` erases: absent != explicit true.
        assert_eq!(
            parse("[package]\nname = \"x\"\n").publish(),
            Publish::Absent
        );
    }

    #[test]
    fn publish_workspace_inheritance_is_reported_as_inherited() {
        assert_eq!(
            parse("[package]\npublish.workspace = true\n").publish(),
            Publish::Inherited
        );
        assert_eq!(
            parse("[package]\npublish = { workspace = true }\n").publish(),
            Publish::Inherited
        );
    }

    #[test]
    fn workspace_package_publish_reads_root_inheritance_source() {
        let root = parse("[workspace.package]\npublish = true\n");
        assert_eq!(root.workspace_package_publish(), Publish::ExplicitTrue);
        // Absent when the root declares no workspace-package publish.
        assert_eq!(
            parse("[workspace]\nmembers = []\n").workspace_package_publish(),
            Publish::Absent
        );
    }

    #[test]
    fn workspace_dep_names_collects_root_table() {
        let m = parse(
            r#"
[workspace.dependencies]
serde = "1"
tokio = { version = "1", features = ["full"] }
"#,
        );
        let names = m.workspace_dep_names();
        assert_eq!(names, BTreeSet::from(["serde".into(), "tokio".into()]));
    }

    #[test]
    fn workspace_dep_names_empty_when_absent() {
        let m = parse(
            r#"
[package]
name = "foo"
"#,
        );
        assert!(m.workspace_dep_names().is_empty());
    }

    #[test]
    fn declared_deps_walks_all_member_sections() {
        let m = parse(
            r#"
[dependencies]
a = "1"
[dev-dependencies]
b = "1"
[build-dependencies]
c = "1"
"#,
        );
        let deps: Vec<_> = m.declared_deps().collect();
        assert_eq!(deps.len(), 3);
        assert!(
            deps.iter()
                .any(|d| d.section == DepSection::Dependencies && d.original_name == "a")
        );
        assert!(
            deps.iter()
                .any(|d| d.section == DepSection::DevDependencies && d.original_name == "b")
        );
        assert!(
            deps.iter()
                .any(|d| d.section == DepSection::BuildDependencies && d.original_name == "c")
        );
    }

    #[test]
    fn declared_deps_normalizes_hyphens() {
        let m = parse(
            r#"
[dependencies]
my-crate = "1"
"#,
        );
        let deps: Vec<_> = m.declared_deps().collect();
        assert_eq!(deps[0].original_name, "my-crate");
        assert_eq!(deps[0].normalized_name, "my_crate");
    }

    #[test]
    fn locate_dep_finds_line_in_correct_section() {
        let content = "[package]\nname = \"a\"\n\n[dependencies]\nserde = \"1.0\"\n";
        let m = parse(content);
        let loc = m.locate_dep(DepSection::Dependencies, "serde").unwrap();
        assert_eq!(loc.line, 5);
        assert_eq!(
            &content[loc.byte_start as usize..loc.byte_end as usize],
            "serde = \"1.0\""
        );
    }

    #[test]
    fn locate_dep_distinguishes_sections() {
        let content = "[dependencies]\nfoo = \"1\"\n\n[dev-dependencies]\nbar = \"1\"\n";
        let m = parse(content);
        assert!(m.locate_dep(DepSection::Dependencies, "bar").is_none());
        assert!(m.locate_dep(DepSection::DevDependencies, "foo").is_none());
    }

    #[test]
    fn locate_dep_crlf() {
        let content = "[package]\r\nname = \"a\"\r\n\r\n[dependencies]\r\nserde = \"1.0\"\r\n";
        let m = parse(content);
        let loc = m.locate_dep(DepSection::Dependencies, "serde").unwrap();
        assert_eq!(loc.line, 5);
        assert_eq!(
            &content[loc.byte_start as usize..loc.byte_end as usize],
            "serde = \"1.0\""
        );
        // The bytes after `end` must still be the original `\r\n`, untouched.
        assert_eq!(
            &content[loc.byte_end as usize..loc.byte_end as usize + 2],
            "\r\n"
        );
    }

    #[test]
    fn locate_dep_entry_spans_multiline_inline_table() {
        let content = "[dependencies]\nserde = { version = \"1\", features = [\n    \"derive\",\n] }\n\n[dev-dependencies]\ntokio = \"1\"\n";
        let m = parse(content);
        // The line locator bails on the wrapped inline table.
        assert!(m.locate_dep(DepSection::Dependencies, "serde").is_none());
        // The span locator captures the whole entry through the closing `}`.
        let loc = m
            .locate_dep_entry(DepSection::Dependencies, "serde")
            .unwrap();
        assert_eq!(loc.line, 2);
        assert_eq!(
            &content[loc.byte_start as usize..loc.byte_end as usize],
            "serde = { version = \"1\", features = [\n    \"derive\",\n] }"
        );
    }

    #[test]
    fn locate_dep_entry_spans_table_block_including_header() {
        let content = "[dependencies]\nserde = \"1\"\n\n[dependencies.foo]\nversion = \"2\"\nfeatures = [\"x\"]\n\n[dev-dependencies]\ntokio = \"1\"\n";
        let m = parse(content);
        assert!(m.locate_dep(DepSection::Dependencies, "foo").is_none());
        let loc = m.locate_dep_entry(DepSection::Dependencies, "foo").unwrap();
        // Must START at the `[dependencies.foo]` header (else deletion orphans
        // it) and END at the last body line — header through body.
        assert_eq!(
            &content[loc.byte_start as usize..loc.byte_end as usize],
            "[dependencies.foo]\nversion = \"2\"\nfeatures = [\"x\"]"
        );
    }

    #[test]
    fn locate_dep_entry_spans_dotted_key() {
        // `foo.workspace = true` — the common modern form. The key is
        // `futures.workspace`, so the single-line locator never matches `futures`.
        let content = "[dependencies]\nfutures.workspace = true\nserde = \"1\"\n";
        let m = parse(content);
        assert!(m.locate_dep(DepSection::Dependencies, "futures").is_none());
        let loc = m
            .locate_dep_entry(DepSection::Dependencies, "futures")
            .unwrap();
        assert_eq!(
            &content[loc.byte_start as usize..loc.byte_end as usize],
            "futures.workspace = true"
        );
    }

    #[test]
    fn locate_dep_entry_handles_single_line_too() {
        let content = "[dependencies]\nserde = \"1.0\"\n";
        let m = parse(content);
        let loc = m
            .locate_dep_entry(DepSection::Dependencies, "serde")
            .unwrap();
        assert_eq!(loc.line, 2);
        assert_eq!(
            &content[loc.byte_start as usize..loc.byte_end as usize],
            "serde = \"1.0\""
        );
    }

    // --- format_workspace_dep scenarios ---

    #[test]
    fn format_plain_version_string() {
        let m = parse("[dependencies]\nserde = \"1.0.200\"\n");
        let out = m
            .format_workspace_dep(DepSection::Dependencies, "serde")
            .unwrap();
        assert_eq!(out, "serde = { workspace = true }");
    }

    #[test]
    fn format_preserves_leading_indent() {
        let m = parse("[dependencies]\n    serde = \"1\"\n");
        let out = m
            .format_workspace_dep(DepSection::Dependencies, "serde")
            .unwrap();
        assert_eq!(out, "    serde = { workspace = true }");
    }

    #[test]
    fn format_table_with_version_only_drops_version() {
        let m = parse("[dependencies]\nserde = { version = \"1.0\" }\n");
        let out = m
            .format_workspace_dep(DepSection::Dependencies, "serde")
            .unwrap();
        assert_eq!(out, "serde = { workspace = true }");
    }

    #[test]
    fn format_table_with_features_keeps_features() {
        let m = parse("[dependencies]\nserde = { version = \"1.0\", features = [\"derive\"] }\n");
        let out = m
            .format_workspace_dep(DepSection::Dependencies, "serde")
            .unwrap();
        assert_eq!(out, "serde = { workspace = true, features = [\"derive\"] }");
    }

    #[test]
    fn format_table_with_optional_and_default_features() {
        let m = parse(
            "[dependencies]\ntokio = { version = \"1\", optional = true, default-features = false }\n",
        );
        let out = m
            .format_workspace_dep(DepSection::Dependencies, "tokio")
            .unwrap();
        assert_eq!(
            out,
            "tokio = { workspace = true, optional = true, default-features = false }"
        );
    }

    #[test]
    fn format_git_table_strips_git_source() {
        let m = parse(
            "[dependencies]\ntonic = { git = \"https://github.com/hyperium/tonic\", branch = \"master\" }\n",
        );
        let out = m
            .format_workspace_dep(DepSection::Dependencies, "tonic")
            .unwrap();
        assert_eq!(out, "tonic = { workspace = true }");
    }

    #[test]
    fn format_missing_dep_returns_none() {
        let m = parse("[dependencies]\nserde = \"1\"\n");
        assert!(
            m.format_workspace_dep(DepSection::Dependencies, "missing")
                .is_none()
        );
    }

    #[test]
    fn deps_iterates_pairs() {
        let m = parse("[dependencies]\nserde = \"1\"\ntokio = { workspace = true }\n");
        let pairs: Vec<_> = m
            .deps(DepSection::Dependencies)
            .map(|(k, _)| k.to_string())
            .collect();
        assert_eq!(pairs, vec!["serde", "tokio"]);
    }

    // --- get_dep_version ---

    #[test]
    fn get_dep_version_reads_plain_string_form() {
        let m = parse("[dependencies]\nserde = \"1.0.200\"\n");
        assert_eq!(
            m.get_dep_version(DepSection::Dependencies, "serde"),
            Some("1.0.200")
        );
    }

    #[test]
    fn get_dep_version_reads_inline_table_version_key() {
        let m = parse("[dependencies]\nserde = { version = \"1.0\", features = [\"derive\"] }\n");
        assert_eq!(
            m.get_dep_version(DepSection::Dependencies, "serde"),
            Some("1.0")
        );
    }

    #[test]
    fn get_dep_version_returns_none_for_workspace_inherit() {
        let m = parse("[dependencies]\nserde = { workspace = true }\n");
        assert_eq!(m.get_dep_version(DepSection::Dependencies, "serde"), None);
    }

    #[test]
    fn get_dep_version_returns_none_for_git_only() {
        let m = parse(
            "[dependencies]\ntonic = { git = \"https://github.com/hyperium/tonic\", branch = \"master\" }\n",
        );
        assert_eq!(m.get_dep_version(DepSection::Dependencies, "tonic"), None);
    }

    #[test]
    fn get_dep_version_returns_none_for_missing_dep() {
        let m = parse("[dependencies]\nserde = \"1\"\n");
        assert_eq!(m.get_dep_version(DepSection::Dependencies, "missing"), None);
    }

    #[test]
    fn get_dep_version_returns_none_for_wrong_section() {
        let m = parse("[dependencies]\nserde = \"1\"\n");
        assert_eq!(
            m.get_dep_version(DepSection::DevDependencies, "serde"),
            None
        );
    }

    #[test]
    fn get_dep_version_returns_none_for_table_block_form() {
        // `[dependencies.<name>]` is `Item::Table`, not a `Value` — the helper
        // only handles single-line entries, so this returns None rather than
        // returning a misleading partial result.
        let m = parse("[dependencies.serde]\nversion = \"1.0\"\n");
        assert_eq!(m.get_dep_version(DepSection::Dependencies, "serde"), None);
    }
}
