//! Unit tests for the normalization + grouping engine. Each test feeds
//! source strings straight through `find_clones`, so the α-rename, literal
//! abstraction, suppression-context, and subsumption rules are exercised
//! without a workspace on disk.

use std::path::PathBuf;

use super::{Options, ScanFile, find_clones};

/// Low thresholds so small snippets qualify; individual tests override. The
/// two noise filters are OFF here so every matching-semantics test keeps
/// pinning normalization/grouping behavior alone — the filters have their own
/// dedicated tests at the bottom.
fn opts() -> Options {
    Options {
        min_lines: 1,
        min_tokens: 5,
        min_instances: 2,
        ignore_literals: true,
        ignore_test_code: true,
        cross_crate_only: false,
        min_distinct_anchors: 0,
        min_non_repeating_ratio: 0.0,
    }
}

fn file(name: &str, krate: &str, src: &str) -> ScanFile {
    ScanFile {
        rel_path: PathBuf::from(name),
        krate: krate.to_string(),
        ast: syn::parse_file(src).expect("test source must parse"),
    }
}

/// Two single-file sources → the clone groups between them.
fn clones_between(a: &str, b: &str, opts: &Options) -> Vec<super::CloneGroup> {
    find_clones(
        &[file("a.rs", "crate-a", a), file("b.rs", "crate-b", b)],
        opts,
    )
}

#[test]
fn consistent_rename_with_different_literals_matches() {
    // The motivating case: names AND literals differ, structure identical.
    let a = "fn compute(user: u32) -> u32 {\n    let total = user + 1;\n    total * 2\n}";
    let b = "fn summed(p: u32) -> u32 {\n    let sum = p + 5;\n    sum * 2\n}";
    let groups = clones_between(a, b, &opts());
    assert!(!groups.is_empty(), "renamed clone must match");
    assert_eq!(groups[0].instances.len(), 2);
}

#[test]
fn name_reuse_pattern_is_part_of_the_structure() {
    // α-consistency edge: in `a` the local shares the *fn's own* name, so
    // both map to one placeholder; `b` uses two distinct names. The reuse
    // pattern differs, so these are (correctly) not clones.
    let a = "fn total(user: u32) -> u32 {\n    let total = user + 1;\n    total * 2\n}";
    let b = "fn summed(p: u32) -> u32 {\n    let sum = p + 5;\n    sum * 2\n}";
    assert!(clones_between(a, b, &opts()).is_empty());
}

#[test]
fn inconsistent_rename_does_not_match() {
    // Same tokens modulo names, but the *pattern* of name reuse differs:
    // `a + b` vs `y + x` (operand order swapped relative to binding order).
    // min-tokens 15: the two `let` statements ARE a genuine 14-token clone
    // run — the test is about the whole fn (~25 tokens) not matching.
    let a = "fn f() -> u32 {\n    let a = g();\n    let b = h();\n    a + b\n}";
    let b = "fn f() -> u32 {\n    let x = g();\n    let y = h();\n    y + x\n}";
    let sized = Options {
        min_tokens: 15,
        ..opts()
    };
    assert!(clones_between(a, b, &sized).is_empty());
}

#[test]
fn different_structure_does_not_match() {
    let a = "fn f(x: u32) -> u32 {\n    let y = x + 1;\n    y * 2\n}";
    let b = "fn f(x: u32) -> u32 {\n    let y = x * 2;\n    y + 1\n}";
    assert!(clones_between(a, b, &opts()).is_empty());
}

#[test]
fn free_function_names_are_anchors() {
    // Identical shapes calling *different* free functions are not clones.
    let a = "fn f(x: u32) -> u32 {\n    let y = alpha(x);\n    beta(y)\n}";
    let b = "fn f(x: u32) -> u32 {\n    let y = gamma(x);\n    delta(y)\n}";
    assert!(clones_between(a, b, &opts()).is_empty());
}

#[test]
fn field_and_method_names_are_anchors() {
    // `.age` vs `.len` and `.iter()` vs `.keys()` must not be erased even
    // when a local shares the field's spelling.
    let a = "fn f(user: U) -> u32 {\n    let age = user.age;\n    age + user.items.iter().count() as u32\n}";
    let b = "fn f(user: U) -> u32 {\n    let age = user.len;\n    age + user.items.keys().count() as u32\n}";
    assert!(clones_between(a, b, &opts()).is_empty());
}

#[test]
fn range_locals_still_rename() {
    // Regression for the field-dot heuristic: `0..n` is a range, not field
    // access, so the trailing local must still α-rename.
    let a = "fn f(n: u32) -> u32 {\n    let mut s = 0;\n    for i in 0..n {\n        s += i;\n    }\n    s\n}";
    let b = "fn f(m: u32) -> u32 {\n    let mut t = 0;\n    for j in 0..m {\n        t += j;\n    }\n    t\n}";
    assert!(!clones_between(a, b, &opts()).is_empty());
}

#[test]
fn path_segments_do_not_rename() {
    // A local named `new` must not erase `Foo::new` path segments.
    let a = "fn f() -> u32 {\n    let new = Foo::new();\n    new.run() + 1\n}";
    let b = "fn f() -> u32 {\n    let old = Foo::make();\n    old.run() + 1\n}";
    assert!(clones_between(a, b, &opts()).is_empty());
}

#[test]
fn literals_are_anchors_when_ignore_literals_is_off() {
    let a = "fn f(x: u32) -> u32 {\n    let y = x + 1;\n    y * 2\n}";
    let b = "fn f(z: u32) -> u32 {\n    let w = z + 5;\n    w * 2\n}";
    let strict = Options {
        ignore_literals: false,
        ..opts()
    };
    assert!(clones_between(a, b, &strict).is_empty());
    // Same literals + renamed locals still match under strict literals.
    let c = "fn f(z: u32) -> u32 {\n    let w = z + 1;\n    w * 2\n}";
    assert!(!clones_between(a, c, &strict).is_empty());
}

#[test]
fn literal_kinds_are_not_conflated() {
    // `#int` vs `#str`: a numeric and a string literal in the same slot are
    // different structures even with ignore-literals on.
    let a = "fn f() -> String {\n    let v = wrap(1);\n    v.render()\n}";
    let b = "fn f() -> String {\n    let v = wrap(\"one\");\n    v.render()\n}";
    assert!(clones_between(a, b, &opts()).is_empty());
}

#[test]
fn macro_contents_normalize_like_code() {
    // Locals referenced inside a macro invocation rename consistently.
    let a = "fn f(count: u32) {\n    let doubled = count * 2;\n    println!(\"{} {}\", count, doubled);\n}";
    let b = "fn f(n: u32) {\n    let d = n * 2;\n    println!(\"{} {}\", n, d);\n}";
    assert!(!clones_between(a, b, &opts()).is_empty());
}

#[test]
fn fn_group_dominates_its_body_block_group() {
    // Identical fns produce both a fn-level and a body-block-level match on
    // the same lines; subsumption must report exactly one group.
    let a = "fn f(x: u32) -> u32 {\n    let y = x + 1;\n    let z = y * 3;\n    z - x\n}";
    let b = "fn g(q: u32) -> u32 {\n    let r = q + 1;\n    let s = r * 3;\n    s - q\n}";
    let groups = clones_between(a, b, &opts());
    assert_eq!(
        groups.len(),
        1,
        "the body-block group must be subsumed by the fn group"
    );
}

#[test]
fn inner_block_copied_to_a_third_place_survives_subsumption() {
    // fns f/g are whole-fn clones; the loop block inside them ALSO appears in
    // otherwise-different h. The block group has an uncovered instance, so it
    // must survive (with all three sites).
    let block = "    for i in 0..limit {\n        acc = acc + i;\n        acc = acc * 2;\n        acc = acc % 97;\n    }\n";
    let f = format!("fn f(limit: u32) -> u32 {{\n    let mut acc = 0;\n{block}    acc\n}}");
    let g = format!("fn g(limit: u32) -> u32 {{\n    let mut acc = 0;\n{block}    acc\n}}");
    let h = format!(
        "fn h(limit: u32, seed: u32) -> u32 {{\n    let mut acc = seed;\n    let mut extra = limit * 3;\n{block}    extra += acc;\n    extra\n}}"
    );
    let src_a = format!("{f}\n{g}");
    let groups = clones_between(&src_a, &h, &opts());
    // The fn group (f, g) plus the block group (3 sites incl. h's).
    assert_eq!(groups.len(), 2);
    let block_group = groups
        .iter()
        .find(|g| g.instances.len() == 3)
        .expect("the 3-site block group must survive");
    assert!(
        block_group
            .instances
            .iter()
            .any(|r| r.file.ends_with("b.rs"))
    );
}

#[test]
fn statement_run_is_found_maximally_not_as_fragments() {
    // The copied span is a mid-body statement RUN: the enclosing fns differ
    // before and after it, so no fn/block candidate covers it whole. The run
    // candidate must — as ONE group spanning the full run at both sites
    // (sub-runs of it are subsumed; the trailing `0`/`1` tails stay out
    // because the statement before them differs).
    let a = "fn f(input: &[u32]) -> u32 {\n\
             \x20   let seed = prepare(input);\n\
             \x20   let mut acc = seed;\n\
             \x20   for v in input {\n\
             \x20       acc = combine(acc, *v);\n\
             \x20       acc = acc % 97;\n\
             \x20   }\n\
             \x20   let out = finish(acc);\n\
             \x20   emit(out);\n\
             \x20   cleanup_alpha();\n\
             \x20   0\n\
             }";
    let b = "fn g(input: &[u32]) -> u32 {\n\
             \x20   let seed = prepare_other(input);\n\
             \x20   let mut total = seed;\n\
             \x20   for item in input {\n\
             \x20       total = combine(total, *item);\n\
             \x20       total = total % 13;\n\
             \x20   }\n\
             \x20   let res = finish(total);\n\
             \x20   emit(res);\n\
             \x20   teardown_beta();\n\
             \x20   1\n\
             }";
    let groups = clones_between(a, b, &opts());
    assert_eq!(groups.len(), 1, "one maximal run group, no fragments");
    let g = &groups[0];
    assert_eq!(g.instances.len(), 2);
    // `let mut acc = seed;` (line 3) through `emit(out);` (line 9), both files.
    assert!(
        g.instances
            .iter()
            .all(|r| r.line_start == 3 && r.line_end == 9)
    );
}

#[test]
fn whole_block_matches_a_sub_run_of_a_longer_block() {
    // a's entire fn body reappears mid-body in b. No braces align (b's block
    // is longer), so the match is a's brace-less full interior run against
    // b's sub-run.
    let a = "fn f(xs: &[u32]) {\n\
             \x20   let mut acc = start();\n\
             \x20   for x in xs {\n\
             \x20       acc = fold(acc, *x);\n\
             \x20   }\n\
             \x20   finish(acc);\n\
             }";
    let b = "fn g(xs: &[u32]) {\n\
             \x20   log_begin();\n\
             \x20   let mut total = start();\n\
             \x20   for y in xs {\n\
             \x20       total = fold(total, *y);\n\
             \x20   }\n\
             \x20   finish(total);\n\
             \x20   publish();\n\
             }";
    let groups = clones_between(a, b, &opts());
    assert_eq!(groups.len(), 1);
    let g = &groups[0];
    let a_site = g
        .instances
        .iter()
        .find(|r| r.file.ends_with("a.rs"))
        .expect("a.rs instance");
    let b_site = g
        .instances
        .iter()
        .find(|r| r.file.ends_with("b.rs"))
        .expect("b.rs instance");
    assert_eq!((a_site.line_start, a_site.line_end), (2, 6));
    assert_eq!((b_site.line_start, b_site.line_end), (3, 7));
}

#[test]
fn run_locals_bound_outside_the_run_are_anchors() {
    // `seed` is bound BEFORE the copied statements with per-file names, and
    // used inside them: from the run's perspective it is free, so the runs
    // must NOT match (the under-matching direction, documented in detect.rs).
    let a = "fn f() -> u32 {\n\
             \x20   let seed = alpha();\n\
             \x20   let mut acc = combine(seed, 1);\n\
             \x20   acc = wrap(acc, seed);\n\
             \x20   acc = wrap(acc, seed);\n\
             \x20   acc + seed\n\
             }";
    let b = "fn g() -> u32 {\n\
             \x20   let source = beta();\n\
             \x20   let mut acc = combine(source, 2);\n\
             \x20   acc = wrap(acc, source);\n\
             \x20   acc = wrap(acc, source);\n\
             \x20   acc + source\n\
             }";
    assert!(clones_between(a, b, &opts()).is_empty());
}

#[test]
fn min_instances_filters_groups() {
    let a = "fn f(x: u32) -> u32 {\n    let y = x + 1;\n    y * 2\n}";
    let b = "fn g(z: u32) -> u32 {\n    let w = z + 5;\n    w * 2\n}";
    let three = Options {
        min_instances: 3,
        ..opts()
    };
    assert!(clones_between(a, b, &three).is_empty());
}

#[test]
fn thresholds_drop_small_regions() {
    let a = "fn f(x: u32) -> u32 {\n    x + 1\n}";
    let b = "fn g(z: u32) -> u32 {\n    z + 5\n}";
    let sized = Options {
        min_lines: 4,
        ..opts()
    };
    assert!(clones_between(a, b, &sized).is_empty());
    let tokened = Options {
        min_tokens: 100,
        ..opts()
    };
    assert!(clones_between(a, b, &tokened).is_empty());
}

#[test]
fn cross_crate_only_requires_two_crates() {
    let a = "fn f(x: u32) -> u32 {\n    let y = x + 1;\n    y * 2\n}\nfn g(z: u32) -> u32 {\n    let w = z + 5;\n    w * 2\n}";
    let cross = Options {
        cross_crate_only: true,
        ..opts()
    };
    // Both instances in one crate → filtered.
    assert!(find_clones(&[file("a.rs", "crate-a", a)], &cross).is_empty());
    // Same pair split across crates → reported.
    let f1 = "fn f(x: u32) -> u32 {\n    let y = x + 1;\n    y * 2\n}";
    let f2 = "fn g(z: u32) -> u32 {\n    let w = z + 5;\n    w * 2\n}";
    assert!(!clones_between(f1, f2, &cross).is_empty());
}

#[test]
fn cfg_test_items_are_skipped() {
    let test_mod = "#[cfg(test)]\nmod tests {\n    fn f(x: u32) -> u32 {\n        let y = x + 1;\n        y * 2\n    }\n}";
    let shipped = "fn g(z: u32) -> u32 {\n    let w = z + 5;\n    w * 2\n}";
    assert!(clones_between(test_mod, shipped, &opts()).is_empty());
    // With ignore-test-code off, the same pair matches.
    let scan_tests = Options {
        ignore_test_code: false,
        ..opts()
    };
    assert!(!clones_between(test_mod, shipped, &scan_tests).is_empty());
}

#[test]
fn test_attr_fns_are_skipped() {
    let a = "#[test]\nfn t() {\n    let y = probe() + 1;\n    assert_eq!(y * 2, 4);\n}";
    let b = "#[tokio::test]\nfn t2() {\n    let w = probe() + 5;\n    assert_eq!(w * 2, 8);\n}";
    assert!(clones_between(a, b, &opts()).is_empty());
}

#[test]
fn trait_default_methods_are_candidates() {
    let a = "trait T {\n    fn run(&self, x: u32) -> u32 {\n        let y = x + 1;\n        y * self.base()\n    }\n    fn base(&self) -> u32;\n}";
    let b = "trait U {\n    fn run(&self, q: u32) -> u32 {\n        let r = q + 9;\n        r * self.base()\n    }\n    fn base(&self) -> u32;\n}";
    assert!(!clones_between(a, b, &opts()).is_empty());
}

#[test]
fn anchor_floor_suppresses_name_poor_clones() {
    // A fill-table pair: structurally identical, but the only distinct
    // verbatim names are `Map`, `u32`, `create`, `insert` — exactly 4.
    // Keywords (`fn`, `let`, `mut`) must not count, or the floor-5 assertion
    // below would pass the pair.
    let a = "fn fill(input: u32) -> Map {\n\
             \x20   let mut m = create();\n\
             \x20   m.insert(input, 1);\n\
             \x20   m.insert(input, 2);\n\
             \x20   m\n\
             }";
    let b = "fn build(seed: u32) -> Map {\n\
             \x20   let mut t = create();\n\
             \x20   t.insert(seed, 5);\n\
             \x20   t.insert(seed, 6);\n\
             \x20   t\n\
             }";
    let at = |floor: usize| Options {
        min_distinct_anchors: floor,
        ..opts()
    };
    assert!(!clones_between(a, b, &at(4)).is_empty(), "4 anchors ≥ 4");
    assert!(clones_between(a, b, &at(5)).is_empty(), "4 anchors < 5");
}

#[test]
fn repetition_ratio_suppresses_stamped_row_tables() {
    // One row stamped out eight times: under `ignore-literals` every insert
    // normalizes identically, so most of the stream repeats its own earlier
    // windows. Self-repetition is not duplication worth extracting — the
    // whole-fn, block, AND run candidates inside must all be filtered.
    //
    // Runs at the shipped size thresholds, not the toy ones: the ratio and
    // size gates are designed to compose. A 2-row sub-run repeats too little
    // to trip the ratio, but at 18 tokens it never clears `min-tokens` — only
    // table-sized candidates surface, and those are exactly the repetitive
    // ones.
    let a = "fn seed_table() -> Table {\n\
             \x20   let mut m = fresh();\n\
             \x20   m.insert(\"a\", 1);\n\
             \x20   m.insert(\"b\", 2);\n\
             \x20   m.insert(\"c\", 3);\n\
             \x20   m.insert(\"d\", 4);\n\
             \x20   m.insert(\"e\", 5);\n\
             \x20   m.insert(\"f\", 6);\n\
             \x20   m.insert(\"g\", 7);\n\
             \x20   m.insert(\"h\", 8);\n\
             \x20   m\n\
             }";
    let b = "fn defaults() -> Table {\n\
             \x20   let mut t = fresh();\n\
             \x20   t.insert(\"s\", 9);\n\
             \x20   t.insert(\"y\", 8);\n\
             \x20   t.insert(\"z\", 7);\n\
             \x20   t.insert(\"w\", 6);\n\
             \x20   t.insert(\"v\", 5);\n\
             \x20   t.insert(\"u\", 4);\n\
             \x20   t.insert(\"q\", 3);\n\
             \x20   t.insert(\"p\", 2);\n\
             \x20   t\n\
             }";
    let sized = Options {
        min_lines: 8,
        min_tokens: 40,
        ..opts()
    };
    let filtered = Options {
        min_non_repeating_ratio: 0.5,
        ..sized
    };
    assert!(!clones_between(a, b, &sized).is_empty(), "matches raw");
    assert!(
        clones_between(a, b, &filtered).is_empty(),
        "filtered at 0.5"
    );
}

#[test]
fn anchor_rich_clone_passes_both_filters_at_defaults() {
    // A realistic copied pipeline: seven distinct callees/types, no repeated
    // windows. Must survive the shipped defaults (floor 4, ratio 0.5).
    let a = "fn load(root: &Path) -> Config {\n\
             \x20   let raw = read_source(root);\n\
             \x20   let parsed = parse_layers(raw);\n\
             \x20   let merged = merge_defaults(parsed);\n\
             \x20   validate(&merged);\n\
             \x20   finalize(merged)\n\
             }";
    let b = "fn boot(dir: &Path) -> Config {\n\
             \x20   let text = read_source(dir);\n\
             \x20   let layers = parse_layers(text);\n\
             \x20   let full = merge_defaults(layers);\n\
             \x20   validate(&full);\n\
             \x20   finalize(full)\n\
             }";
    let defaults = Options {
        min_distinct_anchors: 4,
        min_non_repeating_ratio: 0.5,
        ..opts()
    };
    assert!(!clones_between(a, b, &defaults).is_empty());
}

#[test]
fn a_few_repeated_lines_do_not_disqualify_a_real_clone() {
    // Genuine clone with ONE repeated statement (`refresh(cache);` twice):
    // a small repeated fraction must stay under the 0.5 bar.
    let a = "fn sync(cache: &mut Cache) -> Report {\n\
             \x20   let snapshot = capture(cache);\n\
             \x20   refresh(cache);\n\
             \x20   let delta = diff_since(snapshot);\n\
             \x20   apply_rules(&delta);\n\
             \x20   refresh(cache);\n\
             \x20   let summary = summarize(delta);\n\
             \x20   render_report(summary)\n\
             }";
    let b = "fn pull(store: &mut Cache) -> Report {\n\
             \x20   let seen = capture(store);\n\
             \x20   refresh(store);\n\
             \x20   let changed = diff_since(seen);\n\
             \x20   apply_rules(&changed);\n\
             \x20   refresh(store);\n\
             \x20   let digest = summarize(changed);\n\
             \x20   render_report(digest)\n\
             }";
    let filtered = Options {
        min_non_repeating_ratio: 0.5,
        ..opts()
    };
    assert!(!clones_between(a, b, &filtered).is_empty());
}

#[test]
fn repetitive_statement_run_is_filtered_incrementally() {
    // The stamped rows sit mid-body in fns that differ before and after, so
    // only RUN candidates cover them — this pins the per-start incremental
    // metric tracking, not just the one-shot fn/block path. `min-tokens` is
    // realistic (a qualifying run is 7+ rows and deeply self-repeating);
    // `min-lines` stays at 1 so the ratio — not the line gate — is what
    // kills the runs.
    let a = "fn f() {\n\
             \x20   prologue_alpha();\n\
             \x20   record(1);\n\
             \x20   record(2);\n\
             \x20   record(3);\n\
             \x20   record(4);\n\
             \x20   record(5);\n\
             \x20   record(6);\n\
             \x20   record(7);\n\
             \x20   record(8);\n\
             \x20   epilogue_alpha();\n\
             }";
    let b = "fn g() {\n\
             \x20   prologue_beta();\n\
             \x20   record(9);\n\
             \x20   record(10);\n\
             \x20   record(11);\n\
             \x20   record(12);\n\
             \x20   record(13);\n\
             \x20   record(14);\n\
             \x20   record(15);\n\
             \x20   record(16);\n\
             \x20   epilogue_beta();\n\
             }";
    let sized = Options {
        min_tokens: 40,
        ..opts()
    };
    let filtered = Options {
        min_non_repeating_ratio: 0.5,
        ..sized
    };
    assert!(!clones_between(a, b, &sized).is_empty(), "runs match raw");
    assert!(
        clones_between(a, b, &filtered).is_empty(),
        "filtered at 0.5"
    );
}

#[test]
fn instances_and_groups_are_deterministically_ordered() {
    let a = "fn f(x: u32) -> u32 {\n    let y = x + 1;\n    y * 2\n}";
    let b = "fn g(z: u32) -> u32 {\n    let w = z + 5;\n    w * 2\n}";
    let groups = clones_between(b, a, &opts());
    assert_eq!(groups.len(), 1);
    let files: Vec<String> = groups[0]
        .instances
        .iter()
        .map(|r| r.file.display().to_string())
        .collect();
    assert_eq!(files, ["a.rs", "b.rs"], "instances sort by (file, line)");
}

// ---- literal divergence -------------------------------------------------
//
// These exercise `divergence::analyze` end-to-end: detect between two
// sources, then read the concrete literals back off the group.

use super::CandidateKind;
use super::divergence::{Divergence, DivergenceAnalyzer};

/// Detect between two sources and analyze the first group's divergence.
fn divergence_between(a: &str, b: &str) -> Divergence {
    let files = [file("a.rs", "crate-a", a), file("b.rs", "crate-b", b)];
    let groups = find_clones(&files, &opts());
    assert!(!groups.is_empty(), "sources must produce a group");
    DivergenceAnalyzer::new(&files)
        .analyze(&groups[0])
        .expect("capture must align")
}

#[test]
fn identical_instances_have_zero_divergence() {
    let a = "fn alpha(x: u32) -> u32 {\n    let y = x + 7;\n    y * 7\n}";
    let b = "fn beta(q: u32) -> u32 {\n    let r = q + 7;\n    r * 7\n}";
    let d = divergence_between(a, b);
    assert_eq!((d.positions, d.divergent, d.params), (2, 0, 0));
    assert!(d.violations.is_empty());
}

#[test]
fn covarying_literals_are_one_parameter() {
    // 7 → 9 at both positions: one consistent mapping = one parameter.
    let a = "fn alpha(x: u32) -> u32 {\n    let y = x + 7;\n    y * 7\n}";
    let b = "fn beta(q: u32) -> u32 {\n    let r = q + 9;\n    r * 9\n}";
    let d = divergence_between(a, b);
    assert_eq!((d.positions, d.divergent, d.params), (2, 2, 1));
    assert!(d.violations.is_empty());
}

#[test]
fn independent_literals_are_separate_parameters() {
    let a = "fn alpha(x: u32) -> u32 {\n    log(\"up\");\n    x + 7\n}";
    let b = "fn beta(q: u32) -> u32 {\n    log(\"down\");\n    q + 9\n}";
    let d = divergence_between(a, b);
    assert_eq!((d.positions, d.divergent, d.params), (2, 2, 2));
    assert!(d.violations.is_empty());
}

#[test]
fn forgotten_rename_is_a_violation() {
    // The CP-Miner headline case: the copy renamed "alpha" → "beta" but
    // missed the third occurrence. The forgotten position AGREES across
    // instances — drift hides at a non-divergent position.
    let a = "fn wa(x: u32) {\n    log(\"alpha\", x);\n    send(\"alpha\", x);\n    emit(\"alpha\", x);\n}";
    let b = "fn wb(q: u32) {\n    log(\"beta\", q);\n    send(\"beta\", q);\n    emit(\"alpha\", q);\n}";
    let d = divergence_between(a, b);
    assert_eq!(d.violations.len(), 1, "exactly one drift finding");
    let v = &d.violations[0];
    assert_eq!(v.file.display().to_string(), "b.rs");
    assert_eq!(v.line, 4, "anchored at the forgotten occurrence");
    assert_eq!(
        (v.found.as_str(), v.expected.as_str()),
        ("\"alpha\"", "\"beta\"")
    );
}

#[test]
fn single_position_parameter_is_not_drift() {
    // A lone divergent position against identity classes is the classic
    // parameterizable clone — donors must be divergent, so no violation.
    let a = "fn wa(x: u32) {\n    log(\"k\", x);\n    tick(3, x);\n}";
    let b = "fn wb(q: u32) {\n    log(\"k\", q);\n    tick(4, q);\n}";
    let d = divergence_between(a, b);
    assert_eq!(d.params, 1);
    assert!(d.violations.is_empty());
}

#[test]
fn all_distinct_defect_is_not_drift() {
    // p2 diverges but its value matches no other instance's mapping side —
    // prose-shaped, killed by the stale-twin guard.
    let a =
        "fn wa(x: u32) {\n    log(\"load\", x);\n    send(\"load\", x);\n    emit(\"load\", x);\n}";
    let b = "fn wb(q: u32) {\n    log(\"store\", q);\n    send(\"store\", q);\n    emit(\"fetch\", q);\n}";
    let d = divergence_between(a, b);
    assert_eq!(d.params, 2, "the defect is its own class");
    assert!(d.violations.is_empty());
}

#[test]
fn sentinel_values_never_anchor_drift() {
    // Same shape as the forgotten rename, but the value is `0`.
    let a = "fn wa(x: u32) {\n    log(0, x);\n    send(0, x);\n    emit(0, x);\n}";
    let b = "fn wb(q: u32) {\n    log(2, q);\n    send(2, q);\n    emit(0, q);\n}";
    let d = divergence_between(a, b);
    assert!(d.violations.is_empty());
}

#[test]
fn drift_detected_across_three_instances() {
    let a =
        "fn wa(x: u32) {\n    log(\"red\", x);\n    send(\"red\", x);\n    emit(\"red\", x);\n}";
    let b =
        "fn wb(q: u32) {\n    log(\"red\", q);\n    send(\"red\", q);\n    emit(\"red\", q);\n}";
    let c =
        "fn wc(z: u32) {\n    log(\"blue\", z);\n    send(\"blue\", z);\n    emit(\"red\", z);\n}";
    let files = [
        file("a.rs", "crate-a", a),
        file("b.rs", "crate-b", b),
        file("c.rs", "crate-c", c),
    ];
    let groups = find_clones(&files, &opts());
    let d = DivergenceAnalyzer::new(&files)
        .analyze(&groups[0])
        .expect("capture must align");
    assert_eq!(d.violations.len(), 1);
    let v = &d.violations[0];
    assert_eq!(v.file.display().to_string(), "c.rs");
    assert_eq!(
        (v.found.as_str(), v.expected.as_str()),
        ("\"red\"", "\"blue\"")
    );
}

#[test]
fn arm_pattern_literals_stay_outside_block_candidates() {
    // The block candidates start at their `{`, right of the arm patterns
    // `3 =>` / `4 =>` on the same line — column slicing must not capture
    // the pattern literals, or they would read as spurious divergence.
    let a = "fn wa(x: u32) -> u32 {\n\
             \x20   let pre = x * 3;\n\
             \x20   match pre {\n\
             \x20       3 => { let y = 7; log(\"k\"); y * 2 }\n\
             \x20       _ => fizz(x),\n\
             \x20   }\n\
             }";
    let b = "fn wb(q: u32) -> u32 {\n\
             \x20   match q {\n\
             \x20       4 => { let y = 7; log(\"k\"); y * 2 }\n\
             \x20       _ => buzz(q, q),\n\
             \x20   }\n\
             }";
    let files = [file("a.rs", "crate-a", a), file("b.rs", "crate-b", b)];
    let groups = find_clones(&files, &opts());
    let block = groups
        .iter()
        .find(|g| g.instances[0].kind == CandidateKind::Block)
        .expect("the twin arm blocks must group");
    let d = DivergenceAnalyzer::new(&files)
        .analyze(block)
        .expect("capture must align");
    assert_eq!(d.positions, 3, "7, \"k\", 2 — and NOT the arm patterns");
    assert_eq!(d.divergent, 0);
}

/// A structurally identical pair (names and literals differ) that groups into
/// exactly one clone — the substrate of the fingerprint-portability tests.
fn clone_pair() -> (String, String) {
    let a = "fn compute(user: u32) -> u32 {\n    let total = user + 1;\n    total * 2\n}";
    let b = "fn summed(p: u32) -> u32 {\n    let sum = p + 5;\n    sum * 2\n}";
    (a.to_string(), b.to_string())
}

/// The a↔b clone's fingerprint (the group that spans `a.rs`).
fn pair_fingerprint(groups: &[super::CloneGroup]) -> u64 {
    groups
        .iter()
        .find(|g| {
            g.instances
                .iter()
                .any(|r| r.file == std::path::Path::new("a.rs"))
        })
        .expect("the a↔b clone must be found")
        .fingerprint
}

#[test]
fn fingerprint_is_portable_across_unrelated_interned_code() {
    // The load-bearing baseline property: an unrelated file interned *first*
    // shifts every later symbol id, but the clone's content digest — and thus
    // its fingerprint — must be unchanged. (The pre-baseline fingerprint hashed
    // interner ids and failed exactly here.)
    let (a, b) = clone_pair();
    let bare = find_clones(&[file("a.rs", "k", &a), file("b.rs", "k", &b)], &opts());
    let unrelated =
        "fn zzz(alpha: u64) -> u64 {\n    let beta = alpha + 7;\n    gamma(beta) + delta(alpha)\n}";
    let shifted = find_clones(
        &[
            file("u.rs", "k", unrelated),
            file("a.rs", "k", &a),
            file("b.rs", "k", &b),
        ],
        &opts(),
    );
    assert_eq!(pair_fingerprint(&bare), pair_fingerprint(&shifted));
}

#[test]
fn fingerprint_is_stable_regardless_of_file_order() {
    let (a, b) = clone_pair();
    let forward = find_clones(&[file("a.rs", "k", &a), file("b.rs", "k", &b)], &opts());
    let reversed = find_clones(&[file("b.rs", "k", &b), file("a.rs", "k", &a)], &opts());
    assert_eq!(pair_fingerprint(&forward), pair_fingerprint(&reversed));
}

#[test]
fn fingerprint_stability_canary() {
    // Pins the exact fingerprint of a fixed clone. Any change to normalization
    // or the hash changes this value and INVALIDATES every checked-in baseline
    // — bump it deliberately and document regeneration (README ratchet note).
    let (a, b) = clone_pair();
    let groups = find_clones(&[file("a.rs", "k", &a), file("b.rs", "k", &b)], &opts());
    assert_eq!(pair_fingerprint(&groups), 0x9930_bf38_35a5_6614);
}
