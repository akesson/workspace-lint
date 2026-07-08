//! Live-in / live-out analysis for statement-run clone groups — the extraction
//! signature a run would produce if lifted into a function.
//!
//! A whole-fn or whole-block clone extracts along a brace boundary: its inputs
//! are its parameters and its output is its return, both already spelled in the
//! source. A *statement run* mid-body has no such boundary — the question
//! "what would the extracted fn's signature be?" is exactly a data-flow one:
//!
//! - **live-in** — names bound *before* the run and *read inside* it. These
//!   become the extracted fn's parameters.
//! - **live-out** — names bound *inside* the run and *read after* it (before
//!   the enclosing fn ends). These become its return values. More than one is
//!   the awkward case: a multi-value return is a code smell the caller has to
//!   destructure, so the lint downgrades those groups.
//!
//! The whole analysis is syntactic (no types, no borrow information), so it
//! runs on the build-free `--stats` path and never needs the semantic tier.
//! It resolves references the same way the fingerprint does — `scan_idents`
//! applies the identical `.`/`::`/`'` suppression, so field names and path
//! segments are never mistaken for variable uses.
//!
//! Reaching-bind resolution is a flat lexical approximation, not a real scope
//! stack (the same discipline the detector documents). A use is credited to
//! the nearest binding of its name at an earlier source position; block nesting
//! is ignored. Consequences, all documented and all in the *softening*
//! direction (never inventing a downgrade that hides a clone, only ever
//! mispricing the extraction hint):
//! - a name bound in a nested scope inside the run that is *also* the spelling
//!   of an outer name read after the run is over-counted as live-out (the
//!   after-use resolves to the inner binding). The shadowing KFN fixture pins
//!   this.
//! - a `let x = f(x)` inside the run that shadows an outer `x` reads as the
//!   inner binding, so the outer `x` may be under-counted as a parameter.
//! - an assignment after the run counts as a read (writes aren't distinguished
//!   from reads), so a value that is only overwritten still reads as live-out.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use proc_macro2::LineColumn;
use quote::ToTokens;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

use super::encode::{NormState, scan_idents};
use super::{CandidateKind, CloneGroup, Region, ScanFile};

/// The extraction signature a statement-run clone group would produce.
/// Produced by [`LivenessAnalyzer::analyze`]; `None` for non-run groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Liveness {
    /// Names bound before the run and read inside it — the parameters an
    /// extraction would take. Identical across instances by construction
    /// (a name bound outside the run stays a verbatim anchor, so the copies
    /// only group where they spell it the same way), so these are the anchor
    /// instance's, in first-read order.
    pub live_in: Vec<String>,
    /// Names bound inside the run and read after it — the values an extraction
    /// would return. Whether a bound name is read afterwards differs per
    /// instance (the names are α-renamed and the surrounding code differs), so
    /// these are the names of the first instance attaining [`Self::max_live_out`]
    /// (the anchor whenever it does — it wins ties), in first-read order, so the
    /// count and the names always agree.
    pub live_out: Vec<String>,
    /// The greatest live-out count across the group's instances — the input to
    /// the multi-return downgrade.
    pub max_live_out: usize,
}

/// Resolves the [`Liveness`] of statement-run groups, lazily walking each
/// file's fns once on first use (mirrors `super::meta::MetaResolver` /
/// `super::capture::LiteralTables`).
pub struct LivenessAnalyzer<'a> {
    asts: HashMap<&'a Path, &'a syn::File>,
    fns: HashMap<PathBuf, Vec<FnSpan<'a>>>,
}

/// One fn's signature and body, kept borrowed so the liveness walk can reach
/// its bindings and uses. Keyed for enclosure by token position.
struct FnSpan<'a> {
    /// Signature start — precedes every position in the body.
    start: LineColumn,
    /// Body end (closing brace).
    end: LineColumn,
    sig: &'a syn::Signature,
    block: &'a syn::Block,
}

impl<'a> LivenessAnalyzer<'a> {
    /// `files` must be the same scan set the groups were found in.
    pub fn new(files: &'a [ScanFile]) -> Self {
        Self {
            asts: files
                .iter()
                .map(|f| (f.rel_path.as_path(), &f.ast))
                .collect(),
            fns: HashMap::new(),
        }
    }

    /// The extraction signature of one group. `None` unless the group is a
    /// statement run whose anchor instance resolves an enclosing fn (a run
    /// inside a `const`/`static` initializer block has none).
    pub fn analyze(&mut self, group: &CloneGroup) -> Option<Liveness> {
        if group.instances.first()?.kind != CandidateKind::Run {
            return None;
        }
        // The anchor grounds live-in (identical across instances) and seeds
        // the live-out maximum; later instances can only raise it.
        let (live_in, anchor_out) = self.instance(&group.instances[0])?;
        let mut max_live_out = anchor_out.len();
        let mut live_out = anchor_out;
        for inst in &group.instances[1..] {
            if let Some((_, out)) = self.instance(inst)
                && out.len() > max_live_out
            {
                max_live_out = out.len();
                live_out = out;
            }
        }
        Some(Liveness {
            live_in,
            live_out,
            max_live_out,
        })
    }

    /// One instance's `(live_in, live_out)` name lists, or `None` when no
    /// enclosing fn contains the region.
    fn instance(&mut self, region: &Region) -> Option<(Vec<String>, Vec<String>)> {
        self.ensure_fns(&region.file);
        let fns = self.fns.get(&region.file)?;
        let (start, end) = region.bounds();
        // Innermost enclosing fn: the greatest signature-start that still
        // contains the whole region (an fn-local fn beats its host).
        let enclosing = fns
            .iter()
            .filter(|f| f.start <= start && end <= f.end)
            .max_by(|a, b| a.start.cmp(&b.start))?;

        // Every binding the fn introduces, with position. Signature params
        // sort before the body because their spans precede the block.
        let mut binds: Vec<(String, LineColumn)> = Vec::new();
        let mut bc = BindPosCollector { out: &mut binds };
        bc.visit_signature(enclosing.sig);
        bc.visit_block(enclosing.block);
        let bind_positions: HashSet<(usize, usize)> =
            binds.iter().map(|(_, p)| (p.line, p.column)).collect();

        // Every name read in the body, minus the binding occurrences
        // themselves (a `let total` pattern is a bind, not a read of `total`).
        let mut uses: Vec<(String, LineColumn)> = Vec::new();
        let mut state = NormState::default();
        scan_idents(
            enclosing.block.to_token_stream(),
            &mut state,
            &mut |ident, pos| {
                if !bind_positions.contains(&(pos.line, pos.column)) {
                    uses.push((ident.to_string(), pos));
                }
            },
        );

        let mut live_in = Vec::new();
        let mut live_out = Vec::new();
        for (name, pos) in &uses {
            let Some(reaching) = reaching_bind(&binds, name, *pos) else {
                continue; // a free item (a called fn, a type) — never a local
            };
            if *pos >= start && *pos < end {
                // Read inside the run, resolving to a binding before it.
                if reaching < start {
                    push_unique(&mut live_in, name);
                }
            } else if *pos >= end && reaching >= start && reaching < end {
                // Read after the run, resolving to a binding inside it.
                push_unique(&mut live_out, name);
            }
        }
        Some((live_in, live_out))
    }

    fn ensure_fns(&mut self, file: &Path) {
        if self.fns.contains_key(file) {
            return;
        }
        let table = match self.asts.get(file) {
            Some(ast) => {
                let mut w = FnSpanCollector { out: Vec::new() };
                w.visit_file(ast);
                w.out
            }
            None => Vec::new(),
        };
        self.fns.insert(file.to_path_buf(), table);
    }
}

/// The binding a use resolves to under the flat approximation: the latest
/// binding of the name at an earlier source position.
fn reaching_bind(
    binds: &[(String, LineColumn)],
    name: &str,
    pos: LineColumn,
) -> Option<LineColumn> {
    binds
        .iter()
        .filter(|(n, bp)| n == name && *bp < pos)
        .map(|(_, bp)| *bp)
        .max()
}

/// Append `name` if absent, preserving first-seen order (so a note's names
/// read in source order).
fn push_unique(out: &mut Vec<String>, name: &str) {
    if !out.iter().any(|n| n == name) {
        out.push(name.to_string());
    }
}

/// Collects the fns of one file with their spans, kept borrowed for the
/// liveness walk. The three fn shapes match `super::Collect`'s.
struct FnSpanCollector<'a> {
    out: Vec<FnSpan<'a>>,
}

impl<'a> FnSpanCollector<'a> {
    fn record(&mut self, sig: &'a syn::Signature, block: &'a syn::Block) {
        self.out.push(FnSpan {
            start: sig.span().start(),
            end: block.span().end(),
            sig,
            block,
        });
    }
}

impl<'a> Visit<'a> for FnSpanCollector<'a> {
    fn visit_item_fn(&mut self, f: &'a syn::ItemFn) {
        self.record(&f.sig, &f.block);
        visit::visit_item_fn(self, f);
    }

    fn visit_impl_item_fn(&mut self, f: &'a syn::ImplItemFn) {
        self.record(&f.sig, &f.block);
        visit::visit_impl_item_fn(self, f);
    }

    fn visit_trait_item_fn(&mut self, f: &'a syn::TraitItemFn) {
        if let Some(block) = &f.default {
            self.record(&f.sig, block);
        }
        visit::visit_trait_item_fn(self, f);
    }
}

/// Collects every `PatIdent` binding with its position (the positional sibling
/// of `super::BindCollector`).
struct BindPosCollector<'a> {
    out: &'a mut Vec<(String, LineColumn)>,
}

impl<'ast> Visit<'ast> for BindPosCollector<'_> {
    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
        self.out.push((p.ident.to_string(), p.ident.span().start()));
        visit::visit_pat_ident(self, p);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Options, find_clones};
    use super::*;

    fn scan(src: &str) -> ScanFile {
        ScanFile {
            rel_path: PathBuf::from("src/lib.rs"),
            krate: "demo".into(),
            ast: syn::parse_file(src).expect("valid source"),
        }
    }

    fn opts() -> Options {
        Options {
            min_lines: 1,
            min_tokens: 1,
            min_instances: 2,
            ignore_literals: true,
            ignore_test_code: false,
            cross_crate_only: false,
            min_distinct_anchors: 0,
            min_non_repeating_ratio: 0.0,
        }
    }

    /// Analyze the single run group two structurally identical fns produce.
    fn liveness_of(src: &str) -> Liveness {
        let files = vec![scan(src)];
        let groups = find_clones(&files, &opts());
        let run = groups
            .iter()
            .find(|g| g.instances[0].kind == CandidateKind::Run)
            .expect("a statement-run group");
        let mut a = LivenessAnalyzer::new(&files);
        a.analyze(run).expect("run resolves an enclosing fn")
    }

    #[test]
    fn params_are_read_before_returns_are_read_after() {
        // The run reads `items`/`config` (bound before) and binds `total`
        // (read after) — the canonical 2-in / 1-out shape.
        let src = r#"
            fn summarize(items: &[u32], config: &Config) -> u32 {
                let offset = base_offset();
                let mut total = 0u32;
                for value in items.iter() {
                    let weighted = value.wrapping_mul(config.factor);
                    total = total.wrapping_add(weighted);
                }
                report_summary(total);
                total.wrapping_sub(offset)
            }
            fn aggregate(items: &[u32], config: &Config) -> u32 {
                let offset = base_ceiling();
                let mut total = 0u32;
                for value in items.iter() {
                    let weighted = value.wrapping_mul(config.factor);
                    total = total.wrapping_add(weighted);
                }
                record_total(total);
                total.wrapping_sub(offset)
            }
        "#;
        let lv = liveness_of(src);
        assert_eq!(lv.live_in, vec!["items", "config"]);
        assert_eq!(lv.live_out, vec!["total"]);
        assert_eq!(lv.max_live_out, 1);
    }

    #[test]
    fn multiple_values_read_after_are_all_live_out() {
        let src = r#"
            fn stats_a(items: &[u32]) -> (u32, u32) {
                let tag = origin_a();
                let mut count = 0u32;
                let mut total = 0u32;
                for value in items.iter() {
                    count = count.wrapping_add(1);
                    total = total.wrapping_add(*value);
                }
                emit_a(tag);
                (count, total)
            }
            fn stats_b(items: &[u32]) -> (u32, u32) {
                let tag = origin_b();
                let mut count = 0u32;
                let mut total = 0u32;
                for value in items.iter() {
                    count = count.wrapping_add(1);
                    total = total.wrapping_add(*value);
                }
                emit_b(tag);
                (count, total)
            }
        "#;
        let lv = liveness_of(src);
        assert_eq!(lv.live_in, vec!["items"]);
        assert_eq!(lv.live_out, vec!["count", "total"]);
        assert_eq!(lv.max_live_out, 2);
    }

    #[test]
    fn max_live_out_is_taken_across_instances_with_its_names() {
        // The first instance escapes only `total`; the second also reads
        // `total` after but through a differing tail — still one live-out.
        // A third instance (by construction identical run) that escapes two
        // values would raise the max; here both escape one, so anchor wins.
        let src = r#"
            fn one(items: &[u32]) -> u32 {
                let seed = pick_one();
                let mut total = 0u32;
                for value in items.iter() {
                    total = total.wrapping_add(*value);
                    total = total.wrapping_mul(2);
                }
                done_one(seed);
                total
            }
            fn two(items: &[u32]) -> u32 {
                let seed = pick_two();
                let mut total = 0u32;
                for value in items.iter() {
                    total = total.wrapping_add(*value);
                    total = total.wrapping_mul(2);
                }
                done_two(seed);
                total
            }
        "#;
        let lv = liveness_of(src);
        assert_eq!(lv.live_out, vec!["total"]);
        assert_eq!(lv.max_live_out, 1);
    }

    #[test]
    fn a_run_in_a_nested_block_resolves_its_enclosing_fn() {
        // The matching run — `let mut scale` + the `for` — sits inside an `if`
        // body (the `return` after it differs between the fns, so it bounds the
        // run). Containment still finds the enclosing fn: `items` (a param) is
        // live-in and the branch-local `scale`, read by the differing return,
        // is live-out. `flag` precedes the run, so it is not live-in.
        let src = r#"
            fn outer_a(items: &[u32], flag: bool) -> u32 {
                if flag {
                    let mut scale = 0u32;
                    for value in items.iter() {
                        scale = scale.wrapping_add(*value);
                    }
                    return scale.wrapping_mul(head_a());
                }
                tail_a()
            }
            fn outer_b(items: &[u32], flag: bool) -> u32 {
                if flag {
                    let mut scale = 0u32;
                    for value in items.iter() {
                        scale = scale.wrapping_add(*value);
                    }
                    return scale.wrapping_mul(head_b());
                }
                tail_b()
            }
        "#;
        let lv = liveness_of(src);
        assert_eq!(lv.live_in, vec!["items"]);
        assert_eq!(lv.live_out, vec!["scale"]);
    }

    #[test]
    fn a_non_run_group_has_no_liveness() {
        // Two byte-identical fns form a Fn group, not a Run group.
        let src = r#"
            fn twin_one(x: u32) -> u32 {
                let a = x.wrapping_add(7);
                let b = a.wrapping_mul(3);
                b.wrapping_sub(1)
            }
            fn twin_two(x: u32) -> u32 {
                let a = x.wrapping_add(7);
                let b = a.wrapping_mul(3);
                b.wrapping_sub(1)
            }
        "#;
        let files = vec![scan(src)];
        let groups = find_clones(&files, &opts());
        let fn_group = groups
            .iter()
            .find(|g| g.instances[0].kind == CandidateKind::Fn)
            .expect("a fn group");
        let mut a = LivenessAnalyzer::new(&files);
        assert_eq!(a.analyze(fn_group), None);
    }

    #[test]
    fn field_and_path_names_are_not_counted_as_uses() {
        // `config.factor` reads `config` (live-in) but not `factor`; a run
        // reading only field/path names would have no live-in.
        let src = r#"
            fn read_a(config: &Config) -> u32 {
                let start = origin_a();
                let mut acc = 0u32;
                for _ in 0..config.count {
                    acc = acc.wrapping_add(config.step);
                }
                yield_a(start);
                acc
            }
            fn read_b(config: &Config) -> u32 {
                let start = origin_b();
                let mut acc = 0u32;
                for _ in 0..config.count {
                    acc = acc.wrapping_add(config.step);
                }
                yield_b(start);
                acc
            }
        "#;
        let lv = liveness_of(src);
        // `config` is live-in; `factor`/`count`/`step` (field names) are not,
        // and `acc` is the sole live-out.
        assert_eq!(lv.live_in, vec!["config"]);
        assert_eq!(lv.live_out, vec!["acc"]);
    }

    #[test]
    fn shadowed_inner_binding_over_counts_live_out() {
        // Documents the flat-scope approximation: the inner `acc` (loop-local)
        // never escapes, but the after-run read of the OUTER `acc` resolves to
        // it, so live-out is over-counted as {count, acc}.
        let src = r#"
            fn shadow_a(items: &[u32]) -> u32 {
                let acc = seed_a();
                let mut count = 0u32;
                for value in items.iter() {
                    let acc = value.wrapping_mul(2);
                    count = count.wrapping_add(acc);
                }
                finish_a(count);
                acc.wrapping_add(count)
            }
            fn shadow_b(items: &[u32]) -> u32 {
                let acc = seed_b();
                let mut count = 0u32;
                for value in items.iter() {
                    let acc = value.wrapping_mul(2);
                    count = count.wrapping_add(acc);
                }
                finish_b(count);
                acc.wrapping_add(count)
            }
        "#;
        let lv = liveness_of(src);
        // Truly only `count` escapes; the approximation adds `acc`.
        assert_eq!(lv.live_out, vec!["count", "acc"]);
        assert_eq!(lv.max_live_out, 2);
    }
}
