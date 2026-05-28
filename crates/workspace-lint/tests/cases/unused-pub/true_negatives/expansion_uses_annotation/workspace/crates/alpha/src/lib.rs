// Exercise Layer 2 macro annotation parsing in collect_module_contents.
// The unused-pub lint should not flag `Referenced` because the expansion_uses!
// marker advertises it as referenced by some downstream macro.
expansion_uses!(crate::Referenced);

pub struct Referenced;

macro_rules! my_macro {
    () => {
        let _ = Referenced;
    };
}
pub(crate) use my_macro;
