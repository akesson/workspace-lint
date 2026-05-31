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

Every remaining finding is an `unused-deps` **false positive on a dev-dependency**:
a dep exercised by a code path the *syntactic* resolver under-scans (doc-tests).
This is the same reason Phase 1's set-level dependency oracle deliberately checks
only `[dependencies]`, not `[dev-dependencies]`.

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

- **`anyhow`**
  - `futures` — **FP**. Referenced only inside a doc-test
    (`/// use futures::stream::…` in `src/error.rs`). The resolver does not parse
    code fences in doc comments, so a dependency used only by doc-tests looks
    unused.
  - `syn` — **unconfirmed**. No path reference appears in any scanned `.rs` or
    doc comment; likely test-infrastructure-only or vestigial. Tracked, not yet
    root-caused.

- **`bitflags`** — clean. Previously flagged `serde_lib` + `serde_test`; both
  cleared by the module-file resolution fix (see History above). Must stay
  `all passed`.

## Takeaway for follow-ups

The one remaining FP is anyhow's `futures` (doc-test-only). Closing it needs
either doc-comment code-fence scanning (its own Phase-3 increment) or the lighter
alternative worth weighing: have `unused-deps` treat `[dev-dependencies]` more
conservatively, since their usage is structurally harder to see than
`[dependencies]`. anyhow's `syn` finding stays open: no source reference appears
anywhere scanned, so it is likely build-/`trybuild`-only or vestigial rather than
a resolver miss.
