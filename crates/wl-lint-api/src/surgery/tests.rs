//! Import-surgery byte math: brace-leaf excision, whole-statement deletion,
//! and the coalescing that keeps adjacent dead leaves from producing
//! overlapping (file-aborting) suggestions. Each case applies the computed
//! deletions to real source and asserts the result compiles-shaped.

use super::*;
use tempfile::TempDir;
use wl_engine::wl_ir::Span as IrSpan;

/// A `DanglingImport` whose spans point at real byte ranges in `src`. `leaf` is
/// the leaf text (must occur once); `stmt` is the whole `use …;` (its own text
/// for a standalone import, or the same as `leaf` for a brace-list leaf, which
/// is how the extractor collapses it).
fn dangling(src: &str, leaf: &str, stmt: &str) -> DanglingImport {
    let elo = src.find(leaf).expect("leaf in src") as u32;
    let elem = IrSpan {
        file: "src/lib.rs".into(),
        lo: elo,
        hi: elo + leaf.len() as u32,
        line: 1,
        from_expansion: false,
    };
    let slo = src.find(stmt).expect("stmt in src") as u32;
    let decl = IrSpan {
        lo: slo,
        hi: slo + stmt.len() as u32,
        ..elem.clone()
    };
    DanglingImport {
        decl,
        elem,
        reexport: false,
        glob: false,
    }
}

/// A glob `DanglingImport`: `stmt` is the whole `use m::*;` text (must occur
/// once); `decl == elem` with the `glob` discriminator set, exactly as the
/// engine emits it.
fn dangling_glob(src: &str, stmt: &str) -> DanglingImport {
    let slo = src.find(stmt).expect("stmt in src") as u32;
    let decl = IrSpan {
        file: "src/lib.rs".into(),
        lo: slo,
        hi: slo + stmt.len() as u32,
        line: 1,
        from_expansion: false,
    };
    DanglingImport {
        elem: decl.clone(),
        decl,
        reexport: false,
        glob: true,
    }
}

/// Run `import_surgery` over `src` and apply every emitted deletion (descending
/// byte order, so earlier offsets stay valid) — the same replace `fix::run`
/// performs.
fn apply(src: &str, dangling: Vec<DanglingImport>) -> String {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/lib.rs"), src).unwrap();
    let diags = import_surgery(dangling, tmp.path());
    let mut spans: Vec<(u32, u32)> = diags
        .iter()
        .flat_map(|d| &d.suggestions)
        .map(|s| (s.span.byte_start, s.span.byte_end))
        .collect();
    spans.sort_by_key(|(lo, _)| std::cmp::Reverse(*lo));
    let mut out = src.to_string();
    for (lo, hi) in spans {
        out.replace_range(lo as usize..hi as usize, "");
    }
    out
}

#[test]
fn standalone_import_deletes_whole_statement() {
    let src = "use a::b;\nuse a::c;\n";
    // `b` is removed; the whole `use a::b;` (decl ⊋ elem) plus its newline go.
    let out = apply(src, vec![dangling(src, "b;", "use a::b;")]);
    assert_eq!(out, "use a::c;\n");
}

#[test]
fn brace_leaf_first_removes_leaf_and_trailing_separator() {
    let src = "use a::{b, c};\n";
    // Brace leaf: decl == elem == "b".
    let out = apply(src, vec![dangling(src, "b", "b")]);
    assert_eq!(out, "use a::{c};\n");
}

#[test]
fn brace_leaf_last_removes_leaf_and_leading_separator() {
    let src = "use a::{b, c};\n";
    let out = apply(src, vec![dangling(src, "c", "c")]);
    assert_eq!(out, "use a::{b};\n");
}

#[test]
fn brace_all_leaves_dead_deletes_whole_statement() {
    let src = "use a::{b, c};\nuse a::d;\n";
    // Both dead: the two separator ranges coalesce, the emptied group
    // collapses, and the whole statement goes — no `use a::{};` residue.
    let out = apply(src, vec![dangling(src, "b", "b"), dangling(src, "c", "c")]);
    assert_eq!(out, "use a::d;\n");
}

#[test]
fn brace_middle_leaf_of_three() {
    let src = "use a::{b, c, d};\n";
    let out = apply(src, vec![dangling(src, "c", "c")]);
    assert_eq!(out, "use a::{b, d};\n");
}

#[test]
fn sole_brace_leaf_deletes_whole_statement() {
    let src = "use a::{b};\nuse a::c;\n";
    let out = apply(src, vec![dangling(src, "b", "b")]);
    assert_eq!(out, "use a::c;\n");
}

#[test]
fn emptied_nested_group_is_excised_from_its_parent() {
    let src = "use a::{b::{c}, d};\n";
    // `c` dead empties `b::{c}` — the whole entry (and its separator) goes,
    // leaving the live sibling.
    let out = apply(src, vec![dangling(src, "c", "c")]);
    assert_eq!(out, "use a::{d};\n");
}

#[test]
fn sibling_groups_emptying_together_collapse_the_statement() {
    let src = "use a::{b::{cc}, d::{ee}};\nfn keep() {}\n";
    let out = apply(
        src,
        vec![dangling(src, "cc", "cc"), dangling(src, "ee", "ee")],
    );
    assert_eq!(out, "fn keep() {}\n");
}

#[test]
fn collapsed_statement_takes_visibility_and_attributes() {
    let src = "#[cfg(feature = \"x\")]\npub(crate) use a::{bb};\nfn keep() {}\n";
    let out = apply(src, vec![dangling(src, "bb", "bb")]);
    assert_eq!(out, "fn keep() {}\n");
}

#[test]
fn multiline_group_with_trailing_comma_collapses() {
    let src = "use a::{\n    bb,\n    cc,\n};\nuse a::d;\n";
    // The two dead leaves never touch (a live newline+indent between them),
    // so widening must judge emptiness against the union of ranges.
    let out = apply(
        src,
        vec![dangling(src, "bb", "bb"), dangling(src, "cc", "cc")],
    );
    assert_eq!(out, "use a::d;\n");
}

#[test]
fn comment_inside_group_bails_to_empty_group_residue() {
    let src = "use a::{bb /* why */};\n";
    // The scanner doesn't model comments — safe bail leaves the `{}` shell
    // rather than risking a wrong statement deletion.
    let out = apply(src, vec![dangling(src, "bb", "bb")]);
    assert_eq!(out, "use a::{ /* why */};\n");
}

#[test]
fn standalone_import_takes_preceding_attribute() {
    let src = "#[cfg(test)]\nuse a::b;\nuse a::c;\n";
    let out = apply(src, vec![dangling(src, "b;", "use a::b;")]);
    assert_eq!(out, "use a::c;\n");
}

#[test]
fn crlf_standalone_deletes_both_newline_bytes() {
    let src = "use a::b;\r\nuse a::c;\r\n";
    let out = apply(src, vec![dangling(src, "b;", "use a::b;")]);
    assert_eq!(out, "use a::c;\r\n");
}

#[test]
fn macro_generated_and_reexport_imports_are_skipped() {
    let src = "use a::{b, c};\n";
    let mut mac = dangling(src, "b", "b");
    mac.decl.from_expansion = true;
    let mut rex = dangling(src, "c", "c");
    rex.reexport = true;
    // Neither is excisable → no deletions emitted, source untouched.
    let out = apply(src, vec![mac, rex]);
    assert_eq!(out, src);
}

#[test]
fn glob_import_deletes_whole_statement() {
    let src = "use dioxus::prelude::*;\nuse a::c;\n";
    let out = apply(src, vec![dangling_glob(src, "use dioxus::prelude::*;")]);
    assert_eq!(out, "use a::c;\n");
}

#[test]
fn glob_import_takes_preceding_attribute_and_blank_line() {
    let src = "#[cfg(feature = \"web\")]\nuse dioxus::prelude::*;\n\nfn keep() {}\n";
    let out = apply(src, vec![dangling_glob(src, "use dioxus::prelude::*;")]);
    assert_eq!(out, "fn keep() {}\n");
}

#[test]
fn nested_list_glob_bails_to_residue() {
    // A nested-list glob's decl span is the collapsed leaf (`inner::*`), not a
    // statement — the byte scanner must bail rather than delete the wrong bytes.
    let src = "use a::{Gadget, inner::*};\n";
    let mut d = dangling_glob(src, "inner::*");
    d.decl.hi = d.decl.lo + "inner::*".len() as u32;
    let out = apply(src, vec![d]);
    assert_eq!(out, src, "non-statement-shaped glob decl: untouched");
}

// --- whole-item deletion (`deletion`): the byte math the tests above build
//     on, exercised directly. Moved here with the module from
//     `wl-lints::unused_pub` (they pin its behavior, not the lint's). ---

use wl_diagnostic::PubVerdict;

fn ir_span(lo: u32, hi: u32) -> IrSpan {
    IrSpan {
        file: "src/lib.rs".into(),
        lo,
        hi,
        line: 1,
        from_expansion: false,
    }
}

#[test]
fn delete_suggestion_unavailable_when_file_missing() {
    let missing = std::path::PathBuf::from("/nonexistent/definitely/not/here.rs");
    assert!(matches!(
        deletion::delete_suggestion(&missing, &ir_span(0, 10)),
        deletion::DeleteOutcome::Unavailable
    ));
}

#[test]
fn delete_suggestion_unavailable_when_start_ge_end() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "pub fn gone() {}\n").unwrap();
    assert!(matches!(
        deletion::delete_suggestion(&file, &ir_span(10, 10)),
        deletion::DeleteOutcome::Unavailable
    ));
}

#[test]
fn delete_suggestion_skips_untracked_file_and_eats_trailing_newline() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "pub fn gone() {}\nrest").unwrap();
    match deletion::delete_suggestion(&file, &ir_span(0, 16)) {
        deletion::DeleteOutcome::Skip(s, reason) => {
            assert_eq!(s.span.byte_start, 0);
            assert_eq!(s.span.byte_end, 17, "deletion eats the trailing newline");
            assert_eq!(s.original.as_deref(), Some("pub fn gone() {}"));
            assert!(reason.contains("untracked or has uncommitted changes"));
        }
        other => panic!(
            "expected Skip for an untracked file, got {}",
            outcome(&other)
        ),
    }
}

fn outcome(o: &deletion::DeleteOutcome) -> &'static str {
    match o {
        deletion::DeleteOutcome::Apply(_) => "Apply",
        deletion::DeleteOutcome::Skip(..) => "Skip",
        deletion::DeleteOutcome::Unavailable => "Unavailable",
    }
}

// --- deletion surface: preceding attributes (LeaveDates 2026-07-05) ---
// `full_span` can't see cfg-stripped or macro-consumed attributes; the
// deletion extends over them lexically or they'd be orphaned onto the next
// item (syntax error before `}`, silent re-target otherwise).

/// Extension measured from the item-start offset within `src` (the `pub`).
fn extended_start(src: &str) -> usize {
    deletion::extend_over_preceding_attrs(src, src.find("pub fn").unwrap())
}

#[test]
fn attr_extension_single_attr() {
    let src = "#[cfg(test)]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), 0);
}

#[test]
fn attr_extension_stacked_attrs_and_blank_line() {
    let src = "fn keep() {}\n\n#[inline]\n#[cfg(test)]\n\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("#[inline]").unwrap());
}

#[test]
fn attr_extension_multiline_attr() {
    let src = "fn keep() {}\n#[tracing::instrument(\n    level = \"debug\",\n    skip(x),\n)]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("#[tracing").unwrap());
}

#[test]
fn attr_extension_bracket_inside_string() {
    let src = "fn keep() {}\n#[doc = \"has ] and [ inside\"]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("#[doc").unwrap());
}

#[test]
fn attr_extension_escaped_quote_inside_string() {
    let src = "fn keep() {}\n#[doc = \"say \\\"]hi\\\"\"]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("#[doc").unwrap());
}

#[test]
fn attr_extension_never_consumes_inner_attr() {
    let src = "#![allow(dead_code)]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("pub fn").unwrap());
}

#[test]
fn attr_extension_stops_at_plain_code() {
    let src = "fn keep() {}\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("pub fn").unwrap());
}

#[test]
fn attr_extension_consumes_doc_lines() {
    let src = "fn keep() {}\n/// docs\n#[cfg(test)]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("/// docs").unwrap());
}

#[test]
fn attr_extension_bails_on_raw_string_fence() {
    // `r#"…"#` breaks the backward bracket math — no extension, attr stays
    // (today's behavior; never a wrong wider deletion).
    let src = "fn keep() {}\n#[doc = r#\"raw ] here\"#]\npub fn gone() {}\n";
    assert_eq!(extended_start(src), src.find("pub fn").unwrap());
}

#[test]
fn blank_line_eating_below_item() {
    let src = b"fn a() {}\n\n\nfn b() {}\n";
    // `end` sits after "fn a() {}\n" — the two blank lines are consumed.
    assert_eq!(lines::eat_blank_lines(src, 10), 12);
    // No blanks → unchanged.
    assert_eq!(lines::eat_blank_lines(b"fn a() {}\nfn b() {}\n", 10), 10);
}

#[test]
fn delete_suggestion_includes_attrs_and_blank_lines() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    let src = "fn keep() {}\n\n#[cfg(test)]\npub fn gone() {}\n\nfn tail() {}\n";
    std::fs::write(&file, src).unwrap();
    let lo = src.find("pub fn").unwrap() as u32;
    let hi = (src.find("gone() {}").unwrap() + "gone() {}".len()) as u32;
    match deletion::delete_suggestion(&file, &ir_span(lo, hi)) {
        deletion::DeleteOutcome::Skip(s, _) => {
            let mut fixed = src.to_string();
            fixed.replace_range(s.span.byte_start as usize..s.span.byte_end as usize, "");
            assert_eq!(
                fixed, "fn keep() {}\n\nfn tail() {}\n",
                "attr deleted with the item, one blank separator survives"
            );
        }
        other => panic!("expected Skip, got {}", outcome(&other)),
    }
}

#[test]
fn pick_deletion_fix_gates() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "pub fn gone() {}\n").unwrap();
    // No auto_delete opt-in → tighten fallback.
    assert!(
        deletion::pick_deletion_fix(false, &file, &ir_span(0, 16), PubVerdict::Unused).is_none()
    );
    // IntraCrate never deletes.
    assert!(
        deletion::pick_deletion_fix(true, &file, &ir_span(0, 16), PubVerdict::IntraCrate).is_none()
    );
    // Unused + opt-in on an untracked file → the Skip suggestion (with note).
    let (sugg, note) =
        deletion::pick_deletion_fix(true, &file, &ir_span(0, 16), PubVerdict::Unused)
            .expect("Some");
    assert_eq!(
        sugg.applicability,
        wl_diagnostic::Applicability::MaybeIncorrect
    );
    assert!(note.is_some());
}
