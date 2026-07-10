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

/// The rewrite/insertion builders behind the centralized-deps `--fix` — a
/// second `impl Manifest` kept out of this file so the read model stays under
/// the file-size ceiling. See [`Manifest::format_workspace_dep`] et al.
mod edit;

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

/// A shape-aware read-only view over ONE dependency entry's `toml_edit` item —
/// plain string (`serde = "1"`), inline table, dotted keys, and
/// `[section.name]` block table alike. The manifest-shape knowledge a
/// dep-auditing lint needs, kept model-side so it can't drift per lint.
///
/// Distinct from the name-keyed helpers below ([`Manifest::dep_package_name`]
/// etc.), which look up TOP-LEVEL section entries only — this type wraps an
/// item the caller already holds (e.g. from [`Manifest::deps`], which
/// includes `[target.<cfg>.…]` tables).
pub struct DepEntry<'a>(&'a Item);

impl<'a> DepEntry<'a> {
    pub fn new(item: &'a Item) -> Self {
        Self(item)
    }

    /// The declared version string: the whole value for the string form, or
    /// the `version` key of any table shape.
    pub fn version(&self) -> Option<&'a str> {
        self.0
            .as_str()
            .or_else(|| self.0.as_table_like()?.get("version")?.as_str())
    }

    /// `workspace = true` — already centralized.
    pub fn uses_workspace(&self) -> bool {
        self.0
            .as_table_like()
            .and_then(|t| t.get("workspace"))
            .and_then(Item::as_bool)
            .unwrap_or(false)
    }

    /// Carries a `path` source (a local dep — never centralized by version).
    pub fn is_path(&self) -> bool {
        self.0
            .as_table_like()
            .is_some_and(|t| t.contains_key("path"))
    }

    /// Carries a `git` source.
    pub fn is_git(&self) -> bool {
        self.0
            .as_table_like()
            .is_some_and(|t| t.contains_key("git"))
    }

    /// Carries a `package = "…"` rename (the dep key is a local alias).
    pub fn is_renamed(&self) -> bool {
        self.0
            .as_table_like()
            .is_some_and(|t| t.contains_key("package"))
    }

    /// The entry's `default-features` key, `None` when absent (cargo's
    /// default is `true`). Resolution-relevant: cargo IGNORES a member's
    /// `default-features = false` when the inherited
    /// `[workspace.dependencies]` entry doesn't declare it — hoisting a dep
    /// without carrying this flag silently changes its feature set.
    pub fn default_features(&self) -> Option<bool> {
        self.0
            .as_table_like()
            .and_then(|t| t.get("default-features"))
            .and_then(Item::as_bool)
    }
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
    /// `Manifest::workspace_package_publish`.
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

    /// Absolute path of the manifest file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Raw source bytes. Useful for byte-addressed fix suggestions and
    /// for callers that scan comment-form directives in the manifest text.
    pub fn raw(&self) -> &str {
        &self.raw
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
        tables.extend(self.target_section_tables(section));
        tables
    }

    /// Only the `[target.<cfg>.<section>]` tables for `section` — the
    /// platform-gated declarations [`Manifest::declared_deps`] flags.
    fn target_section_tables(&self, section: DepSection) -> Vec<&dyn TableLike> {
        let mut tables: Vec<&dyn TableLike> = Vec::new();
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

    /// Whether this (root) manifest declares a `[workspace.dependencies]`
    /// table at all — the distinction that decides which insertion API
    /// applies: an existing table takes per-dep insertions
    /// ([`Manifest::workspace_dep_insertion`]), an absent one must be created
    /// ONCE with every entry ([`Manifest::workspace_table_creation`]), or
    /// each insertion would carry its own duplicate `[workspace.dependencies]`
    /// header. (Entries themselves are read via
    /// [`Manifest::deps`]`(DepSection::WorkspaceDependencies)`.)
    pub fn has_workspace_deps_table(&self) -> bool {
        self.section_table(DepSection::WorkspaceDependencies)
            .is_some()
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
    /// [`FastModel::root_manifest`](crate::FastModel::root_manifest)'s
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
    pub(crate) fn publish(&self) -> Publish {
        let item = self
            .doc
            .as_table()
            .get("package")
            .and_then(Item::as_table_like)
            .and_then(|p| p.get("publish"));
        classify_publish_item(item)
    }

    /// `true` if this manifest declares a `[workspace]` table (a virtual root
    /// or a root package that also defines the workspace). Distinguishes a real
    /// workspace root from a lone `[package]` that cargo treats as its own
    /// implicit workspace.
    pub fn defines_workspace(&self) -> bool {
        self.doc.as_table().contains_key("workspace")
    }

    /// `[workspace.package] publish` from a workspace *root* manifest — the
    /// inheritance source for members that declare `publish.workspace = true`.
    /// [`Publish::Absent`] if there's no such key.
    pub(crate) fn workspace_package_publish(&self) -> Publish {
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
    /// and `[build-dependencies]` — top-level and `[target.<cfg>.…]` tables
    /// alike. Excludes the workspace.dependencies section (that's the
    /// root-only sink, not a "the crate depends on X" signal). Whether a
    /// platform-gated dep may be *judged* is the engine's call
    /// (`wl_engine::semantic`'s `NotJudged::TargetGated`, from cargo
    /// metadata's `target` field), not a manifest-shape concern.
    pub fn declared_deps(&self) -> impl Iterator<Item = DeclaredDep> + '_ {
        DepSection::member_sections().into_iter().flat_map(|s| {
            let dep = move |name: &str| DeclaredDep {
                section: s,
                original_name: name.to_string(),
                normalized_name: name.replace('-', "_"),
            };
            self.section_table(s)
                .into_iter()
                .chain(self.target_section_tables(s))
                .flat_map(|t| t.iter())
                .map(move |(name, _)| dep(name))
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

    /// The features this package enables under **default features**: the
    /// transitive closure of `default` within its own `[features]` table
    /// (bare entries name sibling features; `dep:`/`?`/`/` forms activate
    /// dependencies, not local features, and are skipped). `cfg(feature)`
    /// always refers to the *enclosing* package's features, so this local
    /// closure is exactly the set a plain `cargo build` compiles with.
    pub fn default_features(&self) -> BTreeSet<String> {
        let mut enabled = BTreeSet::new();
        let Some(features) = self
            .doc
            .as_table()
            .get("features")
            .and_then(Item::as_table_like)
        else {
            return enabled;
        };
        let mut queue = vec!["default".to_string()];
        while let Some(name) = queue.pop() {
            if !enabled.insert(name.clone()) {
                continue;
            }
            let Some(values) = features.get(&name).and_then(|v| v.as_array()) else {
                continue;
            };
            for entry in values.iter() {
                if let Some(raw) = entry.as_str()
                    && !raw.starts_with("dep:")
                    && !raw.contains('/')
                {
                    queue.push(raw.trim().to_string());
                }
            }
        }
        enabled
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

    /// The [`DepEntry`] shape view over every TOML form a dep entry takes.
    #[test]
    fn dep_entry_reads_every_toml_shape() {
        let m = parse(
            "[dependencies]\n\
             plain = \"1.0\"\n\
             inline = { version = \"2.0\", features = [\"x\"] }\n\
             ws = { workspace = true }\n\
             local = { path = \"../local\" }\n\
             gitdep = { git = \"https://example.com/r.git\" }\n\
             alias = { package = \"real-name\", version = \"3.0\" }\n\
             dotted.workspace = true\n\
             \n\
             [dependencies.block]\n\
             version = \"4.0\"\n",
        );
        let table = m.section_table(DepSection::Dependencies).unwrap();
        let entry = |name: &str| DepEntry::new(table.get(name).unwrap());

        assert_eq!(entry("plain").version(), Some("1.0"));
        assert!(!entry("plain").uses_workspace());
        assert!(!entry("plain").is_renamed());

        assert_eq!(entry("inline").version(), Some("2.0"));
        assert!(entry("ws").uses_workspace());
        assert!(entry("dotted").uses_workspace());
        assert!(entry("local").is_path());
        assert!(entry("gitdep").is_git());
        assert_eq!(entry("gitdep").version(), None);
        assert!(entry("alias").is_renamed());
        assert_eq!(entry("alias").version(), Some("3.0"));
        assert_eq!(entry("block").version(), Some("4.0"));
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
    fn workspace_deps_table_readable_via_deps_and_presence_flag() {
        let m = parse(
            r#"
[workspace.dependencies]
serde = "1"
tokio = { version = "1", features = ["full"] }
"#,
        );
        assert!(m.has_workspace_deps_table());
        let names: BTreeSet<String> = m
            .deps(DepSection::WorkspaceDependencies)
            .map(|(name, _)| name.to_string())
            .collect();
        assert_eq!(names, BTreeSet::from(["serde".into(), "tokio".into()]));

        let absent = parse("[package]\nname = \"foo\"\n");
        assert!(!absent.has_workspace_deps_table());
        assert_eq!(absent.deps(DepSection::WorkspaceDependencies).count(), 0);
    }

    /// Sorted-position insertion into an EXISTING table; `default-features =
    /// false` rides along on the entry (cargo resolves features from the
    /// workspace side — a member-only flag is ignored).
    #[test]
    fn workspace_dep_insertion_sorts_and_carries_default_features() {
        let m = parse("[workspace.dependencies]\nanyhow = \"1\"\nserde = \"1\"\n");
        assert!(m.has_workspace_deps_table());
        let (_, pos, text) = m.workspace_dep_insertion("log", "0.4", false).unwrap();
        assert_eq!(
            text,
            "log = { version = \"0.4\", default-features = false }\n"
        );
        // Between anyhow and serde: at the start of serde's line.
        assert_eq!(&m.raw()[pos as usize..pos as usize + 5], "serde");
        // Plain entry keeps the string form.
        let (_, _, plain) = m.workspace_dep_insertion("log", "0.4", true).unwrap();
        assert_eq!(plain, "log = \"0.4\"\n");
    }

    /// No table → per-dep insertion refuses (`None`); creating the table is
    /// [`Manifest::workspace_table_creation`]'s job, ONCE with every entry.
    /// The per-dep path used to emit its own `[workspace.dependencies]`
    /// header here — N applied fixes stacked N duplicate sections and cargo
    /// rejected the manifest (ripgrep, 2026-07-10 validation).
    #[test]
    fn absent_table_refuses_per_dep_insertion_and_creates_once() {
        let m = parse("[workspace]\nmembers = [\"crates/*\"]\n");
        assert!(m.workspace_dep_insertion("log", "0.4", true).is_none());

        let (_, pos, text) =
            m.workspace_table_creation(&[("serde", "1", true), ("log", "0.4", false)]);
        assert_eq!(pos as usize, m.raw().len());
        assert_eq!(
            text,
            "\n[workspace.dependencies]\n\
             log = { version = \"0.4\", default-features = false }\n\
             serde = \"1\"\n"
        );
    }

    #[test]
    fn dep_entry_default_features_reads_the_key() {
        let m = parse(
            "[dependencies]\na = \"1\"\nb = { version = \"1\", default-features = false }\n\
             c = { version = \"1\", default-features = true }\n",
        );
        let df = |name: &str| {
            let table = m.doc.as_table().get("dependencies").unwrap();
            DepEntry::new(table.as_table_like().unwrap().get(name).unwrap()).default_features()
        };
        assert_eq!(df("a"), None);
        assert_eq!(df("b"), Some(false));
        assert_eq!(df("c"), Some(true));
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
}
