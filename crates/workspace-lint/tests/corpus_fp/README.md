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
  positives** and **4 false positives** across three limitation classes; two of
  the three classes were then fixed (see History below), leaving the snapshot at
  three true positives plus one documented known-FP:
  - **`quickcheck`** — **TRUE POSITIVE**. A `[dev-dependencies]` the *root*
    `regex` crate declares but uses nowhere (`regex-automata` declares its own).
    Same shape as anyhow's `syn`.
  - **`regex_syntax::{perl_decimal,perl_space}::BY_NAME`** — **TRUE POSITIVES**.
    Generated `pub const`s in the private `mod unicode_tables`; the lookups read
    `DECIMAL_NUMBER` / `WHITE_SPACE`, never the `BY_NAME` variant, so these are
    genuinely-unreferenced over-exposed items. (Other tables' `BY_NAME` *are*
    read and are correctly seen as used.)
  - **`aho-corasick`** — **known-FP (feature-plumbing)**. The root crate declares
    it only to forward the `perf-literal` feature (`dep:aho-corasick`,
    `aho-corasick?/std`) to `regex-automata`; it is never named in the root
    crate's own code. `unused-deps` matches code references, not feature-table
    entries — a documented limitation, left flagged.

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

## Takeaway for follow-ups

- **Corpus is FP-clean except one documented known-FP.** Every audited crate is
  clean except confirmed *true positives* — anyhow's `syn` and regex's
  `quickcheck` (unused dev-deps), regex's two `BY_NAME` consts (unreferenced
  generated consts) — plus regex's `aho-corasick`, a documented feature-plumbing
  known-FP (`unused-deps` doesn't read `[features]` `dep:` / `?/` entries).
- **Structural coverage** now includes a large 7-member workspace with genuine
  cross-crate references (`regex`, 2119 items) exercising `unused-pub` at scale,
  on top of deep cfg-gated arch-specific module trees (`memchr`), multi-member
  workspaces (`thiserror`), and module-file resolution (`bitflags`).
- **True positives** (anyhow `syn`, regex `quickcheck`, regex's `BY_NAME` consts)
  — leave flagged (no action). They validate that the lints catch real unused
  deps / over-exposed items, and rule out blanket conservatism (which would hide
  them).
- **Remaining known-FP class:** feature-plumbing-only dependencies. `unused-deps`
  matches code references, not feature-table `dep:` / `optional?/feature`
  entries, so an optional dep declared solely to forward a feature reads as
  unused. Tracked for a follow-up that consults the `[features]` table.
