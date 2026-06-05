// Exercise Layer 2 comment-directive parsing in build_module_from_file. The
// unused-pub lint should not flag `Referenced`: the directive below advertises it
// as referenced by a downstream macro expansion the resolver can't see into.
// workspace-syn: expansion-uses(crate::Referenced)
pub struct Referenced;

macro_rules! my_macro {
    () => {
        let _ = Referenced;
    };
}
pub(crate) use my_macro;
