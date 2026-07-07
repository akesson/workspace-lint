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

// --- Rust line-comment directives ---

#[test]
fn parses_rust_comment_directive_as_line_anchor() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "src/lib.rs",
        "// workspace-lint: expect(unused-pub)\npub fn helper() {}\n",
    );
    let directives = scan(tmp.path());
    assert_eq!(directives.len(), 1);
    assert_eq!(directives[0].lint, "unused-pub");
    assert_eq!(directives[0].kind, DirectiveKind::Expect);
    match &directives[0].anchor {
        SilenceAnchor::Line { file, line } => {
            assert_eq!(file, &PathBuf::from("src/lib.rs"));
            assert_eq!(*line, 1, "anchor is the comment's own line");
        }
        other => panic!("expected Line anchor, got {other:?}"),
    }
    // Origin tracks the comment line for stale-expect reporting.
    assert_eq!(directives[0].origin.line, 1);
}

#[test]
fn rust_comment_directive_suppresses_item_below_via_lookback() {
    // End-to-end: the comment on line 1 must suppress an unused-pub finding
    // anchored at the item on line 2 (the suppress.rs lookback window).
    use wl_diagnostic::builder::at_line;
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "src/lib.rs",
        "// workspace-lint: expect(unused-pub)\npub fn helper() {}\n",
    );
    let directives = scan(tmp.path());
    let mut map = crate::suppress::SuppressionMap::from_directives(directives);
    let d = at_line("workspace-lint::unused-pub", "unused", "src/lib.rs", 2).build();
    assert!(map.is_suppressed(&d));
    let ran = wl_lint_api::LintId::ALL.iter().copied().collect();
    assert!(map.stale_expects(&ran, tmp.path()).is_empty());
}

#[test]
fn rust_comment_directive_comma_list() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "src/lib.rs",
        "    // workspace-lint: allow(unused-pub, file-size)\n    pub fn f() {}\n",
    );
    let mut lints: Vec<String> = scan(tmp.path()).into_iter().map(|d| d.lint).collect();
    lints.sort();
    assert_eq!(lints, ["file-size", "unused-pub"]);
}

#[test]
fn rust_doc_comment_is_not_a_directive() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "src/lib.rs",
        "/// workspace-lint: allow(unused-pub)\npub fn documented() {}\n",
    );
    assert!(
        scan(tmp.path()).is_empty(),
        "a `///` doc comment must not be parsed as a directive"
    );
}

#[test]
fn rust_trailing_comment_is_not_a_directive() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "src/lib.rs",
        "pub fn f() { let _x = 1; } // workspace-lint: allow(unused-pub)\n",
    );
    assert!(
        scan(tmp.path()).is_empty(),
        "a trailing comment after code must not be parsed as a directive"
    );
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
    // A single-line macro invocation: origin spans exactly its own line.
    assert_eq!(directives[0].origin.line_end, 3);
}

#[test]
fn comment_directive_origin_is_single_line() {
    let tmp = TempDir::new().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[deps]\n# workspace-lint: expect(unused-deps)\n",
    );
    let directives = scan(tmp.path());
    assert!(
        directives
            .iter()
            .all(|d| d.origin.line == 2 && d.origin.line_end == 2)
    );
}

#[test]
fn multiline_macro_invocation_origin_spans_start_to_end() {
    let tmp = TempDir::new().unwrap();
    // The invocation opens on line 2 and closes on line 4.
    write(
        tmp.path(),
        "src/lib.rs",
        "\nworkspace_lint::expect!(\n    unused_pub,\n);\n",
    );
    let directives = scan(tmp.path());
    assert_eq!(directives[0].origin.line, 2);
    assert_eq!(directives[0].origin.line_end, 4);
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
