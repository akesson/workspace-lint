// TRUE NEGATIVE (unused-pub) — Dioxus router cross-linking (Phase 4).
//
// `Home`, `NavBar`, and `Favorites` are `#[component] pub fn`s referenced ONLY
// through the `#[derive(Routable)]` enum below — `Home`/`Favorites` as route
// variant idents, `NavBar` via `#[layout(NavBar)]`. There is deliberately NO
// `rsx!` here, so this isolates the route-attribute capture from the rsx capture:
// the router references live in enum *attributes*, which the token/AST scans
// never visit, so without the Phase A Routable capture each component has zero
// referrers and reads "appears unused" — the false positive this guards against.
// The capture emits them as bare `Origin::Component` occurrences and the Phase B
// `DioxusComponentPass` binds them to the same-crate `pub fn`s, making each
// IntraCrate (suppressed here), so this passes cleanly.

// Private (not a pub item, so not finding-eligible itself); the route attributes
// are the only thing referencing the components.
#[derive(Routable, Clone, PartialEq)]
enum Route {
    #[layout(NavBar)]
    #[route("/")]
    Home,

    #[route("/favorites")]
    Favorites,
}

// Referenced only via the route enum's attributes — never via `rsx!` or `use`.
#[component]
pub fn Home() {}

#[component]
pub fn NavBar() {}

#[component]
pub fn Favorites() {}

// Keep the (private) route enum benign — referencing it here means the enum
// itself isn't a dead-code concern, leaving the components as the only items
// whose "used?" answer depends on the Routable capture.
fn _anchor() {
    let _ = Route::Home;
}
