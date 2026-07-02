# wl-extractor — the Phase-1 IR extractor

A Dylint `LateLintPass` compiled as a cdylib. Loaded into a nightly rustc
driver once per crate compilation, it walks that crate's `TyCtxt` and writes a
byte-precise IR fragment (`wl-ir::IrFragment`) to `$WL_IR_OUT/<crate>.json` —
the facts channel of workspace-lint's two-phase engine
(`SPIKE-rustc-fidelity-tree.md` §4). It never emits a diagnostic.

## Toolchain

This package lives **outside** the main (stable) workspace:

- `rust-toolchain.toml` pins the nightly that dylint 6.0.1 tracks
  (`rustc_private` has no stability guarantee — the pin *is* the contract).
  Bump procedure: SPIKE §12.4 (the WS2 drill measured ~1 edit per 10 weeks).
- `.cargo/config.toml` links via `dylint-link` (`cargo install dylint-link`),
  which embeds the toolchain + host triple in the artifact name:
  `libwl_extractor@nightly-…-<triple>.dylib` / `.so` / `.dll`.
- `Cargo.lock` is committed; builds are `--locked` for determinism.

## Build & test

```sh
cargo build            # the dylib (rust-toolchain.toml picks the pin)
cargo test             # golden-spine tier 1: tests/probe.rs extracts
                       # tests/probes/expansion via the embedded dylint::run
                       # and asserts the macro-expansion span policy
```

CI: `.github/workflows/extractor.yml` runs both on Linux, macOS, and Windows.

## Relation to the rest of the engine

- `crates/wl-ir` — the serde contract this dylib writes (path dependency;
  shipped in lockstep — the workspace-lint binary vendors this whole package).
- `spike/driver` — the raw `rustc_driver` twin carrying an identical copy of
  `extract()`; the original proof the walk is host-agnostic (retires with
  `spike/`).
- Phase 2 (assembly into the workspace-global model) is stable-toolchain code;
  see `crates/wl-engine` (migration PRs 3–4).
