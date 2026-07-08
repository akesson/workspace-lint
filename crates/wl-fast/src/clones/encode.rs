//! The normalized-stream vocabulary behind the clone finder: how a token
//! stream becomes pre-normalized [`FlatTok`]s ([`flatten`]) and how those
//! hash into a fingerprint plus the two noise metrics ([`Encoder`]).
//!
//! `detect` owns the *what* (which regions are candidates, how they bind,
//! group, and subsume); this module owns the *how* of turning one region into
//! a comparable, measurable integer stream. See `detect`'s module docs for
//! the normalization rules and their known approximations.

use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hasher};

use proc_macro2::{TokenStream, TokenTree};

/// String → dense symbol id, shared across every file in one `find_clones`
/// call so interned ids are comparable cross-file.
#[derive(Default)]
pub(crate) struct Interner(HashMap<String, u32>);

impl Interner {
    pub(crate) fn intern(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.0.get(s) {
            return id;
        }
        let id = self.0.len() as u32;
        self.0.insert(s.to_string(), id);
        id
    }
}

/// One pre-normalized token. Everything the contextual rules can decide
/// per-token is decided at flatten time — the rename-suppression verdict
/// (`renameable`), the anchor verdict (`anchor`: a verbatim occurrence counts
/// toward anchor diversity iff it isn't a keyword), and the literal
/// abstraction — so the hashing pass only does integer compares. Statement
/// boundaries carry no `NormState` across them (every statement ends in `;`
/// or a brace group, both of which reset the state), which is what makes the
/// run pass's per-statement flattening equivalent to one continuous flatten.
#[derive(Clone, Copy)]
pub(crate) enum FlatTok {
    Open(u8),
    Close(u8),
    Ident {
        sym: u32,
        renameable: bool,
        anchor: bool,
    },
    Punct(char),
    Lit(u32),
}

/// Flatten `tokens` into [`FlatTok`]s — the single normalization pass behind
/// every candidate kind (fn, block, statement run).
pub(crate) fn flatten(
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
                let s = i.to_string();
                out.push(FlatTok::Ident {
                    sym: interner.intern(&s),
                    renameable: !state.suppress_rename(),
                    anchor: !NON_ANCHOR_IDENTS.contains(&s.as_str()),
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

/// Report every ident in a local-reference-shaped position — the same
/// `renameable` verdict [`flatten`] computes (skip the ident after a single
/// field-access `.`, after `::`, and after `'`) — with its span-start
/// position. The liveness pass ([`super::liveness`]) uses this to find which
/// names a statement run reads, applying exactly the fingerprint's
/// suppression rules so a struct field or path segment is never mistaken for
/// a variable use. Recursion into groups deliberately walks macro bodies:
/// `println!("{x}")`-style uses are over-counted rather than missed (the
/// conservative direction for a liveness hint — see [`super::liveness`]).
pub(super) fn scan_idents(
    tokens: TokenStream,
    state: &mut NormState,
    f: &mut impl FnMut(&proc_macro2::Ident, proc_macro2::LineColumn),
) {
    for tt in tokens {
        match tt {
            TokenTree::Group(g) => {
                state.reset();
                scan_idents(g.stream(), state, f);
                state.reset();
            }
            TokenTree::Ident(i) => {
                if !state.suppress_rename() {
                    f(&i, i.span().start());
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
            }
            TokenTree::Literal(_) => state.reset(),
        }
    }
}

/// Keywords and the bool literals never count as anchors: they are structure,
/// not semantic identity. (`true`/`false` and `_` are idents at token level.)
const NON_ANCHOR_IDENTS: &[&str] = &[
    "_", "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true",
    "type", "union", "unsafe", "use", "where", "while", "yield",
];

/// Window width for the repetition metric: long enough that ordinary code
/// rarely echoes a window verbatim, shorter than the typical stamped-out row
/// (`#0 . insert ( #str , #lit ) ;` is 9 tokens), so row N+1 repeats row N's
/// windows.
const KGRAM: usize = 8;
/// Rolling-hash multiplier (any odd constant; this is the FNV-1a prime).
const ROLL_BASE: u64 = 0x100000001b3;
/// `ROLL_BASE^(KGRAM-1)` — the factor that evicts the token leaving the
/// window from the rolling hash.
const ROLL_EVICT: u64 = ROLL_BASE.wrapping_pow(KGRAM as u32 - 1);

/// Per-candidate measurements read off the normalized stream: the grouping
/// key (`fingerprint`, `tokens`) plus the two noise metrics `push_region`
/// gates on. Instances that group together share one normalized stream, so
/// they share these values — filtering per candidate, pre-bucket, is
/// equivalent to filtering groups and cheaper.
pub(crate) struct Measured {
    pub fingerprint: u64,
    pub tokens: usize,
    /// Distinct anchor count, tracked only up to the configured floor (the
    /// only consumer is the `>= floor` gate).
    pub distinct_anchors: usize,
    /// `1 − repeated-windows ⁄ windows` (1.0 when the stream is shorter than
    /// one window).
    pub non_repeating: f64,
}

/// The one normalized-stream encoder behind every candidate kind: hashes each
/// [`FlatTok`] (applying the α-rename against the caller's bind set) and
/// tracks the two noise metrics as it goes. The run pass builds one per start
/// statement and snapshots at every statement boundary — all state only grows
/// as the run extends, so a snapshot is always the measurement of exactly
/// `[start..=end]`.
#[derive(Default)]
pub(crate) struct Encoder {
    hasher: DefaultHasher,
    tokens: usize,
    /// Distinct anchor syms, capped at `anchor_floor` (monotone as the stream
    /// extends, so tracking can stop at the floor).
    anchors: Vec<u32>,
    anchor_floor: usize,
    /// Repetition tracking (skipped entirely when the ratio filter is off):
    /// a rolling Rabin–Karp hash over the last [`KGRAM`] token ids.
    track_repeats: bool,
    ring: [u64; KGRAM],
    window: u64,
    seen: HashSet<u64>,
    windows: usize,
    repeated: usize,
}

impl Encoder {
    pub(crate) fn new(anchor_floor: usize, track_repeats: bool) -> Self {
        Self {
            anchor_floor,
            track_repeats,
            ..Self::default()
        }
    }

    /// Hash one token. `binds` decides α-renaming (membership = rename),
    /// `rename` carries the candidate's first-occurrence numbering. Each
    /// token kind hashes as a tag byte + payload, and the same (tag, payload)
    /// pair packs into a collision-free u64 id for the repetition window.
    pub(crate) fn feed(&mut self, tok: FlatTok, binds: &[u32], rename: &mut Vec<u32>) {
        self.tokens += 1;
        let id: u64 = match tok {
            FlatTok::Open(d) => {
                self.hasher.write_u8(1);
                self.hasher.write_u8(d);
                (1 << 32) | u64::from(d)
            }
            FlatTok::Close(d) => {
                self.hasher.write_u8(2);
                self.hasher.write_u8(d);
                (2 << 32) | u64::from(d)
            }
            FlatTok::Ident {
                sym,
                renameable,
                anchor,
            } => {
                if renameable && binds.contains(&sym) {
                    let n = rename.iter().position(|&s| s == sym).unwrap_or_else(|| {
                        rename.push(sym);
                        rename.len() - 1
                    });
                    self.hasher.write_u8(3);
                    self.hasher.write_u32(n as u32);
                    (3 << 32) | n as u64
                } else {
                    if anchor {
                        self.note_anchor(sym);
                    }
                    self.hasher.write_u8(4);
                    self.hasher.write_u32(sym);
                    (4 << 32) | u64::from(sym)
                }
            }
            FlatTok::Punct(c) => {
                self.hasher.write_u8(5);
                self.hasher.write_u32(c as u32);
                (5 << 32) | u64::from(c as u32)
            }
            FlatTok::Lit(sym) => {
                self.hasher.write_u8(6);
                self.hasher.write_u32(sym);
                (6 << 32) | u64::from(sym)
            }
        };
        if self.track_repeats {
            self.roll(id);
        }
    }

    fn note_anchor(&mut self, sym: u32) {
        if self.anchors.len() < self.anchor_floor && !self.anchors.contains(&sym) {
            self.anchors.push(sym);
        }
    }

    /// Slide the repetition window one token and count a repeat when the
    /// completed window was already seen in this candidate.
    fn roll(&mut self, id: u64) {
        let pos = (self.tokens - 1) % KGRAM;
        if self.tokens > KGRAM {
            let evicted = self.ring[pos];
            self.window = self.window.wrapping_sub(evicted.wrapping_mul(ROLL_EVICT));
        }
        self.ring[pos] = id;
        self.window = self.window.wrapping_mul(ROLL_BASE).wrapping_add(id);
        if self.tokens >= KGRAM {
            self.windows += 1;
            if !self.seen.insert(self.window) {
                self.repeated += 1;
            }
        }
    }

    pub(crate) fn snapshot(&self) -> Measured {
        let non_repeating = if self.windows == 0 {
            1.0
        } else {
            1.0 - self.repeated as f64 / self.windows as f64
        };
        Measured {
            fingerprint: self.hasher.clone().finish(),
            tokens: self.tokens,
            distinct_anchors: self.anchors.len(),
            non_repeating,
        }
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
pub(crate) struct NormState {
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

/// Per-kind literal placeholder. `true`/`false` never reach here (they are
/// idents at token level, kept verbatim by design — flag semantics differ).
/// Shared with `capture`: the divergence pass classifies captured literals
/// with the same function, so its kind-sequences mirror the fingerprint.
pub(super) fn literal_placeholder(l: &proc_macro2::Literal) -> &'static str {
    match syn::Lit::new(l.clone()) {
        syn::Lit::Str(_) | syn::Lit::ByteStr(_) | syn::Lit::CStr(_) => "#str",
        syn::Lit::Char(_) | syn::Lit::Byte(_) => "#char",
        syn::Lit::Int(_) => "#int",
        syn::Lit::Float(_) => "#float",
        _ => "#lit",
    }
}
