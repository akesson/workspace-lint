//! Shipped-source line counting, shared by the `crate-size` and `file-size`
//! budgets.
//!
//! A size budget is about the maintained *shipped* source, not the test mass
//! that legitimately dwarfs it (in this very workspace ~40% of the binary
//! crate's `src/` is `#[cfg(test)]` unit tests, on top of whole `tests/` trees).
//! So this module counts code lines the way the lints always have — with the
//! same tokei classifier — but first removes test code:
//!
//! - **cargo dev-target directories** (`tests/`, `benches/`, `examples/`) at the
//!   crate root, which hold integration tests, benches, examples, and fixtures —
//!   never shipped. `build.rs` stays counted (it is maintained shipped source).
//!   `crate-size` knows each crate's root and calls `in_dev_target_dir`;
//!   `file-size` has no cargo metadata, so it discovers the owning crate root
//!   itself via `in_dev_target_dir_rootless`.
//! - **in-file test items**: any item gated by exactly `#[cfg(test)]`, any fn
//!   marked `#[test]` / `#[wasm_bindgen_test]`, and the sibling file an
//!   out-of-line `#[cfg(test)] mod x;` resolves to.
//!
//! syn only *locates* the test regions; tokei still counts, so a file with no
//! test code produces a byte-identical number to the previous whole-file count.
//! Every approximation here leans toward *over*-counting (treating ambiguous
//! code as shipped) so the budget is never silently loosened: `cfg(any(test,
//! …))` is not treated as test-only, and a file that fails to parse is counted
//! whole.
//!
//! One deliberate divergence from a raw tokei walk: embedded code blocks (e.g. a
//! `rust`-tagged fence inside a doc comment) are *not* counted, because the
//! per-file parse here reports only the host language's `.code`, never tokei's
//! embedded-language `children`. Both budgets share this definition, so they
//! agree to the line.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use syn::spanned::Spanned;
use syn::visit::{self, Visit};
use tokei::{Config as TokeiConfig, LanguageType};

/// First-path-component names that are cargo dev targets, never shipped.
const DEV_TARGET_DIRS: &[&str] = &["tests", "benches", "examples"];

/// True when `file` lives under a top-level cargo dev-target dir of `crate_dir`
/// (`<crate>/tests/…`, `<crate>/benches/…`, `<crate>/examples/…`). A nested
/// `src/…/tests.rs` is intentionally NOT matched here — its `#[cfg(test)]`
/// content is removed by the in-file scan instead.
pub fn in_dev_target_dir(crate_dir: &Path, file: &Path) -> bool {
    let Ok(rel) = file.strip_prefix(crate_dir) else {
        return false; // unexpected path shape — count it (never under-count).
    };
    rel.components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .is_some_and(|first| DEV_TARGET_DIRS.contains(&first))
}

/// Like [`in_dev_target_dir`], but for a file outside every workspace member
/// (an excluded package's sources matched by a broad glob): discover the
/// owning crate root by walking ancestors to the nearest one holding a
/// `Cargo.toml`, then apply the same top-level dev-target rule against it.
/// The fallback arm of `SourceMeasure::in_dev_target`.
///
/// A `.rs` file with no `Cargo.toml` ancestor (outside any crate) is treated as
/// shipped — never under-count.
pub(crate) fn in_dev_target_dir_rootless(file: &Path) -> bool {
    let mut ancestor = file.parent();
    while let Some(dir) = ancestor {
        if dir.join("Cargo.toml").is_file() {
            return in_dev_target_dir(dir, file);
        }
        ancestor = dir.parent();
    }
    false
}

/// One parsed file awaiting the counting pass: its path, full source, and the
/// 1-based test line ranges to subtract.
type ParsedFile<'a> = (&'a PathBuf, String, Vec<(usize, usize)>);

/// Sum shipped (non-test) Rust code lines across `rust_files`. Thin wrapper over
/// `shipped_lines_by_file`; `crate-size` wants one crate total.
pub fn count_rust_shipped(rust_files: &[PathBuf]) -> usize {
    shipped_lines_by_file(rust_files).values().sum()
}

/// Shipped (non-test) Rust code lines **per file**, using tokei for the
/// classification. `file-size` needs the per-file breakdown; `crate-size` sums
/// it. Out-of-line `#[cfg(test)] mod x;` target files are dropped from the map
/// entirely (they have no shipped lines); for the rest, the production lines
/// (everything outside the test ranges) are counted via `production_code`.
pub(crate) fn shipped_lines_by_file(rust_files: &[PathBuf]) -> HashMap<PathBuf, usize> {
    // Pass 1: parse each file once; record its test line ranges and collect the
    // sibling files declared as out-of-line test modules (resolvable only with
    // the whole file set in hand).
    let mut parsed: Vec<ParsedFile<'_>> = Vec::new();
    let mut test_mod_files: HashSet<PathBuf> = HashSet::new();
    for path in rust_files {
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        let dir = path.parent().unwrap_or_else(|| Path::new(""));
        let (ranges, mod_targets) = test_regions(&content, dir);
        test_mod_files.extend(mod_targets);
        parsed.push((path, content, ranges));
    }

    // Pass 2: count, skipping files resolved as out-of-line test modules.
    let cfg = TokeiConfig::default();
    parsed
        .iter()
        .filter(|(path, _, _)| !test_mod_files.contains(*path))
        .map(|(path, content, ranges)| ((*path).clone(), production_code(content, ranges, &cfg)))
        .collect()
}

/// Tokei code lines of the file's *production* source: the lines outside every
/// test range, extracted into one buffer and counted together.
///
/// Removing the test lines (rather than blanking them in place to empty lines)
/// is the load-bearing choice. tokei classifies lines with a *stateful*
/// string/comment scanner, so a `'"'`-style token — a char literal holding a
/// quote, e.g. `s.trim_matches('"')` — can leave the scanner believing a string
/// is open. If the balancing quote sits inside a *blanked* test region, the
/// scanner never closes and counts every following blank line as code, leaking
/// the whole test block back in. Extracting only the production lines keeps the
/// counted buffer self-contained and balanced, so that can't happen. Whatever
/// residual `'"'` imprecision tokei has *within* production is the same it has
/// on the raw file — this never makes the count larger than the raw file.
fn production_code(content: &str, ranges: &[(usize, usize)], cfg: &TokeiConfig) -> usize {
    if ranges.is_empty() {
        return LanguageType::Rust.parse_from_str(content, cfg).code;
    }
    let mut in_test: HashSet<usize> = HashSet::new(); // 0-based line indices.
    for &(start, end) in ranges {
        for line in start..=end {
            if line >= 1 {
                in_test.insert(line - 1);
            }
        }
    }
    let production: Vec<&str> = content
        .lines()
        .enumerate()
        .filter(|(i, _)| !in_test.contains(i))
        .map(|(_, l)| l)
        .collect();
    LanguageType::Rust
        .parse_from_str(production.join("\n"), cfg)
        .code
}

/// Test line ranges (1-based, inclusive) in one file's source, plus the
/// candidate sibling files of any out-of-line `#[cfg(test)] mod x;`. A parse
/// failure yields no exclusions, so the file is counted whole.
fn test_regions(content: &str, dir: &Path) -> (Vec<(usize, usize)>, Vec<PathBuf>) {
    let Ok(file) = syn::parse_file(content) else {
        return (Vec::new(), Vec::new());
    };
    let mut scan = TestRegionScan {
        dir,
        ranges: Vec::new(),
        mod_targets: Vec::new(),
    };
    scan.visit_file(&file);
    (scan.ranges, scan.mod_targets)
}

struct TestRegionScan<'a> {
    dir: &'a Path,
    ranges: Vec<(usize, usize)>,
    mod_targets: Vec<PathBuf>,
}

impl<'ast> Visit<'ast> for TestRegionScan<'_> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let attrs = crate::syn_util::item_attrs(item);
        if attrs.iter().any(is_cfg_test) {
            self.ranges.push(item_line_range(attrs, item));
            // `#[cfg(test)] mod x;` (no inline body) → exclude the file the
            // module lives in. The inline-body form is already covered by the
            // range push above.
            if let syn::Item::Mod(m) = item
                && m.content.is_none()
            {
                self.mod_targets
                    .extend(out_of_line_mod_files(self.dir, &m.ident.to_string()));
            }
            return; // whole item excluded; no need to descend into it.
        }
        if let syn::Item::Fn(f) = item
            && has_test_attr(&f.attrs)
        {
            self.ranges.push(item_line_range(&f.attrs, item));
            return;
        }
        visit::visit_item(self, item);
    }
}

/// True for exactly `#[cfg(test)]`. `cfg(any(test, …))`, `cfg(all(test, …))`,
/// and `cfg_attr(…)` are deliberately NOT matched: only an unconditional test
/// gate counts as test-only code (the conservative, never-under-count choice).
/// Shared with `duplicate-code`'s `ignore-test-code` skip so the two lints
/// agree on what "test code" means.
pub(crate) fn is_cfg_test(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    matches!(&attr.meta, syn::Meta::List(list)
        if list.parse_args::<syn::Ident>().is_ok_and(|i| i == "test"))
}

/// True when any attribute marks a test fn: a path ending in `test`
/// (`#[test]`, `#[tokio::test]`) or the `#[wasm_bindgen_test]` marker.
/// Shared with `duplicate-code` (see [`is_cfg_test`]).
pub(crate) fn has_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path()
            .segments
            .last()
            .is_some_and(|s| s.ident == "test" || s.ident == "wasm_bindgen_test")
    })
}

/// 1-based inclusive line range an item occupies, including its outer
/// attributes so the `#[cfg(test)]` / `#[test]` line is subtracted too.
fn item_line_range(attrs: &[syn::Attribute], item: &syn::Item) -> (usize, usize) {
    let span = item.span();
    let mut start = span.start().line;
    let end = span.end().line;
    for attr in attrs {
        start = start.min(attr.span().start().line);
    }
    (start, end)
}

/// The two paths an out-of-line `mod <name>;` can resolve to. Both are returned
/// without a filesystem check; only the one present in the counted file set
/// matters, and a stray candidate simply matches nothing.
fn out_of_line_mod_files(dir: &Path, name: &str) -> [PathBuf; 2] {
    [
        dir.join(format!("{name}.rs")),
        dir.join(name).join("mod.rs"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shipped(content: &str) -> usize {
        let (ranges, _) = test_regions(content, Path::new(""));
        production_code(content, &ranges, &TokeiConfig::default())
    }

    #[test]
    fn counts_plain_code_unchanged() {
        let src = "pub fn a() {}\npub fn b() {}\npub fn c() {}\n";
        assert_eq!(shipped(src), 3);
    }

    #[test]
    fn excludes_inline_cfg_test_module() {
        let src = "\
pub fn a() {}
pub fn b() {}

#[cfg(test)]
mod tests {
    #[test]
    fn t1() {}
    #[test]
    fn t2() {}
}
";
        // Only the two shipped fns count; the whole cfg(test) module is gone.
        assert_eq!(shipped(src), 2);
    }

    #[test]
    fn excludes_bare_test_fn() {
        let src = "\
pub fn a() {}

#[test]
fn standalone() {
    assert!(true);
}
";
        assert_eq!(shipped(src), 1);
    }

    #[test]
    fn excludes_wasm_bindgen_test_fn() {
        let src = "\
pub fn a() {}

#[wasm_bindgen_test]
fn in_browser() {
    assert!(true);
}
";
        assert_eq!(shipped(src), 1);
    }

    #[test]
    fn keeps_cfg_any_test_as_shipped() {
        // `cfg(any(test, feature = "x"))` is also live in non-test builds — not
        // test-only — so it stays counted (the never-under-count direction):
        // the attr line plus both fns, 3 code lines. (A wrong exclusion would
        // drop `maybe` and the attr, leaving 1.)
        let src = "\
#[cfg(any(test, feature = \"x\"))]
pub fn maybe() {}
pub fn always() {}
";
        assert_eq!(shipped(src), 3);
    }

    #[test]
    fn parse_failure_counts_whole_file() {
        // Not valid Rust → no exclusions, count everything tokei sees as code.
        let src = "this is not ; valid rust @@@\npub fn but_two_code_lines() {}\n";
        let (ranges, mods) = test_regions(src, Path::new(""));
        assert!(ranges.is_empty());
        assert!(mods.is_empty());
    }

    #[test]
    fn out_of_line_test_mod_resolves_both_candidates() {
        let (ranges, mods) = test_regions("#[cfg(test)]\nmod helpers;\n", Path::new("src"));
        assert_eq!(ranges.len(), 1); // the decl line is excluded too.
        assert!(mods.contains(&PathBuf::from("src/helpers.rs")));
        assert!(mods.contains(&PathBuf::from("src/helpers/mod.rs")));
    }

    #[test]
    fn nested_cfg_test_in_shipped_module_is_excluded() {
        let src = "\
pub mod feature {
    pub fn shipped() {}

    #[cfg(test)]
    mod tests {
        #[test]
        fn t() {}
    }
}
";
        // `pub mod feature {`, `pub fn shipped`, and the module's closing `}` —
        // the inner cfg(test) block (incl. its braces) is subtracted.
        assert_eq!(shipped(src), 3);
    }

    #[test]
    fn test_region_with_quote_char_literal_does_not_overcount() {
        // Regression: a `'"'` char literal in shipped code (here `trim_matches`)
        // confuses tokei's stateful string scanner. The old approach blanked the
        // test lines in place, which severed the scanner's quote balance and made
        // every following blank line count as string-continuation "code" — the
        // whole test mod leaked back in. Counting the region in isolation and
        // subtracting avoids that: shipped is exactly the two production fns.
        let src = "\
pub fn parse(s: &str) -> &str {
    s.trim_matches('\"')
}
pub fn other() {}

#[cfg(test)]
mod tests {
    #[test]
    fn t1() {
        assert_eq!(super::parse(\"x\"), \"x\");
    }
    #[test]
    fn t2() {
        assert_eq!(super::other(), ());
    }
}
";
        // `pub fn parse {`, `s.trim_matches`, `}`, `pub fn other` = 4 shipped.
        assert_eq!(shipped(src), 4);
    }

    #[test]
    fn in_dev_target_dir_matches_top_level_only() {
        let root = Path::new("/ws/crates/demo");
        assert!(in_dev_target_dir(
            root,
            Path::new("/ws/crates/demo/tests/it.rs")
        ));
        assert!(in_dev_target_dir(
            root,
            Path::new("/ws/crates/demo/benches/b.rs")
        ));
        assert!(in_dev_target_dir(
            root,
            Path::new("/ws/crates/demo/examples/e.rs")
        ));
        // src/ and build.rs are shipped; a nested src/tests.rs is not a dev dir.
        assert!(!in_dev_target_dir(
            root,
            Path::new("/ws/crates/demo/src/lib.rs")
        ));
        assert!(!in_dev_target_dir(
            root,
            Path::new("/ws/crates/demo/build.rs")
        ));
        assert!(!in_dev_target_dir(
            root,
            Path::new("/ws/crates/demo/src/tests.rs")
        ));
    }

    #[test]
    fn rootless_discovers_crate_root_via_cargo_toml() {
        // Build `<tmp>/crates/demo/{Cargo.toml, src/lib.rs, tests/it.rs}` and a
        // workspace `Cargo.toml` at the root. The nearest `Cargo.toml` to each
        // file is the crate dir, so the dev-target rule keys off the crate root
        // even with no cargo metadata.
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let demo = root.join("crates/demo");
        std::fs::create_dir_all(demo.join("src")).unwrap();
        std::fs::create_dir_all(demo.join("tests")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(demo.join("Cargo.toml"), "[package]\n").unwrap();

        assert!(in_dev_target_dir_rootless(&demo.join("tests/it.rs")));
        assert!(!in_dev_target_dir_rootless(&demo.join("src/lib.rs")));
        // No `Cargo.toml` ancestor → treated as shipped (never under-count).
        assert!(!in_dev_target_dir_rootless(Path::new(
            "/nonexistent/tests/x.rs"
        )));
    }
}
