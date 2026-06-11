// `Calc` is public because `app` uses it cross-crate — but only through an
// inferred return type + a method call, so the syn resolver sees it used only
// inside demo (make's signature) and flags it intra-crate. rust-analyzer
// resolves the real cross-crate use, so --fix writes an expect directive
// instead of tightening.
// workspace-lint: expect(unused-pub) -- rust-analyzer sees `demo::Calc` referenced (crates/app/src/main.rs:5)
pub struct Calc {
    v: u8,
}

impl Calc {
    pub fn value(&self) -> u8 {
        self.v
    }
}

pub fn make() -> Calc {
    Calc { v: 7 }
}

// `OnlyHere` is genuinely used only inside demo — rust-analyzer confirms the
// resolver's intra-crate finding, so --fix tightens it to pub(crate).
pub(crate) struct OnlyHere;

fn keep(_: OnlyHere) {}

pub fn seed() {
    keep(OnlyHere);
}
