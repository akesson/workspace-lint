// `Calc` is used cross-crate by `app`, but only through the `Value` trait's
// `value()` method on an `impl Value` return — its concrete type is never named
// in a public signature. So the structural signature-exposure guard can't see
// it (and must not suppress it), and the syn resolver — which doesn't model
// trait-method dispatch — sees it used only inside demo (the impl block + make's
// body) and flags it intra-crate. rust-analyzer resolves the real cross-crate
// use, so `--fix --scip-index` writes an expect directive instead of tightening.
//
// (Before the signature-exposure guard this fixture returned `Calc` by name; the
// guard now resolves that simpler case structurally, so the SCIP path is
// exercised here with a trait-dispatch use the guard genuinely cannot see.)
pub trait Value {
    fn value(&self) -> u8;
}

pub struct Calc {
    v: u8,
}

impl Value for Calc {
    fn value(&self) -> u8 {
        self.v
    }
}

pub fn make() -> impl Value {
    Calc { v: 7 }
}

// `OnlyHere` is genuinely used only inside demo — rust-analyzer confirms the
// resolver's intra-crate finding, so --fix tightens it to pub(crate).
pub struct OnlyHere;

fn keep(_: OnlyHere) {}

pub fn seed() {
    keep(OnlyHere);
}
