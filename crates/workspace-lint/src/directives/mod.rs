//! Parse `workspace_lint::allow!`/`expect!` macro invocations from Rust files
//! and `# workspace-lint: allow(...)`/`expect(...)` comments from TOML and
//! Markdown. Rust files additionally accept a **line-comment** directive form
//! (`// workspace-lint: allow|expect(...)`) so an item-level finding can be
//! silenced without depending on the `workspace-lint-marker` crate — this is
//! the form `--fix` writes when deep verification disproves a finding.
//!
//! Each directive becomes a [`Directive`] entry in the
//! [`crate::suppress::SuppressionMap`]. Diagnostics whose lint name and
//! anchor are contained by a directive's anchor are suppressed before being
//! rendered.

use crate::diagnostic::SilenceAnchor;
use fs_err as fs;
use ignore::WalkBuilder;
use regex::Regex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use wl_engine::fast::FastModel;

/// One parsed suppression directive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Directive {
    pub kind: DirectiveKind,
    /// Kebab-case short lint name (`file-size`, `unused-pub`, …). The map
    /// stores diagnostics keyed by the same kebab form.
    pub lint: String,
    pub anchor: SilenceAnchor,
    /// Where the directive itself lives — used for the stale-expect
    /// diagnostic's span.
    pub origin: DirectiveOrigin,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DirectiveKind {
    Allow,
    Expect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DirectiveOrigin {
    pub file: PathBuf,
    pub line: u32,
}

/// Scan the workspace for directives. Walks every file ignoring git-ignored
/// paths via the `ignore` crate. Per file:
///
/// - `.rs` → parse with syn for `allow!`/`expect!` macro invocations, AND
///   line-scan for `// workspace-lint: allow|expect(...)` comment directives.
/// - `.toml`, `.md` → line-scan for `# workspace-lint: allow|expect(...)`.
///
/// The anchor of each parsed directive depends on the file kind:
///
/// - `.rs` macro invocation: [`SilenceAnchor::File`] (plus a [`SilenceAnchor::Line`]
///   when it immediately precedes an item).
/// - `.rs` comment directive: [`SilenceAnchor::Line`] at the comment's own line.
/// - `Cargo.toml` line directive: [`SilenceAnchor::Line`] anchored at the
///   line *after* the comment (1-3 line lookback when matching).
/// - Other TOML/MD: [`SilenceAnchor::File`].
///
/// Workspace-wide directive scan, parsing every `.rs` file on demand. Use
/// [`scan_with_model`] in production code paths — it parses each file
/// known to the fast tier's module walk once, up-front, and reuses the cache.
pub(crate) fn scan(root: &Path) -> Vec<Directive> {
    scan_inner(root, &HashMap::new())
}

/// Same as [`scan`], but pre-parses every `.rs` file the fast tier's module
/// walk reached (deduped by canonical path) so the directive walk doesn't pay
/// a second parse per file. Files outside the walk's reach fall back to
/// on-demand parsing inside [`scan_inner`].
pub(crate) fn scan_with_model(fast: &FastModel) -> Vec<Directive> {
    let lookup = build_parsed_lookup(fast);
    scan_inner(fast.root(), &lookup)
}

fn scan_inner(root: &Path, parsed_lookup: &HashMap<PathBuf, syn::File>) -> Vec<Directive> {
    let mut directives = Vec::new();
    for entry in WalkBuilder::new(root).build().flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
        match kind_for(&rel) {
            FileKind::Rust => {
                scan_rust(path, &rel, parsed_lookup, &mut directives);
                scan_rust_comments(path, &rel, &mut directives);
            }
            FileKind::TomlOrMd => scan_text(path, &rel, &mut directives),
            FileKind::Skip => {}
        }
    }
    directives
}

/// Build a map from canonical absolute file path → `syn::File` by walking
/// every workspace member's modules and parsing each unique backing file
/// once via `FastModel::parse_file`. Inline `mod foo { ... }` submodules
/// don't contribute a new file (they carry their parent's, deduped by the
/// canonical-path key). Files that fail to parse are silently skipped — the
/// directive scan will hit them via the on-demand fallback path and surface
/// the same failure shape it always did.
fn build_parsed_lookup(fast: &FastModel) -> HashMap<PathBuf, syn::File> {
    let mut map = HashMap::new();
    for krate in fast.members() {
        for module in krate.all_modules() {
            let file = &module.file;
            let key = file.canonicalize().unwrap_or_else(|_| file.clone());
            if map.contains_key(&key) {
                continue;
            }
            if let Ok(parsed) = fast.parse_file(file) {
                map.insert(key, parsed);
            }
        }
    }
    map
}

#[derive(Copy, Clone)]
enum FileKind {
    Rust,
    TomlOrMd,
    Skip,
}

fn kind_for(rel: &Path) -> FileKind {
    match rel.extension().and_then(|e| e.to_str()) {
        Some("rs") => FileKind::Rust,
        Some("toml") | Some("md") => FileKind::TomlOrMd,
        _ => {
            // Files literally named Cargo.toml without extension are still
            // covered by the `toml` arm. CLAUDE.md / README.md etc. ditto.
            FileKind::Skip
        }
    }
}

fn scan_rust(
    abs_path: &Path,
    rel: &Path,
    parsed_lookup: &HashMap<PathBuf, syn::File>,
    out: &mut Vec<Directive>,
) {
    // Fast path: the resolver already parsed this file. Lookup by the
    // canonicalized absolute path; the lookup keys are pre-canonicalized.
    let canon = abs_path.canonicalize();
    if let Ok(canon) = &canon
        && let Some(file) = parsed_lookup.get(canon)
    {
        walk_items(&file.items, rel, out);
        return;
    }
    // Fall back to on-demand parsing for orphan / non-member files and
    // for callers (like tests) that don't pass a `Workspace`.
    let Ok(source) = fs::read_to_string(abs_path) else {
        return;
    };
    let Ok(file) = syn::parse_file(&source) else {
        return;
    };
    walk_items(&file.items, rel, out);
}

/// Line-scan a `.rs` file for `// workspace-lint: allow|expect(...)` comment
/// directives, each emitting a [`SilenceAnchor::Line`] at the comment's own
/// line. This is the marker-crate-free way to silence an item-level finding
/// (`unused-pub`): write the comment immediately above the item
/// and the suppression lookback (up to `LOOKBACK_FORWARD` lines, see
/// [`crate::suppress`]) binds it to the finding below. It's the form `--fix`
/// writes for a deep-verification-disproved finding.
///
/// Deliberately narrow to avoid false suppression: the directive text must
/// *start* the line (after the `//`), so doc comments (`///`, `//!` both leave
/// a leading non-`//` char after one strip) and trailing comments after code
/// (`x = 1; // …` doesn't start with `//`) never match. The one accepted
/// blind spot — a line *inside a multi-line string literal* that happens to
/// start with `// workspace-lint:` — mirrors the TOML scanner's, and only ever
/// over-*suppresses* (FP-safe).
fn scan_rust_comments(abs_path: &Path, rel: &Path, out: &mut Vec<Directive>) {
    let Ok(content) = fs::read_to_string(abs_path) else {
        return;
    };
    let re = directive_regex();
    for (idx, raw) in content.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        // A single `//` strip leaves `///`/`//!` with a leading `/`/`!`, which
        // the `^\s*workspace-lint:` regex rejects — so doc comments are out.
        let body = raw.trim_start().trim_start_matches("//").trim();
        let Some(caps) = re.captures(body) else {
            continue;
        };
        let kind = match &caps[1] {
            "allow" => DirectiveKind::Allow,
            "expect" => DirectiveKind::Expect,
            _ => continue,
        };
        for lint in caps[2].split(',').map(str::trim) {
            if lint.is_empty() {
                continue;
            }
            out.push(Directive {
                kind,
                lint: lint.to_string(),
                anchor: SilenceAnchor::Line {
                    file: rel.to_path_buf(),
                    line: line_no,
                },
                origin: DirectiveOrigin {
                    file: rel.to_path_buf(),
                    line: line_no,
                },
            });
        }
    }
}

/// Walk a sequence of top-level items, emitting directives with the right
/// anchor grain. Every Rust directive emits a [`SilenceAnchor::File`]
/// (preserves the original semantics — a directive at the top of a file
/// suppresses file-level findings like `file-size`); when the directive is
/// *immediately* followed by a non-directive item (function, struct, …),
/// it ALSO emits a [`SilenceAnchor::Line`] at the followed item's first
/// line, so item-targeted findings like `unused-pub` get the more precise
/// scope. Both anchors share the same `origin`, so the
/// stale-expect dedup collapses them back into one source-level directive.
fn walk_items(items: &[syn::Item], rel: &Path, out: &mut Vec<Directive>) {
    let mut i = 0;
    while i < items.len() {
        match &items[i] {
            syn::Item::Macro(item_macro)
                if let Some((kind, idents)) = parse_workspace_lint_directive(&item_macro.mac) =>
            {
                let line = item_macro.mac.path.segments[0].ident.span().start().line as u32;
                // Step past consecutive directive macros to find the next
                // non-directive item (if any).
                let mut next = i + 1;
                while next < items.len() {
                    if let syn::Item::Macro(m) = &items[next]
                        && parse_workspace_lint_directive(&m.mac).is_some()
                    {
                        next += 1;
                        continue;
                    }
                    break;
                }
                let mut anchors = vec![SilenceAnchor::File {
                    file: rel.to_path_buf(),
                }];
                if let Some(item_line) = items.get(next).and_then(item_anchor_line) {
                    anchors.push(SilenceAnchor::Line {
                        file: rel.to_path_buf(),
                        line: item_line,
                    });
                }
                for ident in idents {
                    let lint = ident.to_string().replace('_', "-");
                    for anchor in &anchors {
                        out.push(Directive {
                            kind,
                            lint: lint.clone(),
                            anchor: anchor.clone(),
                            origin: DirectiveOrigin {
                                file: rel.to_path_buf(),
                                line: line.max(1),
                            },
                        });
                    }
                }
            }
            syn::Item::Mod(item_mod) => {
                if let Some((_, inner)) = &item_mod.content {
                    walk_items(inner, rel, out);
                }
            }
            _ => {}
        }
        i += 1;
    }
}

fn parse_workspace_lint_directive(
    mac: &syn::Macro,
) -> Option<(
    DirectiveKind,
    syn::punctuated::Punctuated<syn::Ident, syn::Token![,]>,
)> {
    let last = mac.path.segments.last()?.ident.to_string();
    let kind = match last.as_str() {
        "allow" => DirectiveKind::Allow,
        "expect" => DirectiveKind::Expect,
        _ => return None,
    };
    let path_str = mac
        .path
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::");
    if !path_str.starts_with("workspace_lint") && path_str != "allow" && path_str != "expect" {
        return None;
    }
    let parsed: Result<syn::punctuated::Punctuated<syn::Ident, syn::Token![,]>, _> =
        mac.parse_body_with(syn::punctuated::Punctuated::parse_terminated);
    parsed.ok().map(|idents| (kind, idents))
}

/// Return the start line of a syn::Item if we can compute it (covers the
/// kinds users would meaningfully prefix with a directive). Returns `None`
/// for items whose syn type doesn't expose an ident with a span.
fn item_anchor_line(item: &syn::Item) -> Option<u32> {
    use syn::spanned::Spanned;
    Some(match item {
        syn::Item::Fn(i) => i.sig.ident.span().start().line as u32,
        syn::Item::Struct(i) => i.ident.span().start().line as u32,
        syn::Item::Enum(i) => i.ident.span().start().line as u32,
        syn::Item::Union(i) => i.ident.span().start().line as u32,
        syn::Item::Trait(i) => i.ident.span().start().line as u32,
        syn::Item::Type(i) => i.ident.span().start().line as u32,
        syn::Item::Const(i) => i.ident.span().start().line as u32,
        syn::Item::Static(i) => i.ident.span().start().line as u32,
        syn::Item::Mod(i) => i.ident.span().start().line as u32,
        syn::Item::Impl(i) => i.span().start().line as u32,
        _ => return None,
    })
}

fn scan_text(abs_path: &Path, rel: &Path, out: &mut Vec<Directive>) {
    let Ok(content) = fs::read_to_string(abs_path) else {
        return;
    };
    let re = directive_regex();
    for (idx, raw) in content.lines().enumerate() {
        let line_no = (idx + 1) as u32;
        // Trim the leading comment marker if any.
        let body = raw
            .trim_start()
            .trim_start_matches('#')
            .trim_start_matches("//")
            .trim_start_matches("<!--")
            .trim();
        let Some(caps) = re.captures(body) else {
            continue;
        };
        let kind = match &caps[1] {
            "allow" => DirectiveKind::Allow,
            "expect" => DirectiveKind::Expect,
            _ => continue,
        };
        let lints: Vec<&str> = caps[2].split(',').map(str::trim).collect();
        // For TOML the natural grain is the dep line right below the comment;
        // additionally, a comment anywhere in a `<crate>/Cargo.toml` should
        // suppress diagnostics anchored at that crate (centralized-deps,
        // unused-deps). For everything else the directive covers the file.
        let is_cargo_toml = rel.file_name().and_then(|n| n.to_str()) == Some("Cargo.toml");
        let toml_like = rel.extension().and_then(|e| e.to_str()) == Some("toml");
        let mut anchors: Vec<SilenceAnchor> = Vec::new();
        if toml_like {
            anchors.push(SilenceAnchor::Line {
                file: rel.to_path_buf(),
                line: line_no,
            });
        } else {
            anchors.push(SilenceAnchor::File {
                file: rel.to_path_buf(),
            });
        }
        if is_cargo_toml && let Some(parent) = rel.parent() {
            anchors.push(SilenceAnchor::Crate {
                manifest_dir: parent.to_path_buf(),
            });
        }

        for lint in lints {
            if lint.is_empty() {
                continue;
            }
            for anchor in &anchors {
                out.push(Directive {
                    kind,
                    lint: lint.to_string(),
                    anchor: anchor.clone(),
                    origin: DirectiveOrigin {
                        file: rel.to_path_buf(),
                        line: line_no,
                    },
                });
            }
        }
    }
}

fn directive_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^\s*workspace-lint:\s*(allow|expect)\(([^)]*)\)").expect("static regex")
    })
}

#[cfg(test)]
mod tests;
