# unused-pub

Detects `pub` items that are never used across the workspace, judged on the
rustc engine's resolved reference graph — it needs **no** SCIP index and
**no** `rust-analyzer` subprocess, so it runs the same locally and in CI.

A semantic lint — on by default at `warn`. It needs a compiling workspace
and the extraction tier; `--fast-only` skips it.

## What it checks

Every `pub` item is checked for a reference from elsewhere in the workspace.
Items re-exported via `pub use` are always skipped (narrowing them would
break the re-export). So are types that appear in the *public signature* of
a more-visible item (a `pub fn` return type, a `pub` field, a trait-impl
associated type, …) — tightening those would not compile
(E0446 / `private_interfaces`). That exemption also covers a type a builder
macro promotes into its generated public `build()` signature, recognized
from the attribute: `typed_builder`'s `#[builder(build_method(into = T))]`
and `derive_builder`'s `#[builder(build_fn(error = "T"))]`.

**Publish-aware.** The lint can't see consumers outside your workspace, so
it treats a crate's public API as off-limits **only when the crate declares
it has external consumers — `publish = true`** (or a registry list) in its
`Cargo.toml`. Every other crate — `publish = false`, or, by default, *no*
`publish` field — is treated as **workspace-internal**: its `pub` items are
checked, and anything not used by another workspace crate is flagged for
narrowing to `pub(crate)`. This is the whole point of the lint at the
workspace level — over-exposed internal APIs get caught. So: **mark
genuinely-published crates with `publish = true`**, and leave internal
crates as they are.

If a crate accumulates several findings, the lint emits a one-line hint
suggesting `publish = true` — in case the flood means the crate really is
published. Set `assume-all-public = true` to opt out entirely and treat
every crate as having an external API (the pre-publish-aware behavior).

Three findings:

- **used only inside the crate** (by production code) → suggests narrowing
  to `pub(crate)`.
- **used only by test code** → the item ships in the production build with
  nothing production reaching it — dead code the tests embalm. Every test
  unit counts: same-crate `#[cfg(test)]` modules, other crates' test code,
  integration tests — and benches, when the `[engine]` matrix has a bench
  entry (`"cargo bench"`); bench reach classifies as test reach. Without a
  bench entry the engine never compiles `benches/`, so a bench-mentioned
  item is instead reported as possibly unused with a note naming the missing
  config entry, and is never deleted (see the mention veto below). No fix is
  machine-applied by plain `--fix`
  (narrowing trips `dead_code` on the non-test build; a bare deletion breaks
  the referencing tests) — gate it `#[cfg(test)]`, move it into test code,
  mark a deliberate test-support API with `expect`, or remove it together
  with its tests (`--fix-auto-delete` does exactly that when the tests are
  exclusive scaffolding — see **Fix behavior**).
- **unused anywhere** → suggests `pub(crate)` (or deletion, under
  `--fix-auto-delete`).

Test reach never hides a finding the other direction either: an item used by
another crate's tests *and* by production code is simply in use, and an item
whose production users are all in its own crate keeps its narrowing advice
only when no other crate's tests reach it (they'd break — `pub(crate)`
cannot cross a crate boundary).

## Configuration

The `[unused-pub]` table is optional (the lint is on by default):

```toml
[unused-pub]
exclude-crates = ["api"]
allowlist = ["*Error", "main"]
kinds = ["function", "struct"]
exclude-paths = ["generated/**"]
suppress-intra-crate = false
assume-all-public = false
publish-hint-threshold = 3
```

- `exclude-crates` — crate names to skip entirely.
- `allowlist` — glob patterns matched against an item's canonical path
  (e.g. `*Error`, `main`).
- `kinds` — item kinds to check: `function` (alias `fn`), `struct`, `enum`,
  `union`, `trait`, `type`, `const`, `static`, `module` (alias `mod`),
  `macro`. Omit (empty) to check all kinds. An unrecognized kind is a config
  error.
- `exclude-paths` — glob patterns for source file paths to skip.
- `suppress-intra-crate` — when `true`, report only items unused *anywhere*
  and drop the "used only inside the crate" findings. Default `false`.
- `assume-all-public` — when `true`, treat *every* crate as having an
  external public API. The conservative pre-publish-aware behavior. Default
  `false`.
- `publish-hint-threshold` — emit the "set `publish = true`" hint once a
  workspace-internal crate reaches this many findings. `0` disables it.
  Default `3`.

Any of these can be set per-crate via `[crates.<name>.unused-pub]`, which
wholesale-replaces the global `[unused-pub]` for that crate.

Ad-hoc (no config) form:

```sh
workspace-lint check unused-pub --allowlist "*Error" --kinds function
```

## Fix behavior

`--fix` tightens `pub fn` / `pub struct` / … to `pub(crate)` for items used
only inside their own crate. It never deletes code.

**One-pass deletion cascade (`--fix-auto-delete`).** This flag is everything
`--fix` does, plus: the fix for an item that's unused everywhere becomes
whole-item deletion (doc comment through body) instead of `pub(crate)`
narrowing — but only when the containing file is git-tracked and clean (git
is the backup; dirty or untracked files are skipped with a `note:`
explaining why). It is deliberately a CLI flag with no config equivalent:
deleting code is a manual, human-invoked operation, and a CI `--fix` run
must never be able to do it.

Deleting a dead item frees whatever it solely reached, so a single
`workspace-lint --fix-auto-delete` run converges the *entire* dead chain —
no commit-and-rerun between layers. When a removal leaves a `use` dangling,
the import is trimmed in the same pass — both an import *of* a removed item
(E0432) and an import whose last real user was removed (an `unused_imports`
warning), including imports of out-of-workspace items: a multi-name list
keeps its live leaves (`use m::{a, b}` → `use m::{b}`) and a list left empty
is dropped whole. Glob imports (`use dioxus::prelude::*;`) are handled by a
resolver-grounded accounting: the extractor records rustc's own glob_map
(which names actually resolved through each glob) plus typeck's
used-trait-imports facts, and the whole statement is deleted only when every
recorded use is explained by removed code and nothing surviving could still
lean on it; every rule fails toward keeping (an extra `unused_imports`
warning, never a broken build).

An item **only used by test code** may be deleted too — but never alone
(that would break `cargo test`). It goes only when every referencing test
item is *exclusive scaffolding*: every workspace item that test reaches is
also deleted in this pass (out-of-workspace calls like `assert_eq!` don't
count), and nothing surviving still uses the test item. Then target, tests,
their now-orphaned test helpers, and their imports are all removed together;
emptied `#[cfg(test)] mod tests {}` shells and emptied `tests/*.rs` files
are left in place (they compile warning-free). A test that also asserts on
surviving code — or a shared fixture another test still uses — **vetoes**
the deletion: the item stays and the note names the blocking test. Every
rule fails toward keeping.

Private code the deleted items alone reached (helpers, consts) is deleted as
collateral in the same cascade, causality-gated: a private item that was
already dead before the fix is the author's, not ours, and private
structs / enums / fields are never touched. An item stays alive if *any*
`[engine] configs` entry uses it, if an `expect!` / `allow!` silences it, if
a `use` naming it is macro-generated or lives in a generated file, or if it
is **mentioned inside a `#[cfg(...)]` region no declared config compiles**,
or **mentioned in a bench source while no config has bench kind** —
deletion needs a higher standard of proof than reporting, so a
possibly-wasm-only (or windows-only, feature-gated, bench-only, …) item is
never deleted; the diagnostic names the uncovered cfg (or the bench file)
and the `[engine]` entry that would cover it.

A deletion is also vetoed when the fixed tree would newly fail a
`-D warnings` gate on something that *survives* (e.g. removing the last read
of a surviving type's field, or an `is_empty` out from under a surviving
`len`) — the finding stays, downgraded, with a note naming what would fire.

**Re-run to converge.** Deletions cascade within one run, but a *narrowing*'s
consequences can't: once `pub fn make(opts: CreateOpts)` becomes
`pub(crate)`, the `CreateOpts` it pinned `pub` is itself tightenable — a
verdict only the next compile sees. A fix run that changed anything prints a
`re-run until no fixes remain` note; expect the second run to apply a small
tail of tightenings and the third to be clean.

**Clippy-unmask guard.** De-`pub`-ing an item strips clippy's
`avoid-breaking-exported-api` exemption, so a narrow can activate style
lints. `--fix` replays the two observed-in-practice ones
(`wrong_self_convention` and `len_without_is_empty`) and downgrades an
unmasking tighten to shown-but-not-applied, with a note naming the method.
On a `-D warnings` codebase, follow a delete run with `cargo clippy --fix`
and `cargo fmt`.

## Silencing

Write an `expect` directive immediately above the item — a marker macro in
Rust, or the marker-free line comment:

```rust
// workspace-lint: expect(unused-pub)
pub fn still_load_bearing() {}
```

Prefer `expect` (warns via `stale-expect` once the item is used or removed)
over the permanent `allow`. For a whole crate the lint shouldn't judge, use
`exclude-crates` or mark it `publish = true`.

A **deliberate test-support API** — a helper exposed *for* other crates'
tests — is the expected exception to the "only used by test code" finding:
`expect` it with a one-line reason. `stale-expect` retires the directive the
day a production caller appears.
