//! Probe module for the `dead_code` deletion-veto substrate (schema 13):
//! variant facts, construction-only variant edges, and the derived
//! `Clone`/`Debug` liveness discount. Every shape here mirrors a class the
//! 2026-07-10 ripgrep drill proved rustc warns on after `--fix-auto-delete`:
//! the extractor's facts must let the assembler see what rustc sees.

/// Variants: `Built` is constructed via its tuple ctor, `Braced` via a
/// struct expression, `MatchedOnly` is named ONLY in a pattern — rustc
/// counts constructions alone, so `MatchedOnly` must receive no edge.
pub enum Signal {
    Built(u32),
    Braced { level: u32 },
    MatchedOnly,
}

pub fn constructs() -> Signal {
    Signal::Built(1)
}

pub fn braces() -> Signal {
    Signal::Braced { level: 2 }
}

pub fn observes(s: &Signal) -> u32 {
    match s {
        Signal::Built(n) => *n,
        Signal::Braced { level } => *level,
        Signal::MatchedOnly => 0,
    }
}

/// Derive-only reads: `shadow` is read ONLY by the derived `Clone`/`Debug`
/// impls (rustc still warns `field shadow is never read` — the extractor
/// must emit NO read edge for it), while `lit` is also read by a getter of
/// the SAME NAME — whose display path equals the field's, the shape the
/// path-level self-edge dedup used to swallow (ripgrep's `Glob::from`).
#[derive(Clone, Debug)]
pub struct Meter {
    shadow: u32,
    lit: u32,
}

impl Meter {
    pub fn new() -> Meter {
        Meter { shadow: 0, lit: 0 }
    }

    pub fn lit(&self) -> u32 {
        self.lit
    }
}

/// Derived `Clone` clones (constructs) every variant; rustc discounts it, so
/// `CloneOnly` must receive no construction edge — only `Used` has one.
#[derive(Clone)]
pub enum Sorted {
    Used,
    CloneOnly(u32),
}

pub fn make_sorted() -> Sorted {
    Sorted::Used
}

pub fn observe_sorted(s: &Sorted) -> u32 {
    match s {
        Sorted::Used => 0,
        Sorted::CloneOnly(n) => *n,
    }
}
