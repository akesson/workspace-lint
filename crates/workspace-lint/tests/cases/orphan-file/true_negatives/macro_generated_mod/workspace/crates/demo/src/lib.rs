// TRUE NEGATIVE (orphan-file) — the `mod` declaration only exists after macro
// expansion, so no syntactic walker can find `macro_made.rs`. rustc compiles it.
macro_rules! declare {
    ($n:ident) => {
        mod $n;
    };
}
declare!(macro_made);

pub fn go() -> u32 {
    macro_made::v()
}
