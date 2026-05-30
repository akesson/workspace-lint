# Differential-oracle regression net

This directory is the **pre-Phase-0** committed regression net from
[`docs/ROADMAP.md`](../../../../docs/ROADMAP.md) §B: it validates the
`syn-workspace` resolver against compiler ground truth without putting
rust-analyzer or a nightly toolchain on the common test path.

## Layout

```
<fixture>/
  workspace/        # a self-contained cargo workspace (own [workspace] table)
  expected/
    rustdoc.json    # normalized public def/visibility + re-export oracle
    scip.json       # normalized per-member referenced-packages + definitions
```

`expected/*.json` are **generated** — distilled from nightly rustdoc JSON and
`rust-analyzer scip` into small, path-relative, deterministic JSON. They are the
committed ground truth; do not edit them by hand.

## How the test works (`../oracle.rs`)

The fast path parses the committed JSON with `serde_json` only (no rust-analyzer,
no nightly) and diffs it against a live `Workspace::load`, asserting three things:

1. **def/visibility** — the resolver enumerates exactly the public defs rustdoc
   reports; impl-block methods are accounted for in `known_impl_methods` (the
   documented enumeration gap), not silently tolerated.
2. **re-export canonicalization** — `pub use` chains resolve to the same
   definition rustdoc resolves them to.
3. **dependency set** — every declared dependency SCIP proves is referenced is
   visible to the resolver (guards `unused-deps` against false positives).

A resolver regression in any dimension fails `cargo nextest run --workspace`.

## Re-blessing

Regenerate the committed oracles after changing a fixture or bumping the pinned
toolchain (needs a `nightly` toolchain + `rust-analyzer` on PATH):

```sh
cargo run --manifest-path tools/oracle-bless/Cargo.toml
```

Review the `git diff` of `expected/*.json` before committing — an unexpected diff
is either a real resolver change or toolchain drift (e.g. rustdoc
`format_version`, rust-analyzer symbol scheme). The toolchain the artifacts were
last blessed with is recorded in `docs/ROADMAP.md` §B.

## Adding a fixture

1. Create `<name>/workspace/` as a self-contained cargo workspace (its own
   `[workspace]` table keeps it out of the parent — `members = ["crates/*"]`
   never matches this deep).
2. Add a `Fixture { dir, rustdoc_crate }` entry in `tools/oracle-bless/src/main.rs`.
3. Run the bless command, then add a `#[test]` in `../oracle.rs`.
