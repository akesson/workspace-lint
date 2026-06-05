# Differential-oracle regression net

This directory is the committed regression net described in
[`DESIGN-ir-pipeline.md`](../../DESIGN-ir-pipeline.md) §10: it validates the
`syn-workspace` resolver against compiler ground truth without putting
rust-analyzer or a nightly toolchain on the common test path.

## Layout

```
<fixture>/
  workspace/        # a self-contained cargo workspace (own [workspace] table)
  expected/
    rustdoc.json         # normalized public def/visibility + re-export oracle
    rustdoc-private.json # full module tree + visibility (--document-private-items)
    scip.json            # normalized per-member referenced-packages + definitions
```

`expected/*.json` are **generated** — distilled from nightly rustdoc JSON and
`rust-analyzer scip` into small, path-relative, deterministic JSON. They are the
committed ground truth; do not edit them by hand.

## How the test works (`../oracle.rs`)

The fast path parses the committed JSON with `serde_json` only (no rust-analyzer,
no nightly) and diffs it against a live `Workspace::load`, asserting five things:

1. **def/visibility (rustdoc)** — the resolver enumerates exactly the public defs
   rustdoc reports; impl-block methods are accounted for in `known_impl_methods`
   (the documented enumeration gap), not silently tolerated.
2. **def witness (SCIP)** — an independent oracle confirms every enumerated def,
   and the SCIP range encoding (the `café` byte-span guard) is unchanged.
3. **module tree + visibility** — the *full* tree, including private and
   `pub(crate)` items (via `--document-private-items`), matches, with visibility
   tiers (public / crate / internal) agreeing.
4. **re-export canonicalization** — `pub use` chains, including `as` renames,
   resolve to the same definition rustdoc resolves them to.
5. **dependency set** — every declared dependency SCIP proves is referenced is
   visible to the resolver (guards `unused-deps` against false positives), and
   dev/build deps are excluded by the `DepSection` filter.

A resolver regression in any dimension fails `cargo nextest run --workspace`.

## Re-blessing

Regenerate the committed oracles after changing a fixture or bumping the pinned
toolchain (needs a `nightly` toolchain + `rust-analyzer` on PATH):

```sh
cargo run --manifest-path tools/oracle-bless/Cargo.toml
```

Review the `git diff` of `expected/*.json` before committing — an unexpected diff
is either a real resolver change or toolchain drift (e.g. rustdoc
`format_version`, rust-analyzer symbol scheme). The pinned rustdoc
`format_version` is the `EXPECTED_RUSTDOC_FORMAT` constant in `tools/oracle-bless`;
the full oracle toolchain the artifacts were last blessed with is recorded in
[`DESIGN-ir-pipeline.md`](../../DESIGN-ir-pipeline.md) §8.

## Adding a fixture

1. Create `<name>/workspace/` as a self-contained cargo workspace (its own
   `[workspace]` table keeps it out of the parent — `members = ["crates/*"]`
   never matches this deep).
2. Add a `Fixture { dir, rustdoc_crate }` entry in `tools/oracle-bless/src/main.rs`.
3. Run the bless command, then add a `#[test]` in `../oracle.rs`.
