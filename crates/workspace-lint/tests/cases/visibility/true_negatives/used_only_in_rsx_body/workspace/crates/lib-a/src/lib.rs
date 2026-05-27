// `Button` is pub and only referenced from inside an `rsx! { ... }` body
// in lib-b. The token-scanner picks up the fully-qualified path inside
// the macro body so visibility shouldn't flag this.
pub fn Button() {}
