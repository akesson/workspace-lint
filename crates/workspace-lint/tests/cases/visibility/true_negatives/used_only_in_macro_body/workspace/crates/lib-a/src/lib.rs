// `Helper` looks unused via direct `use` bindings, but a workspace-owned
// macro references `crate::Helper` in its expansion — so Layer 1
// autodetect picks it up and the visibility check correctly stays quiet.

pub struct Helper;

#[macro_export]
macro_rules! invoke_helper {
    () => {
        let _h = $crate::Helper;
    };
}
