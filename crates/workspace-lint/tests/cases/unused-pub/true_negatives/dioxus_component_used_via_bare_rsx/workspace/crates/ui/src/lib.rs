// TRUE NEGATIVE (unused-pub) — Dioxus component cross-linking (Phase 4).
//
// `Card` is a `#[component] pub fn` defined in the `card` module and used only
// as a *bare* `Card {}` invocation inside an `rsx!` body. The glob `use
// card::*;` brings it into scope but does NOT create a named use-binding, so the
// only thing referencing `Card` is the bare rsx usage. Both the structured rsx
// walker and the baseline token scan drop single-ident names, so without the
// Phase B dioxus `global_facts` hook `Card` has zero referrers and reads "appears
// unused" — the false positive. The hook binds the bare usage to the same-crate
// `pub fn Card`, making it IntraCrate (suppressed here), so this passes cleanly.

mod card;
use card::*;

// Private (not flagged itself) so the only finding-eligible item is `Card`.
fn _anchor() {
    let _ = rsx! { Card {} };
}
