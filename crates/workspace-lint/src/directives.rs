//! Parse `workspace_lint::allow!`/`expect!` macro invocations from Rust files
//! and `# workspace-lint: allow(...)`/`expect(...)` comments from TOML and
//! Markdown.
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
use syn_workspace::Workspace;

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
/// - `.rs` → parse with syn for `allow!`/`expect!` macro invocations.
/// - `.toml`, `.md` → line-scan for `# workspace-lint: allow|expect(...)`.
///
/// The anchor of each parsed directive depends on the file kind:
///
/// - `.rs` file-level: [`SilenceAnchor::File`].
/// - `Cargo.toml` line directive: [`SilenceAnchor::Line`] anchored at the
///   line *after* the comment (1-3 line lookback when matching).
/// - Other TOML/MD: [`SilenceAnchor::File`].
///
/// Workspace-wide directive scan, parsing every `.rs` file on demand. Use
/// [`scan_with_workspace`] in production code paths — it parses each file
/// known to the resolver once, up-front, and reuses the cache.
pub(crate) fn scan(root: &Path) -> Vec<Directive> {
    scan_inner(root, &HashMap::new())
}

/// Same as [`scan`], but pre-parses every `.rs` file the resolver reached
/// (deduped by canonical path) so the directive walk doesn't pay a second
/// parse per file. Files outside the resolver's reach fall back to
/// on-demand parsing inside [`scan_inner`].
pub(crate) fn scan_with_workspace(workspace: &Workspace) -> Vec<Directive> {
    let lookup = build_parsed_lookup(workspace);
    scan_inner(workspace.root(), &lookup)
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
            FileKind::Rust => scan_rust(path, &rel, parsed_lookup, &mut directives),
            FileKind::TomlOrMd => scan_text(path, &rel, &mut directives),
            FileKind::Skip => {}
        }
    }
    directives
}

/// Build a map from canonical absolute file path → `syn::File` by walking
/// every workspace member's modules and parsing each unique backing file
/// once via `Workspace::parse_file`. Inline `mod foo { ... }` submodules
/// don't contribute (their content lives in the parent's file). Files
/// that fail to parse are silently skipped — the directive scan will hit
/// them via the on-demand fallback path and surface the same failure
/// shape it always did.
fn build_parsed_lookup(workspace: &Workspace) -> HashMap<PathBuf, syn::File> {
    let mut map = HashMap::new();
    for krate in workspace.members() {
        for module in krate.all_modules() {
            let Some(file) = &module.file else { continue };
            let key = file.canonicalize().unwrap_or_else(|_| file.clone());
            if map.contains_key(&key) {
                continue;
            }
            if let Ok(parsed) = workspace.parse_file(file) {
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

/// Walk a sequence of top-level items, emitting directives with the right
/// anchor grain. Every Rust directive emits a [`SilenceAnchor::File`]
/// (preserves the original semantics — a directive at the top of a file
/// suppresses file-level findings like `file-size`); when the directive is
/// *immediately* followed by a non-directive item (function, struct, …),
/// it ALSO emits a [`SilenceAnchor::Line`] at the followed item's first
/// line, so item-targeted findings like `visibility` and `unused-pub` get
/// the more precise scope. Both anchors share the same `origin`, so the
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
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(dir: &Path, name: &str, content: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    // --- Rust file macros ---

    #[test]
    fn parses_workspace_lint_allow_invocation() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "workspace_lint::allow!(file_size);\n",
        );
        let directives = scan(tmp.path());
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].lint, "file-size");
        assert_eq!(directives[0].kind, DirectiveKind::Allow);
        match &directives[0].anchor {
            SilenceAnchor::File { file } => assert_eq!(file, &PathBuf::from("src/lib.rs")),
            other => panic!("expected File anchor, got {other:?}"),
        }
    }

    #[test]
    fn parses_workspace_lint_expect_invocation() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "workspace_lint::expect!(unused_pub);\n",
        );
        let directives = scan(tmp.path());
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].kind, DirectiveKind::Expect);
        assert_eq!(directives[0].lint, "unused-pub");
    }

    #[test]
    fn parses_comma_separated_lint_list() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "workspace_lint::allow!(file_size, unused_pub, centralized_deps);\n",
        );
        let directives = scan(tmp.path());
        let mut lints: Vec<&str> = directives.iter().map(|d| d.lint.as_str()).collect();
        lints.sort();
        assert_eq!(lints, ["centralized-deps", "file-size", "unused-pub"]);
    }

    #[test]
    fn parses_unqualified_allow_after_use() {
        // Accept `allow!(...)` when the file presumably did `use workspace_lint::*`.
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "src/lib.rs", "allow!(file_size);\n");
        let directives = scan(tmp.path());
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].lint, "file-size");
    }

    #[test]
    fn ignores_other_macros() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "println!(\"hi\");\nvec![1,2,3];\nformat!(\"x\");\n",
        );
        assert!(scan(tmp.path()).is_empty());
    }

    #[test]
    fn ignores_malformed_macros() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "workspace_lint::allow!(\"not an ident\");\n",
        );
        assert!(scan(tmp.path()).is_empty());
    }

    // --- TOML comment directives ---

    #[test]
    fn parses_toml_comment_directive_emits_line_and_crate_anchors() {
        // For a Cargo.toml directive we emit two anchors (Line at the comment
        // line, Crate at the parent dir) so the comment naturally silences
        // either a per-line diagnostic on a nearby dep line or a crate-level
        // diagnostic like centralized-deps anchored at the manifest dir.
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "crates/foo/Cargo.toml",
            "[package]\nname = \"foo\"\n\n# workspace-lint: allow(unused-deps)\n[dependencies]\nfoo = \"1\"\n",
        );
        let directives = scan(tmp.path());
        assert_eq!(directives.len(), 2);
        assert!(directives.iter().any(|d| matches!(
            &d.anchor,
            SilenceAnchor::Line { file, line }
                if file == &PathBuf::from("crates/foo/Cargo.toml") && *line == 4,
        )));
        assert!(directives.iter().any(|d| matches!(
            &d.anchor,
            SilenceAnchor::Crate { manifest_dir }
                if manifest_dir == &PathBuf::from("crates/foo"),
        )));
        assert!(directives.iter().all(|d| d.lint == "unused-deps"));
    }

    #[test]
    fn parses_toml_comma_list() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "non-cargo.toml",
            "# workspace-lint: allow(unused-deps, centralized-deps)\n",
        );
        let directives = scan(tmp.path());
        // Non-Cargo.toml files only emit one anchor per lint (Line).
        let mut lints: Vec<&str> = directives.iter().map(|d| d.lint.as_str()).collect();
        lints.sort();
        assert_eq!(lints, ["centralized-deps", "unused-deps"]);
    }

    #[test]
    fn parses_md_comment_directive() {
        let tmp = TempDir::new().unwrap();
        // `// workspace-lint: ...` style in Markdown also accepted.
        write(
            tmp.path(),
            "README.md",
            "Some text.\n// workspace-lint: allow(freshness)\nMore text.\n",
        );
        let directives = scan(tmp.path());
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].lint, "freshness");
        match &directives[0].anchor {
            SilenceAnchor::File { file } => assert_eq!(file, &PathBuf::from("README.md")),
            other => panic!("expected File anchor, got {other:?}"),
        }
    }

    #[test]
    fn parses_html_comment_directive() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "README.md",
            "<!-- workspace-lint: expect(freshness) -->\n",
        );
        let directives = scan(tmp.path());
        assert_eq!(directives.len(), 1);
        assert_eq!(directives[0].kind, DirectiveKind::Expect);
        assert_eq!(directives[0].lint, "freshness");
    }

    #[test]
    fn ignores_unrelated_toml_comments() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "Cargo.toml",
            "# this is just a comment\n# TODO: something\n",
        );
        assert!(scan(tmp.path()).is_empty());
    }

    // --- Origin info ---

    #[test]
    fn origin_records_file_and_line() {
        let tmp = TempDir::new().unwrap();
        write(
            tmp.path(),
            "src/lib.rs",
            "\n\nworkspace_lint::allow!(file_size);\n",
        );
        let directives = scan(tmp.path());
        assert_eq!(directives[0].origin.file, PathBuf::from("src/lib.rs"));
        assert_eq!(directives[0].origin.line, 3);
    }

    #[test]
    fn skips_non_text_extensions() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "data.bin", "anything goes here");
        assert!(scan(tmp.path()).is_empty());
    }

    // --- item_anchor_line ---

    fn parse_one(src: &str) -> syn::Item {
        let file: syn::File = syn::parse_str(src).unwrap();
        file.items.into_iter().next().unwrap()
    }

    #[test]
    fn item_anchor_line_for_fn_points_at_ident() {
        // `pub fn foo()` is on line 2 → the `foo` ident lives on line 2.
        let item = parse_one("\npub fn foo() {}\n");
        assert_eq!(item_anchor_line(&item), Some(2));
    }

    #[test]
    fn item_anchor_line_for_struct_enum_union() {
        assert_eq!(item_anchor_line(&parse_one("\nstruct S;")), Some(2));
        assert_eq!(item_anchor_line(&parse_one("\n\nenum E { A, B }")), Some(3));
        assert_eq!(item_anchor_line(&parse_one("union U { x: u8 }")), Some(1));
    }

    #[test]
    fn item_anchor_line_for_trait_type_const_static() {
        assert_eq!(item_anchor_line(&parse_one("trait T {}")), Some(1));
        assert_eq!(item_anchor_line(&parse_one("\ntype A = u8;")), Some(2));
        assert_eq!(item_anchor_line(&parse_one("const C: u8 = 1;")), Some(1));
        assert_eq!(item_anchor_line(&parse_one("static X: u8 = 1;")), Some(1));
    }

    #[test]
    fn item_anchor_line_for_mod_and_impl() {
        assert_eq!(item_anchor_line(&parse_one("\nmod m {}")), Some(2));
        // For Impl, the anchor is the `impl` keyword line, not an ident.
        assert_eq!(
            item_anchor_line(&parse_one("\nstruct S;\nimpl S {}")),
            Some(2),
        );
    }

    #[test]
    fn item_anchor_line_none_for_unsupported_kinds() {
        // syn::Item::Use is not one of the matched arms.
        assert_eq!(item_anchor_line(&parse_one("use std::io;")), None);
        // syn::Item::ExternCrate, ForeignMod, Macro etc. → also None.
        assert_eq!(item_anchor_line(&parse_one("extern crate foo;")), None);
    }
}
