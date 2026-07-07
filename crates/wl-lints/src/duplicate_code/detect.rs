//! The name-invariant clone finder behind `duplicate-code`.
//!
//! Type-2 clone detection: two regions match when their *structure* is
//! identical even if local variable names and literal values differ. The
//! mechanism is normalization-before-hashing — every candidate region (a whole
//! fn, any nested block, or any run of ≥2 consecutive statements) is flattened
//! to a canonical token sequence in which local bindings are α-renamed to
//! positional placeholders (`#0`, `#1`, … in first-occurrence order) and
//! literals are abstracted to per-kind placeholders. Identical sequences
//! bucket together by hash; no pairwise comparison ever happens.
//!
//! Statement *runs* are what keep a real-world clone in one piece: a copied
//! span that sits mid-body in two otherwise-different fns aligns with no
//! single AST node, and without run candidates it fragments into whichever
//! nested blocks happen to clear the thresholds (observed in the wild as k
//! parallel groups at a constant line offset). The maximal matching run is a
//! candidate of its own, and subsumption then swallows the fragments.
//!
//! What is deliberately *kept verbatim* — the semantic anchors that make a
//! match meaningful rather than merely isomorphic:
//! - free identifiers: called functions, types, field and method names, enum
//!   variants, path segments. Two blocks that call different functions are
//!   not clones.
//! - `true`/`false` (idents at token level) and lifetime names.
//!
//! Known approximations (all lean toward *missing* a clone or the
//! `known_false_positives` bucket, never toward unsoundness):
//! - "local" is judged by a flat per-candidate binding set (every `PatIdent`
//!   in the region plus the fn's own name), not a real scope stack — a name
//!   that is a binding in one inner scope and a free item elsewhere in the
//!   same candidate is renamed everywhere.
//! - struct-pattern shorthand (`Point { x }`) renames `x`, erasing the field
//!   identity in the pattern position.
//! - generic type parameters are not renamed (they are types, not `PatIdent`
//!   bindings), so clones differing only in a type-param name don't match.
//! - macro invocations are normalized as raw token sequences with the same
//!   rules; binding positions *inside* a macro's own grammar can't be known,
//!   so names introduced there stay verbatim (the under-matching direction).
//! - `macro_rules!` bodies are never scanned (their tokens aren't Rust items).
//! - statement-run candidates collect bindings in *prefix* order (a `let`
//!   α-renames its name from that statement on) rather than the flat
//!   per-candidate set — prefix order is what makes one incremental pass per
//!   start statement affordable, and it is the more scoping-faithful choice.
//!   Names bound *outside* the run (fn params, earlier `let`s) stay verbatim,
//!   so a run only matches where its context spells those names identically —
//!   again the under-matching direction.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hasher};
use std::path::PathBuf;

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use quote::ToTokens;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

use crate::shipped_source::{has_test_attr, is_cfg_test, item_attrs};

/// One already-parsed source file to scan. `rel_path` is the
/// workspace-relative display path (what diagnostics anchor on and what the
/// suppression scanner round-trips); `krate` is the owning member's cargo
/// package name, used by the `cross-crate-only` filter and shown in notes.
pub(crate) struct ScanFile {
    pub rel_path: PathBuf,
    pub krate: String,
    pub ast: syn::File,
}

/// Detection thresholds, mirrored 1:1 from `DuplicateCodeConfig`.
pub(crate) struct Options {
    /// Minimum source lines a region must span to be a candidate.
    pub min_lines: u32,
    /// Minimum normalized-token count — guards against dense one-liners that
    /// clear the line bar (a match arm list, a builder chain).
    pub min_tokens: usize,
    /// Minimum number of instances for a group to be reported.
    pub min_instances: usize,
    /// Abstract literal values to per-kind placeholders (`#int`, `#str`, …).
    pub ignore_literals: bool,
    /// Skip `#[cfg(test)]` items and `#[test]`-marked fns. (Whole dev
    /// targets — `tests/`, `benches/`, `examples/` — are filtered by the
    /// caller at enumeration time, where cargo target kinds are known.)
    pub ignore_test_code: bool,
    /// Report only groups whose instances span at least two crates.
    pub cross_crate_only: bool,
}

/// One occurrence of a clone: a line range in a file.
#[derive(Clone, Debug)]
pub(crate) struct Region {
    pub file: PathBuf,
    pub krate: String,
    pub line_start: u32,
    pub line_end: u32,
}

/// A set of structurally identical regions (≥ `min_instances`), sorted by
/// (file, line). `tokens` is the shared normalized-token weight.
pub(crate) struct CloneGroup {
    pub instances: Vec<Region>,
    pub tokens: usize,
}

/// Find all clone groups across `files`. Deterministic: groups are ordered by
/// their first instance's (file, line), instances within a group by the same
/// key.
pub(crate) fn find_clones(files: &[ScanFile], opts: &Options) -> Vec<CloneGroup> {
    // Bucket every candidate by (fingerprint, token count). The token count
    // in the key means a 64-bit hash collision would additionally need equal
    // lengths to produce a false group.
    let mut buckets: HashMap<(u64, usize), Vec<Region>> = HashMap::new();
    // One interner across all files: run fingerprints must agree cross-file,
    // so the symbol ids they hash have to come from a shared table.
    let mut interner = Interner::default();
    for file in files {
        let mut collect = Collect {
            opts,
            file,
            interner: &mut interner,
            out: Vec::new(),
        };
        collect.visit_file(&file.ast);
        for cand in collect.out {
            buckets
                .entry((cand.fingerprint, cand.tokens))
                .or_default()
                .push(cand.region);
        }
    }

    let mut groups: Vec<CloneGroup> = buckets
        .into_iter()
        .filter_map(|((_, tokens), mut instances)| {
            if instances.len() < opts.min_instances {
                return None;
            }
            if opts.cross_crate_only {
                let crates: HashSet<&str> = instances.iter().map(|r| r.krate.as_str()).collect();
                if crates.len() < 2 {
                    return None;
                }
            }
            instances.sort_by(|a, b| (&a.file, a.line_start).cmp(&(&b.file, b.line_start)));
            Some(CloneGroup { instances, tokens })
        })
        .collect();

    // Subsumption: a fn candidate and its own body block (or any nested
    // block) match wherever the parent matches, producing a redundant inner
    // group. Accept groups largest-first and drop any group *every* instance
    // of which lies inside an already-accepted region — a group with even one
    // uncovered instance still carries new information (the inner block was
    // copied somewhere the whole parent was not) and is kept whole.
    groups.sort_by(|a, b| {
        b.tokens.cmp(&a.tokens).then_with(|| {
            let ka = (&a.instances[0].file, a.instances[0].line_start);
            let kb = (&b.instances[0].file, b.instances[0].line_start);
            ka.cmp(&kb)
        })
    });
    let mut accepted: Vec<CloneGroup> = Vec::new();
    let mut covered: HashMap<PathBuf, Vec<(u32, u32)>> = HashMap::new();
    for group in groups {
        let dominated = group.instances.iter().all(|r| {
            covered.get(&r.file).is_some_and(|ranges| {
                ranges
                    .iter()
                    .any(|&(ls, le)| r.line_start >= ls && r.line_end <= le)
            })
        });
        if dominated {
            continue;
        }
        for r in &group.instances {
            covered
                .entry(r.file.clone())
                .or_default()
                .push((r.line_start, r.line_end));
        }
        accepted.push(group);
    }

    accepted.sort_by(|a, b| {
        (&a.instances[0].file, a.instances[0].line_start)
            .cmp(&(&b.instances[0].file, b.instances[0].line_start))
    });
    accepted
}

/// A candidate region with its normalized fingerprint.
struct Candidate {
    fingerprint: u64,
    tokens: usize,
    region: Region,
}

/// Walks one file's AST, recording a candidate for every fn (signature +
/// body, the fn's own name α-renamed so a renamed copy still matches), every
/// nested block, and every run of consecutive statements that clears the
/// size thresholds.
struct Collect<'a> {
    opts: &'a Options,
    file: &'a ScanFile,
    interner: &'a mut Interner,
    out: Vec<Candidate>,
}

impl<'a> Collect<'a> {
    /// Record a fn-shaped candidate: signature + body, with the fn's own
    /// name in the binding set. Including the signature makes the match
    /// stricter (param/return types must agree structurally); the body-only
    /// view of the same code is separately collected via `visit_block`, so a
    /// same-body-different-signature pair is still caught there.
    fn candidate_fn(&mut self, sig: &syn::Signature, block: &syn::Block) {
        let mut binds = collect_binds_sig(sig);
        binds.insert(sig.ident.to_string());
        collect_binds(block, &mut binds);

        let mut tokens = sig.to_token_stream();
        tokens.extend(block.to_token_stream());

        let line_start = sig.span().start().line as u32;
        let line_end = block.span().end().line as u32;
        self.push_candidate(tokens, &binds, line_start, line_end);
    }

    fn candidate_block(&mut self, block: &syn::Block) {
        let mut binds = HashSet::new();
        collect_binds(block, &mut binds);
        let line_start = block.span().start().line as u32;
        let line_end = block.span().end().line as u32;
        self.push_candidate(block.to_token_stream(), &binds, line_start, line_end);
    }

    /// Record a candidate for every run of ≥2 consecutive statements. This is
    /// what finds a clone *maximally* when it is a statement span mid-body in
    /// two fns that differ elsewhere: no single fn/block candidate covers it,
    /// and the block-shaped pieces inside it would otherwise surface as
    /// parallel fragment groups. The full run `[0..n)` is included on purpose
    /// — brace-less, it is what lets a whole block at one site match a
    /// sub-run of a longer block at another.
    ///
    /// Cost control: each statement is flattened ONCE to pre-normalized
    /// integer tokens ([`FlatTok`]: rename eligibility baked in, idents
    /// interned), then one incremental pass per start statement accrues
    /// bindings in prefix order and snapshots the fingerprint at every
    /// statement boundary. The quadratic part touches no strings and
    /// allocates nothing per token.
    fn candidate_stmt_runs(&mut self, block: &syn::Block) {
        let stmts = &block.stmts;
        if stmts.len() < 2 {
            return;
        }
        let flat: Vec<Vec<FlatTok>> = stmts
            .iter()
            .map(|s| {
                let mut toks = Vec::new();
                let mut state = NormState::default();
                flatten(
                    s.to_token_stream(),
                    self.opts.ignore_literals,
                    self.interner,
                    &mut state,
                    &mut toks,
                );
                toks
            })
            .collect();
        let stmt_binds: Vec<Vec<u32>> = stmts
            .iter()
            .map(|s| {
                let mut names = HashSet::new();
                collect_binds_stmt(s, &mut names);
                names.iter().map(|n| self.interner.intern(n)).collect()
            })
            .collect();
        let spans: Vec<(u32, u32)> = stmts
            .iter()
            .map(|s| {
                let span = s.span();
                (span.start().line as u32, span.end().line as u32)
            })
            .collect();

        for start in 0..stmts.len() - 1 {
            // Small linear structures beat hash maps here: bind sets and
            // rename tables are tiny, and this loop is the hot quadratic.
            let mut binds: Vec<u32> = Vec::new();
            let mut rename: Vec<u32> = Vec::new();
            let mut hasher = DefaultHasher::new();
            let mut count = 0usize;
            let line_start = spans[start].0;
            for end in start..stmts.len() {
                binds.extend(&stmt_binds[end]);
                for &t in &flat[end] {
                    count += 1;
                    match t {
                        FlatTok::Open(d) => {
                            hasher.write_u8(1);
                            hasher.write_u8(d);
                        }
                        FlatTok::Close(d) => {
                            hasher.write_u8(2);
                            hasher.write_u8(d);
                        }
                        FlatTok::Ident { sym, renameable } => {
                            if renameable && binds.contains(&sym) {
                                let n =
                                    rename.iter().position(|&s| s == sym).unwrap_or_else(|| {
                                        rename.push(sym);
                                        rename.len() - 1
                                    });
                                hasher.write_u8(3);
                                hasher.write_u32(n as u32);
                            } else {
                                hasher.write_u8(4);
                                hasher.write_u32(sym);
                            }
                        }
                        FlatTok::Punct(c) => {
                            hasher.write_u8(5);
                            hasher.write_u32(c as u32);
                        }
                        FlatTok::Lit(sym) => {
                            hasher.write_u8(6);
                            hasher.write_u32(sym);
                        }
                    }
                }
                if end == start {
                    continue; // a single statement is not a run
                }
                let line_end = spans[end].1;
                if line_end.saturating_sub(line_start) + 1 < self.opts.min_lines {
                    continue;
                }
                self.push_region((hasher.clone().finish(), count), line_start, line_end);
            }
        }
    }

    fn push_candidate(
        &mut self,
        tokens: TokenStream,
        binds: &HashSet<String>,
        line_start: u32,
        line_end: u32,
    ) {
        if line_end.saturating_sub(line_start) + 1 < self.opts.min_lines {
            return;
        }
        let mut rename = HashMap::new();
        let mut state = NormState::default();
        let mut fp = Fingerprint::default();
        walk(
            tokens,
            binds,
            self.opts.ignore_literals,
            &mut rename,
            &mut state,
            &mut fp,
        );
        self.push_region(fp.snapshot(), line_start, line_end);
    }

    fn push_region(&mut self, (fingerprint, tokens): (u64, usize), line_start: u32, line_end: u32) {
        if tokens < self.opts.min_tokens {
            return;
        }
        self.out.push(Candidate {
            fingerprint,
            tokens,
            region: Region {
                file: self.file.rel_path.clone(),
                krate: self.file.krate.clone(),
                line_start,
                line_end,
            },
        });
    }

    fn skip_test_item(&self, attrs: &[syn::Attribute]) -> bool {
        self.opts.ignore_test_code && (attrs.iter().any(is_cfg_test) || has_test_attr(attrs))
    }
}

impl<'a, 'ast> Visit<'ast> for Collect<'a> {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if self.opts.ignore_test_code && item_attrs(item).iter().any(is_cfg_test) {
            return;
        }
        if let syn::Item::Fn(f) = item
            && self.opts.ignore_test_code
            && has_test_attr(&f.attrs)
        {
            return;
        }
        visit::visit_item(self, item);
    }

    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.candidate_fn(&f.sig, &f.block);
        visit::visit_item_fn(self, f);
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        if self.skip_test_item(&f.attrs) {
            return;
        }
        self.candidate_fn(&f.sig, &f.block);
        visit::visit_impl_item_fn(self, f);
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        if self.skip_test_item(&f.attrs) {
            return;
        }
        if let Some(block) = &f.default {
            self.candidate_fn(&f.sig, block);
        }
        visit::visit_trait_item_fn(self, f);
    }

    fn visit_block(&mut self, b: &'ast syn::Block) {
        self.candidate_block(b);
        self.candidate_stmt_runs(b);
        visit::visit_block(self, b);
    }
}

/// Collect every `PatIdent` binding name in a block's subtree — `let`, `for`,
/// closure, and match-arm patterns — into a flat set (see the module docs for
/// why not a scope stack).
fn collect_binds(block: &syn::Block, binds: &mut HashSet<String>) {
    let mut c = BindCollector { binds };
    c.visit_block(block);
}

/// The binding names a signature introduces (typed param patterns).
fn collect_binds_sig(sig: &syn::Signature) -> HashSet<String> {
    let mut binds = HashSet::new();
    let mut c = BindCollector { binds: &mut binds };
    c.visit_signature(sig);
    binds
}

/// The binding names one statement introduces, accrued into a run's prefix
/// binding set.
fn collect_binds_stmt(stmt: &syn::Stmt, binds: &mut HashSet<String>) {
    let mut c = BindCollector { binds };
    c.visit_stmt(stmt);
}

struct BindCollector<'a> {
    binds: &'a mut HashSet<String>,
}

impl<'ast> Visit<'ast> for BindCollector<'_> {
    fn visit_pat_ident(&mut self, p: &'ast syn::PatIdent) {
        self.binds.insert(p.ident.to_string());
        visit::visit_pat_ident(self, p);
    }
}

/// String → dense symbol id, shared across every file in one `find_clones`
/// call so interned ids are comparable cross-file.
#[derive(Default)]
struct Interner(HashMap<String, u32>);

impl Interner {
    fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.0.get(s) {
            return id;
        }
        let id = self.0.len() as u32;
        self.0.insert(s.to_string(), id);
        id
    }
}

/// One pre-normalized token for the statement-run pass. Everything the
/// contextual rules can decide per-token is decided at flatten time — the
/// rename-suppression verdict (`renameable`) and the literal abstraction —
/// so the quadratic per-start pass only does integer compares. Statement
/// boundaries carry no `NormState` across them (every statement ends in `;`
/// or a brace group, both of which reset the state), which is what makes
/// per-statement flattening equivalent to one continuous walk.
#[derive(Clone, Copy)]
enum FlatTok {
    Open(u8),
    Close(u8),
    Ident { sym: u32, renameable: bool },
    Punct(char),
    Lit(u32),
}

/// Flatten `tokens` into [`FlatTok`]s, mirroring [`walk`]'s state machine.
fn flatten(
    tokens: TokenStream,
    ignore_literals: bool,
    interner: &mut Interner,
    state: &mut NormState,
    out: &mut Vec<FlatTok>,
) {
    for tt in tokens {
        match tt {
            TokenTree::Group(g) => {
                let d = g.delimiter() as u8;
                out.push(FlatTok::Open(d));
                state.reset();
                flatten(g.stream(), ignore_literals, interner, state, out);
                out.push(FlatTok::Close(d));
                state.reset();
            }
            TokenTree::Ident(i) => {
                out.push(FlatTok::Ident {
                    sym: interner.intern(&i.to_string()),
                    renameable: !state.suppress_rename(),
                });
                state.reset();
            }
            TokenTree::Punct(p) => {
                let c = p.as_char();
                let colons = if c == ':' { state.colons_run + 1 } else { 0 };
                let dots = if c == '.' { state.dots_run + 1 } else { 0 };
                state.reset();
                state.colons_run = colons;
                state.dots_run = dots;
                state.after_quote = c == '\'';
                out.push(FlatTok::Punct(c));
            }
            TokenTree::Literal(l) => {
                let text = if ignore_literals {
                    interner.intern(literal_placeholder(&l))
                } else {
                    interner.intern(&l.to_string())
                };
                out.push(FlatTok::Lit(text));
                state.reset();
            }
        }
    }
}

/// Streaming normalized-token fingerprint for the one-shot fn/block
/// candidates: [`walk`] pushes each normalized token straight into the
/// hasher, so no `Vec<String>` sequence is ever materialized.
#[derive(Default)]
struct Fingerprint {
    hasher: DefaultHasher,
    tokens: usize,
}

impl Fingerprint {
    fn push(&mut self, tok: &str) {
        self.hasher.write(tok.as_bytes());
        // Delimit (0xFF never occurs in UTF-8) so ["ab","c"] ≠ ["a","bc"].
        self.hasher.write_u8(0xFF);
        self.tokens += 1;
    }

    fn snapshot(&self) -> (u64, usize) {
        (self.hasher.clone().finish(), self.tokens)
    }
}

/// Punctuation context feeding the ident-rename suppression rules. All three
/// exist to tell a *use of a local* apart from token shapes where the same
/// spelling is not a local reference:
/// - after a single field-access `.` (`user.age` — `age` is a field name);
///   a run of 2+ dots is a range (`0..n`), where the trailing ident IS a
///   local and must still rename.
/// - after `::` (`Foo::new` — path segments are free names). A single `:`
///   does NOT suppress: `let x: u32` binds `x`… and pattern/type-ascription
///   positions vastly outnumber the struct-literal field position this
///   over-renames.
/// - after `'` (lifetime names — kept verbatim, never confused with a local
///   that shares the spelling).
#[derive(Default)]
struct NormState {
    dots_run: usize,
    colons_run: usize,
    after_quote: bool,
}

impl NormState {
    fn suppress_rename(&self) -> bool {
        self.dots_run == 1 || self.colons_run == 2 || self.after_quote
    }

    fn reset(&mut self) {
        self.dots_run = 0;
        self.colons_run = 0;
        self.after_quote = false;
    }
}

fn walk(
    tokens: TokenStream,
    binds: &HashSet<String>,
    ignore_literals: bool,
    rename: &mut HashMap<String, usize>,
    state: &mut NormState,
    out: &mut Fingerprint,
) {
    for tt in tokens {
        match tt {
            TokenTree::Group(g) => {
                let (open, close) = delimiters(g.delimiter());
                out.push(open);
                state.reset();
                walk(g.stream(), binds, ignore_literals, rename, state, out);
                out.push(close);
                state.reset();
            }
            TokenTree::Ident(i) => {
                let s = i.to_string();
                if !state.suppress_rename() && binds.contains(&s) {
                    let next = rename.len();
                    let n = *rename.entry(s).or_insert(next);
                    out.push(&format!("#{n}"));
                } else {
                    out.push(&s);
                }
                state.reset();
            }
            TokenTree::Punct(p) => {
                let c = p.as_char();
                let colons = if c == ':' { state.colons_run + 1 } else { 0 };
                let dots = if c == '.' { state.dots_run + 1 } else { 0 };
                state.reset();
                state.colons_run = colons;
                state.dots_run = dots;
                state.after_quote = c == '\'';
                out.push(c.encode_utf8(&mut [0u8; 4]));
            }
            TokenTree::Literal(l) => {
                if ignore_literals {
                    out.push(literal_placeholder(&l));
                } else {
                    out.push(&l.to_string());
                }
                state.reset();
            }
        }
    }
}

fn delimiters(d: Delimiter) -> (&'static str, &'static str) {
    match d {
        Delimiter::Parenthesis => ("(", ")"),
        Delimiter::Bracket => ("[", "]"),
        Delimiter::Brace => ("{", "}"),
        // Invisible groups don't occur in freshly-parsed source, but cost
        // nothing to keep structural.
        Delimiter::None => ("∅(", ")∅"),
    }
}

/// Per-kind literal placeholder. `true`/`false` never reach here (they are
/// idents at token level, kept verbatim by design — flag semantics differ).
fn literal_placeholder(l: &proc_macro2::Literal) -> &'static str {
    match syn::Lit::new(l.clone()) {
        syn::Lit::Str(_) | syn::Lit::ByteStr(_) | syn::Lit::CStr(_) => "#str",
        syn::Lit::Char(_) | syn::Lit::Byte(_) => "#char",
        syn::Lit::Int(_) => "#int",
        syn::Lit::Float(_) => "#float",
        _ => "#lit",
    }
}
