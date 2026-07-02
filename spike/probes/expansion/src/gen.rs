// The `pub` token below lives in THIS file. If any emitted `vis_span` ever
// points here for an item *invoked* from lib.rs, `--fix` would edit the macro
// definition rather than the call site — the exact failure this probe rules
// out. The correct outcome: the generated fn's whole-item `span` maps to the
// lib.rs invocation site (`from_expansion == true`), and its `vis_span` is
// `None`.
macro_rules! make_pub_fn {
    ($name:ident) => {
        pub fn $name() -> u32 {
            42
        }
    };
}
