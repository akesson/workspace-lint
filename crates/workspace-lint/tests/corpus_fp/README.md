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

## Findings (audited 2026-05-30)

Every current finding is an `unused-deps` **false positive on a dev-dependency**,
and they cluster around one theme: dev-deps are exercised by code paths the
*syntactic* resolver under-scans (doc-tests, examples, `trybuild` UI fixtures,
`#[cfg]`-gated test modules) and by reference positions it doesn't record
(external globs, derive-attribute paths). This is the same reason Phase 1's
set-level dependency oracle deliberately checks only `[dependencies]`, not
`[dev-dependencies]`.

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

- **`bitflags`**
  - `serde_lib` (a renamed dep, `package = "serde"`) — **FP**. Used via
    `use serde_lib::*` (an *external* glob import, which the resolver
    intentionally does not expand — a known false-negative class) and via
    `#[derive(serde_lib::Serialize)]` (a derive-attribute path).
  - `serde_test` — **FP**. Used via a `use serde_test::{…}` inside a
    `#[cfg(feature = "serde")]`-gated module.

## Takeaway for follow-ups

Closing these needs resolver work (scanning doc-comment code fences, recording
external-glob and derive-attribute references, traversing cfg-gated modules) —
each its own Phase-3 increment, out of scope for the corpus harness PR. A
lighter alternative worth weighing: have `unused-deps` treat
`[dev-dependencies]` more conservatively, since their usage is structurally
harder to see than `[dependencies]`.
