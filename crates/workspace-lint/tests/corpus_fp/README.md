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

Scope: **`unused-deps` only** (see `corpus_fp.rs` for why `unused-pub` is
excluded — it flags a standalone library's whole public API by construction).

## Findings (audited 2026-05-31)

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

- **`anyhow`** — one FP, one **true positive**:
  - `futures` — **FP**. Referenced only inside a doc-test
    (`/// use futures::stream::…` in `src/error.rs:62`). The resolver does not
    parse code fences in doc comments, so a dependency used only by doc-tests
    looks unused. Post-#30 (all targets — tests/examples/benches/build.rs — are
    scanned), doc-tests are the *only* remaining dev-dep blind spot.
  - `syn` — **TRUE POSITIVE** (confirmed). An exhaustive search found zero
    references anywhere — not `use syn`, not `syn::`, not in `build.rs`, tests,
    examples, or doc comments. anyhow (v1.0.102) declares a `syn` dev-dep it
    genuinely does not use; the lint is *correctly* flagging it. (This is why
    blanket `[dev-dependencies]` conservatism would be wrong — it would suppress
    this real finding.)

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

> **History (Phase 3, increment 4):** the two gaps above were closed in
> `syn-workspace`. `extract_code_paths` now keeps a bare single ident that names
> a same-module sibling (not just a `use` binding), so a sibling referenced by
> bare name in a field type / literal / bound / impl is recorded.
> `bindings_from_use` now binds the module for `use path::{self, …}` instead of a
> name called `self`. Both only *add* same-crate (or, for `{self}`, normalized)
> references, so the cross-crate SCIP precision gate is unaffected; the FPs
> reclassify `Unused` → `IntraCrate` and `suppress-intra-crate` drops them.

## Takeaway for follow-ups

- **anyhow `syn`** — a confirmed true positive; leave it flagged (no action). It
  validates that `unused-deps` catches real unused deps, and rules out blanket
  dev-dependency conservatism (which would hide it).
- **anyhow `futures`** — the **lone remaining corpus FP**; needs doc-comment
  code-fence scanning (its own increment), the last dev-dep blind spot.
