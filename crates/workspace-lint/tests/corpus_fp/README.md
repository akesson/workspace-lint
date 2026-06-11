# Corpus lint false-positive audit

Committed snapshots of the resolver-backed lints run against the real
third-party crates vendored under `corpus/` (see `../corpus_fp.rs`). Each
`<crate>.stderr` is the **current** diagnostic output. A clean crate has an
empty/`all passed` snapshot; a non-empty one captures findings that are either a
real resolver false positive or a documented limitation.

The snapshot itself is the forcing function: when the resolver improves and a
documented FP stops firing, the snapshot mismatches and this file gets updated —
promoting "known FP" to "fixed". Re-bless after triage with
`WORKSPACE_LINT_BLESS=1 cargo test -p workspace-lint --test corpus_fp`.

Scope: **`unused-deps` on every crate; `unused-pub` on multi-member workspaces
only** (see `corpus_fp.rs` for why — on a standalone single-crate library
`unused-pub` would flag the whole public API by construction, so it yields
meaningful signal only where cross-crate referrers exist).

## Findings (audited 2026-06-01)

Most findings are resolver **false positives** (the resolver missed a real
reference), but not all — `unused-deps` on a genuinely-unused dependency is a
**true positive**, a lint win (see anyhow's `syn`). The audit runs `unused-deps`
on every crate and `unused-pub` on multi-member workspaces (see `corpus_fp.rs`).

> **History (Phase 3, increment 1):** the earlier audit also reported FPs on
> bitflags' `serde_lib`/`serde_test` and hypothesized an "external-glob /
> derive-attribute / cfg-gated-module" gap. That root-cause was **wrong**. The
> real bug was module-file resolution: both deps are referenced in
> `src/external/serde.rs`, reached via `mod serde;` in `src/external.rs`, and the
> resolver didn't implement the `foo.rs`-owns-`foo/` convention, so it never
> loaded the file. Fixing that (see `syn-workspace/src/resolve/module_tree.rs`)
> cleared bitflags entirely — the glob, derive, and cfg-gated-module mechanisms
> all worked once the file was actually traversed.

- **`heck`** — clean. The control crate; must stay `all passed`.

- **`itertools`** — clean. Its one normal dependency, `either`, is referenced by
  path (`use either::Either`) and correctly seen, so `unused-deps` does not flag
  it. (`itertools` also backs the set-level SCIP differential in
  `syn-workspace/tests/oracle.rs`, which independently confirms `either` is
  visible to the resolver.)

- **`anyhow`** — one **true positive**, no FPs:
  - `syn` — **TRUE POSITIVE** (confirmed). An exhaustive search found zero
    references anywhere — not `use syn`, not `syn::`, not in `build.rs`, tests,
    examples, or doc comments. anyhow declares a `syn` dev-dep it genuinely does
    not use; the lint is *correctly* flagging it. (This is why blanket
    `[dev-dependencies]` conservatism would be wrong — it would suppress this
    real finding.)
  - `futures` — **fixed** (was the lone remaining corpus FP). Referenced only
    inside a doc-test (`/// use futures::stream::…` in `src/error.rs:62`). Doc
    comments are now scanned for code fences (see History below), so the dep is
    correctly seen as used.

- **`bitflags`** — clean (`unused-deps`). Previously flagged `serde_lib` +
  `serde_test`; both cleared by the module-file resolution fix (see History
  above). Must stay `all passed`.

- **`thiserror`** (multi-member: `thiserror` lib + `thiserror-impl` proc-macro) —
  clean (`unused-deps` + `unused-pub`). It initially surfaced 8 `unused-pub` FPs
  on *internal* `pub` items that genuinely *are* used; the cross-crate resolution
  was already correct (public API + `#[proc_macro_derive]` entries exempt). Both
  root-cause gaps were then fixed (see History below):
  - **Bare single-ident sibling references** (7): `Source`/`From`/`Transparent`/
    `Fmt` (`Option<Source<'a>>` fields, `Some(Source { … })` literals), the two
    `Sealed` traits (supertrait bounds + `impl Sealed for …`), `Placeholder`.
  - **`use path::{self, …}` group-self import** (1): `attr::get`.

- **`memchr`** — clean (`unused-deps`) after a fix. Its deep, cfg-gated,
  arch-specific module tree (`src/arch/{x86_64,aarch64,wasm32,all,generic}/…`)
  loaded and resolved without trouble. It surfaced **one** real FP: the optional
  `log` dependency, referenced only as `log::debug!`/`log::trace!` inside
  memchr's own `debug!`/`trace!` `macro_rules!` wrappers (`src/macros.rs`).
  memchr also defines a `macro_rules! log` in that same module, and the resolver
  treated that macro name as a sibling that shadowed the external `log` crate in
  the `log::debug` path — so `log` looked unused. Fixed (see History below).

> **History (Phase 3, increment 4):** the two gaps above were closed in
> `syn-workspace`. `extract_code_paths` now keeps a bare single ident that names
> a same-module sibling (not just a `use` binding), so a sibling referenced by
> bare name in a field type / literal / bound / impl is recorded.
> `bindings_from_use` now binds the module for `use path::{self, …}` instead of a
> name called `self`. Both only *add* same-crate (or, for `{self}`, normalized)
> references, so the cross-crate SCIP precision gate is unaffected; the FPs
> reclassify `Unused` → `IntraCrate` and `suppress-intra-crate` drops them.

> **History (Phase 3, increment 5):** anyhow's `futures` FP — a dep used only in
> a `/// use futures::stream::…` doc-test example — was the last corpus FP. The
> resolver now scans rust-compiling code fences in line doc comments (`///` /
> `//!`) for crate-name references (`syn-workspace/src/resolve/doc_fences.rs`).
> These feed the dependency lint **only** (via `Workspace::doctest_dep_refs`),
> deliberately kept out of the occurrence graph: doc-test code is a separate
> compilation unit, so the refs must not reach `unused-pub` or the SCIP
> projection. `text` / `ignore` / `compile_fail` / other-language fences are
> skipped; rustdoc hidden lines (`# `) are scanned. Block doc comments
> (`/** … */`) are a documented non-goal.

- **`regex`** (7-member workspace: `regex` + `regex-automata` / `-syntax` /
  `-lite` / `-cli` / `-capi` / `-test`) — the largest corpus crate (2119 items),
  with genuine intra-workspace cross-crate references (`regex` → `regex-automata`
  → `regex-syntax`): the `unused-pub`-at-scale stress test. It surfaced **3 true
  positives** and **4 false positives** across three limitation classes; all
  three classes were then fixed (see History below), leaving the snapshot at
  **three true positives and no false positives**:
  - **`quickcheck`** — **TRUE POSITIVE**. A `[dev-dependencies]` the *root*
    `regex` crate declares but uses nowhere (`regex-automata` declares its own).
    Same shape as anyhow's `syn`.
  - **`regex_syntax::{perl_decimal,perl_space}::BY_NAME`** — **TRUE POSITIVES**.
    Generated `pub const`s in the private `mod unicode_tables`; the lookups read
    `DECIMAL_NUMBER` / `WHITE_SPACE`, never the `BY_NAME` variant, so these are
    genuinely-unreferenced over-exposed items. (Other tables' `BY_NAME` *are*
    read and are correctly seen as used.)
  - **`aho-corasick`** — **fixed** (was the feature-plumbing FP). The root crate
    declares it only to forward the `perf-literal` feature (`dep:aho-corasick`,
    `aho-corasick?/std`) to `regex-automata`; it is never named in the root
    crate's own code. `unused-deps` now reads the `[features]` table, so a dep
    forwarded via `dep:` / `?/` counts as used (see History below).

- **`dioxus`** (the framework monorepo, pinned to `v0.7.9`; a 112-member
  workspace, the largest in the corpus) — added as the first corpus crate
  carrying real `rsx!` (1100+ invocations), so the smoke gate
  (`syn-workspace/tests/corpus.rs`) stress-tests the `dioxus-rsx 0.7.9` Phase A
  parser on production component trees. Unlike the leaf-library crates above, the
  framework-scale `unused-pub` + `unused-deps` audit surfaces a *large* finding
  set; triage confirmed the lint is **behaving correctly at scale** — every
  finding is a true positive or a documented structural non-goal, with one
  fixable resolver FP class (now fixed). The classes:
  - **One resolver FP — FIXED.** `dioxus_signals`' `default_impl!` / `read_impls!`
    / `fmt_impls!` / `write_impls!` (`#[macro_export] macro_rules!` invoked only by
    bare intra-crate `name!(...)`) read "appears unused" because bare macro
    invocations weren't captured as references. Closed by the core `MacroCallPass`
    (see History); they now read IntraCrate.
  - **Macro-expansion (known-FP, structural non-goal).** `eq_impls!` is invoked
    only as `$crate::eq_impls!{…}` *inside another macro's expansion body*
    (`read_impls!`), never at a real call site — invisible to a resolver that
    doesn't expand macros. Same class as a dep used only inside a
    `#[derive]`/`#[server]` expansion (`expansion_uses!` is the opt-in fix; a
    third-party crate doesn't annotate).
  - **Router cross-linking — FIXED.** HotDog's `DogView` / `NavBar` /
    `Favorites` are `pub fn` components referenced only through a
    `#[derive(Routable)]` enum (`#[route]` / `#[layout(...)]`), not a bare `rsx!`
    invocation, so they read "appears unused". Closed in Phase 4 increment 3 (see
    History): the route component names are captured as `Origin::Component` and the
    existing `DioxusComponentPass` binds them to the same-crate `pub fn` — they now
    read IntraCrate, exactly like a bare `rsx!` component.
  - **Trait-method / derive-via-re-export deps (known-FP — needs trait solving /
    macro expansion).** e.g. `digest` via `Sha256::digest`, `anyhow` via
    `.context()`, `serde` where only `Serialize`/`Deserialize` *derives* appear
    with no `serde::` path (the trait is glob-imported from a prelude). Type
    inference and trait solving are explicit non-goals.
  - **`#[serde(with)]` helper fns — FIXED.** `dioxus_liveview`'s
    `history.rs::routes::{serialize, deserialize}` read "appears unused": they're
    invoked only through `#[serde(with = "routes")]`, code the serde derive
    generates. Closed by the Tier-H `serde-with` assertion (see History), which
    credits the named module plus its `serialize`/`deserialize` children — they
    now read IntraCrate.
  - **Re-export-path deps (known-FP).** `const_format` / `xxhash-rust` /
    `self-replace` are referenced only as `other_crate::dep::…` (a re-export of a
    transitive dep), never via the direct dep's own root segment.
  - **JS-interop exports (true-positive-ish).** `geolocation_native_plugin`'s
    `*Json` fns and `dioxus_interpreter_js::Interpreter` are reached from
    JavaScript — invisible to a Rust resolver.
  - **`ignore`-doc / genuinely-unused (true positives).** `dioxus`'s `tokio`
    dev-dep appears only in a `rust,ignore` doc fence (deliberately not scanned);
    the `dioxus_cli` platform config structs and `dioxus_core::Component` were
    in this bucket until Phase 4 increment 4 (below) — their references were
    real but invisible (test-module glob imports; an own-variant `pub use`
    path) — leaving the genuinely-referrer-less items still flagged.

> **History (Phase 5 — Tier-H assertions, 2026-06-11):** the two
> `dioxus_liveview` `history.rs` `unused-pub` findings (`routes::serialize` /
> `routes::deserialize`, used only via `#[serde(with = "routes")]`) were cleared
> by the built-in `serde-with` usage assertion (`syn-workspace/src/assertions.rs`),
> which parses the `with`-named path and credits its `serialize`/`deserialize`
> children. This is the corpus's only Tier-H-visible case; the strum / wasm-bindgen
> / md-5 rules that motivated the tier don't appear in the dioxus tree (they're
> guarded by `tests/cases/unused-deps/true_negatives/asserted_*`). The dep audit
> is unmoved — `axum` and friends stay flagged (a `__axum` macro-interpolation
> local does *not* vouch for them; the `md5-libname` fallback strips only the
> manifest side, not referenced names).

> **History (Phase 3, increment 8 — feature-plumbing deps):** the last corpus FP,
> regex's `aho-corasick`, was closed. `Manifest::feature_dep_refs`
> (`syn-workspace/src/manifest.rs`) reads the `[features]` table and extracts the
> dependency named by each value (`dep:NAME`, `NAME?/feat`, `NAME/feat` — leading
> ident before `?`/`/`, hyphen-normalized); `unused-deps` unions those into its
> referenced-crate set (`referenced_crate_names`), so an optional dep declared
> solely to forward a feature counts as used. Pure manifest data — no resolver
> model, no `unused-pub`/SCIP impact. Guarded by
> `unused-deps/true_negatives/dep_used_only_in_feature_plumbing`. With this the
> corpus is **fully FP-clean**: every flagged item is a confirmed true positive.

> **History (Phase 3, increment 7):** `regex` was added to exercise `unused-pub`
> at scale on a real multi-member workspace. Two of the three resolver/lint gaps
> it surfaced were fixed in `syn-workspace` (the third, feature-plumbing deps,
> stays a documented `unused-deps` known-FP above):
> - **Function-local `use` imports** (`regex-automata`'s `PERL_WORD`,
>   `regex-syntax`'s `age::BY_NAME`): a `pub` item referenced only through a
>   `use` *inside a fn body* — `use crate::…::age;` then `age::BY_NAME`, or a
>   braced `use crate::util::{unicode_data::perl_word::PERL_WORD, utf8};` then a
>   bare `PERL_WORD` — was missed, because only module-level `use`s were
>   processed. `collect_module_contents` now also collects `use`s nested in item
>   bodies (a `syn::visit` pass that stops at nested `mod`s) and feeds them to the
>   same binding pipeline. The bindings are module-scoped and only *add*
>   crate-local references the code already makes, so the cross-crate SCIP
>   differential is unmoved (precision-neutral, mirroring the sibling-name
>   broadening).
> - **Glob re-export reachability** (`regex`'s `Locations`): a backwards-compat
>   `pub type Locations` reachable only via `pub use crate::regex::string::*`
>   (and `…::bytes::*`) was flagged, because external-reachability only walked
>   direct module paths and the named-`pub use` exemption (`is_target`) didn't
>   cover globs. `Module::glob_reexports` now records public glob targets
>   (canonicalized: `crate`/`self`/`super`-anchored, or sibling-module-prepended
>   for the `pub use inner::*` form), and `ReExportIndex` marks every public item
>   of a glob target as a re-export target — the same exemption named `pub use`s
>   already receive. Guarded by
>   `unused-pub/true_negatives/{used_via_function_local_use,used_via_glob_reexport}`
>   plus resolver unit tests in `module_tree.rs` / `re_export.rs`.

> **History (Phase 3, increment 6):** `memchr` was added to stress deep,
> cfg-gated, arch-specific module trees. It surfaced one real FP — the `log`
> dep, referenced only as `log::debug!`/`log::trace!` inside `macro_rules!`
> wrapper macros, alongside a local `macro_rules! log` of the same name. A
> `macro_rules!` definition introduces a name in the *macro* namespace only, so
> it must not shadow a path-position reference (`log::debug` resolves `log` in
> the type/module namespace). `sibling_name`
> (`syn-workspace/src/resolve/module_tree.rs`) no longer treats `macro_rules!`
> items as siblings, so the `log` reference resolves to the external crate. The
> change is precision-neutral (it only *adds* external references that were
> being shadowed): the SCIP differential is unmoved (precision 100 %, recall
> 12/18). Guarded by the
> `unused-deps/true_negatives/dep_referenced_in_macro_not_shadowed_by_local_macro`
> fixture.

> **History (Phase 4, increment 2 — first real Dioxus corpus crate + intra-crate
> macro fix):** the `dioxus` monorepo (`v0.7.9`, whose own `dioxus-rsx` is the
> `0.7.9` this resolver parses against) was vendored to exercise the `dioxus_rsx`
> Phase A parser on real `rsx!` at scale — the prior coverage was a single
> synthetic fixture. Its framework-scale `unused-pub` audit surfaced one resolver
> FP class: an exported `macro_rules!` invoked only by bare intra-crate
> `name!(...)` was flagged unused, because bare single-ident macro invocations
> were never captured as references — a side effect of the increment-6 fix that
> drops macros from `sibling_names` (so `macro_rules! log` can't shadow the `log`
> crate in `log::debug!`). The core `MacroCallPass`
> (`syn-workspace/src/plugins/macro_calls.rs`) closes it: a bare invocation
> (`Ident !` + a delimited group — so multi-segment `m::foo!` and the `log::debug!`
> path case are untouched, preserving increment-6) is captured as
> `Origin::MacroCall` and bound to a same-crate `macro_rules!` of that name — the
> macro twin of the Dioxus `DioxusComponentPass`, but **core** (always on) since
> `macro_rules!` is a language feature. `Origin::MacroCall` is excluded from the
> SCIP projection (like `Macro`/`Component`), so the differential is unmoved
> (precision-neutral). Guarded by
> `unused-pub/true_negatives/exported_macro_used_intra_crate`. A macro invoked
> only via *another macro's* expansion (`$crate::eq_impls!` inside `read_impls!`)
> stays a documented macro-expansion known-FP.

> **History (Phase 4, increment 3 — Dioxus router cross-linking):** the increment-2
> audit's last tractable resolver FP — HotDog's `DogView` / `NavBar` / `Favorites`,
> `pub fn` components referenced only through a `#[derive(Routable)]` enum — is
> closed. The fix is **capture-only**: no new Phase B pass. The existing
> `DioxusComponentPass` already binds any bare `Origin::Component` ident to a
> same-crate `pub fn`; the gap was that route component names live in enum
> *attributes* (`#[route(...)]` / `#[layout(...)]`), which the token/AST scans
> never visit. A new capture
> (`syn-workspace/src/plugins/dioxus_rsx/routable.rs`, called from the module
> walk) emits each route component as `Origin::Component`: a `#[route]` variant
> binds its ident (or an explicit `#[route(path, Comp)]` 2nd arg), each
> `#[layout(Comp)]` binds `Comp`; `#[nest]` / `#[redirect]` / `#[child]` / `#[end_*]`
> name no component. Because route components reuse `Origin::Component` (already
> SCIP-skipped), the differential oracle is unmoved (precision-neutral) and the
> pass registry is untouched. Same-crate, by-name binding carries the identical
> precision tradeoff the rsx component pass already makes. The re-bless removed
> exactly the three HotDog findings, nothing else. Guarded by
> `unused-pub/true_negatives/dioxus_route_component_used` (a private
> `#[derive(Routable)]` enum whose `pub fn` components have no `rsx!` site, so only
> the route capture can link them) plus capture unit tests in `routable.rs`.

> **History (Phase 4, increment 4 — glob-import binding, prefix crediting,
> sibling-target classification, `{self as alias}`):** an own-workspace audit
> (run against this repo author's seven sibling projects, 2026-06-11) surfaced
> three resolver gaps the library-shaped corpus structurally couldn't — their
> reference patterns (bench/test-module glob imports) produce no `unused-pub`
> signal on crates whose items are consumed everywhere. All three fixed in
> `syn-workspace`, plus one classification fix they exposed:
> - **Glob-import bare names** (`use my_lib::*;` in a bench, `use super::*;`
>   in a `#[cfg(test)]` module, then bare `helper()` / `helpers::run()`): Tier 1
>   deliberately emits no bindings for globs, so these references were
>   invisible. Phase A now captures unmatched bare idents in glob-importing
>   modules as `Origin::GlobCandidate` (keyword/primitive/position-filtered,
>   deduped per module); the core Phase B `GlobImportPass`
>   (`syn-workspace/src/plugins/glob_imports.rs`) binds them — and
>   multi-segment runs whose root a glob brought into scope — against the glob
>   target's items/submodules/re-exports when the target is a workspace module.
>   FP-safe by-name binding, mirroring `MacroCallPass`; excluded from the SCIP
>   projection.
> - **Associated-path prefix crediting**: a reference to `a::b::c` now also
>   credits `a::b` in `referring_crates` — `Type::assoc_fn()` is a use of
>   `Type`, `module::item` of `module`. This is what resolved
>   `dioxus_core::Component` (its own variant is `pub use`d one line below the
>   enum — a real reference the exact-path index missed).
> - **`use path::{self as alias}`**: the `Rename` branch bound a bogus
>   `path::self`; it now binds the module under the alias, like the unrenamed
>   `{self, …}` form fixed in Phase 3.
> - **Sibling-target classification** (exposed by the glob fix): references
>   from a package's *sibling targets* (integration tests, benches, examples,
>   non-primary bins) now classify as cross-crate, not intra-crate — those
>   targets link the lib as an external crate, so the `pub(crate)` advice
>   would break them (`Workspace::referenced_from_sibling_target`).
>
> The re-bless removed nine dioxus `unused-pub` findings (the `dioxus_cli`
> config structs + `generate_manifest_schema`, referenced from `#[cfg(test)]`
> glob-importing modules — now IntraCrate, suppressed by this audit's
> `suppress-intra-crate`; `dioxus_core::Component` and
> `dioxus_fullstack::WebSocketStream` via prefix crediting) and two
> `unused-deps` names (`dioxus-router`, `dioxus-stores` — used only through
> prelude-glob bare names). Each removal was re-verified against the source as
> a genuine reference. Guarded by
> `unused-pub/true_negatives/{used_via_group_self_rename,used_via_assoc_fn_path,used_via_glob_import_in_bench,used_via_glob_import_test_mod}`
> plus resolver unit tests in `use_tree.rs`.

> **History (target-specific dependency tables):** `unused-deps` (and
> `centralized-deps`) now enumerate `[target.<cfg>.dependencies]` /
> `dev-dependencies` / `build-dependencies`, closing a silent false *negative* —
> a platform-gated dep that's unused or un-centralized was previously never
> checked (`Manifest::deps` only read the three top-level tables). `dioxus`
> surfaced the resulting true positives: `packages/desktop`'s Android
> (`jni`/`ndk`/`ndk-sys`/`ndk-context`) and macOS (`core-foundation`) deps, which
> have **zero references in that crate's `src/`**; `ecommerce-site`'s `chrono`
> (declared under both `cfg(wasm)` and `cfg(not wasm)`, unused either way); and
> the root crate's wasm-target `getrandom` dev-dep. All confirmed genuinely
> unreferenced — the same true-positive / structural class dioxus already
> documents. A dep declared under several `cfg`s in one section is reported once
> (both lints dedup by `(section, name)`). Guarded by
> `unused-deps/{true_positives/target_cfg_dep_unused,true_negatives/target_cfg_dep_used}`,
> `centralized-deps/true_positives/target_cfg_dep_needs_workspace`, and
> `manifest.rs` unit tests `deps_includes_target_specific_tables` /
> `deps_target_tables_respect_section`.

## Takeaway for follow-ups

- **The leaf-library corpus is fully FP-clean.** Every audited *library* crate
  (anyhow … regex) is clean except confirmed *true positives* — anyhow's `syn` and
  regex's `quickcheck` (unused dev-deps), and regex's two `BY_NAME` consts
  (unreferenced generated consts). No surviving false positives remain.
- **The `dioxus` framework crate is a deliberate exception** — at 112 members it
  is too large/complex to be FP-clean, and its audit instead *validates the lint
  at framework scale*: every finding is a true positive or a **documented
  structural non-goal** (macro-expansion, trait-method, derive-via-re-export,
  re-export-path, JS-interop). Its two genuine resolver FP classes were fixed:
  intra-crate exported-macro invocations (increment 2) and `#[derive(Routable)]`
  router cross-linking (increment 3).
- **Structural coverage** now spans the Dioxus framework monorepo (`dioxus`, 112
  members, the first crate with real `rsx!`) on top of a 7-member workspace with
  genuine cross-crate references (`regex`, 2119 items) exercising `unused-pub` at
  scale, deep cfg-gated arch-specific module trees (`memchr`), multi-member
  workspaces (`thiserror`), and module-file resolution (`bitflags`).
- **True positives** (anyhow `syn`, regex `quickcheck`, regex's `BY_NAME` consts)
  — leave flagged (no action). They validate that the lints catch real unused
  deps / over-exposed items, and rule out blanket conservatism (which would hide
  them).
- **Known-FP classes are now framework-scale and structural.** Every resolver FP
  class found to date is closed: the feature-plumbing-only-dependency FP via the
  `[features]` table (increment 8); the intra-crate exported-macro FP via
  `MacroCallPass` (Phase 4 increment 2); the `#[derive(Routable)]` router
  cross-linking FP via the route-component capture (Phase 4 increment 3); and
  the glob-import / assoc-path-prefix / `{self as alias}` trio via
  `GlobImportPass` + prefix crediting (Phase 4 increment 4). What
  `dioxus` documents above are *structural non-goals* — macro expansion,
  trait-method and re-export-path attribution, derive-via-re-export, JS interop —
  each requiring semantics (type/trait solving, macro expansion) the resolver
  deliberately doesn't implement. They are the honest ceiling of a syn-only
  resolver, not bugs. The standing `unused-deps` limitations
  (`build.rs`-generated code, `*-sys` link-only deps) remain suppressed via the
  `ignore` knob.
