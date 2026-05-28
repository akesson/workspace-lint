# Changelog

All notable changes to this crate will be documented here. Format loosely
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning
follows [SemVer](https://semver.org/).

## [Unreleased] — 0.4.0

### Added
- `Workspace::crate_relative_path(path) -> PathBuf` — strips the workspace
  root prefix from an absolute path so callers (mostly diagnostic-anchor
  builders) can produce paths that round-trip with the suppression
  directive scanner. Non-breaking.
- `UseBinding::source: Option<SourceSpan>` — leaf-anchored span for the
  ident that produced each binding. Lets downstream lints (architecture,
  future use-walking analyses) emit line-accurate diagnostics instead of
  pointing at the enclosing module. `Rename` uses anchor at the
  canonical (LHS) ident, since that's what the binding resolves to.

### Changed
- `bindings_from_use(item, scope)` now takes a third `file: &Path`
  parameter so it can populate `UseBinding::source`. Breaking — every
  caller must thread the parsed file's path through. The in-tree
  consumer (`module_tree::collect_module_contents`) migrates in
  lockstep.

### Migration
| Before | After |
|--------|-------|
| `UseBinding { local_name, canonical, visibility }` | add `source: None` (or a real `SourceSpan`) |
| `bindings_from_use(item, scope)` | `bindings_from_use(item, scope, file)` |

## [0.3.0]

### Added
- `Item::vis_byte_range: Option<Range<u32>>` captures the byte range of the
  `pub` keyword itself (when `visibility == Public`). Structural-fix
  consumers (visibility tighteners, dead-code removers) can rewrite the
  keyword precisely, without scanning past preceding doc comments or
  attributes. `None` for non-public items, macros (`#[macro_export]`-only),
  and synthetic / orphan spans where the resolver couldn't pin byte offsets.

### Migration
Adding a public field to `Item` is a SemVer-breaking change. In-tree consumers
construct `Item` only via the resolver, so they're unaffected. Out-of-tree
consumers that build `Item` directly (e.g. in tests) must add
`vis_byte_range: None` to their initializers.

## [0.2.0]

Library-grade pass. Most changes are breaking; the in-tree consumer
(`workspace-lint`) migrates in lockstep.

### Added
- `Workspace::warnings()` returns non-fatal `LoadWarning` entries (auxiliary
  targets that failed to parse). The library no longer prints to stderr.
- `Workspace::load_with_options(root, LoadOptions)` accepts a `LoadOptions`
  struct to configure marker-crate names for `expansion_uses!` detection.
  `Workspace::load(root)` is preserved as a thin wrapper using defaults.
- `Workspace::parse_file(path)` — on-demand `syn::File` parse; replaces the
  removed `Module.parsed_file` cache.
- `Manifest::get_dep_version(section, name)` — version-string helper so
  consumers don't reach for `toml_edit::Item::as_str()` directly.
- `LoadOptions { marker_crates: Vec<String>, .. }` with `Default` =
  `["workspace_syn", "syn_workspace_marker"]`.
- Public README and CHANGELOG; `keywords` / `categories` in `Cargo.toml`.
- Compile-time assertion that `Workspace`, `Crate`, `Target`, `Module` are
  all `Send + Sync`.

### Changed
- `Error` is now `#[non_exhaustive]`. Variants carry structured `#[source]`
  chains instead of stringifying via `format!`. Uses `thiserror`.
- `SourceSpan`: `byte_start: u32` / `byte_end: u32` collapsed into
  `byte_range: Option<Range<u32>>`. The `has_byte_range()` helper now
  delegates to `byte_range.is_some()`.

### Removed
- `Module.parsed_file: Option<Rc<syn::File>>` — removing the cached AST
  makes the entire workspace model `Send + Sync` (`syn::File` is `Send`
  but not `Sync` because `proc-macro2::Span` contains
  `PhantomData<Rc<()>>`). Consumers needing the AST call
  `Workspace::parse_file(path)` on demand and cache as they see fit.

### Migration

| Before | After |
|--------|-------|
| `module.parsed_file.clone()` | `ws.parse_file(module.file.as_ref().unwrap())?` |
| `Error::Manifest(s)` matching | `match e { Error::Manifest { path, source } => ... }` |
| `span.byte_start..span.byte_end` | `span.byte_range.clone().unwrap()` |
| `item.as_str()` | `manifest.get_dep_version(section, name)` |
| `Workspace::load(root)` | unchanged (`load_with_options` is opt-in) |

## [0.1.0] — initial publish baseline

- Three-tier name resolution (per-file `use`, cross-file module trees,
  `pub use` chains).
- `cargo_metadata`-based workspace discovery.
- Token-level and structured macro-body reference extraction.
- `quote!` and `dioxus::rsx!` built-in plugins.
- `dioxus` feature flag (default-on) makes the Dioxus dep optional.
