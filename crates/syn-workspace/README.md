# syn-workspace

[![Crates.io](https://img.shields.io/crates/v/syn-workspace.svg)](https://crates.io/crates/syn-workspace)
[![Docs.rs](https://docs.rs/syn-workspace/badge.svg)](https://docs.rs/syn-workspace)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

A resolved workspace model for Rust, built on [`syn`].

`syn-workspace` fills the gap between per-file `syn` parsing and the full
rust-analyzer frontend: it loads a Cargo workspace, resolves imports
(including `use ... as ...` renames and `pub use` chains), and exposes a
typed model that downstream tools can query in sub-second time.

## What it does

- **Workspace discovery** via `cargo metadata` — honors `members`, glob
  patterns, `exclude`, and `default-members`.
- **Per-file imports** (Tier 1) — every `use` declaration becomes a
  `UseBinding { local_name, canonical }` with renames preserved.
- **Cross-file module trees** (Tier 2) — walks `mod foo;` declarations
  through `foo.rs` / `foo/mod.rs` / `#[path = "..."]` overrides.
- **`pub use` chain resolution** (Tier 2.5) — `Workspace::resolve_canonical`
  chases re-export edges to the definition site.
- **Macro-body reference extraction** — token-scanning of `macro_rules!`
  bodies, plus structured plugins for `quote!` and (optionally) `rsx!`.
- **Cross-crate reference index** — per-crate "what does this crate
  reference?" sets, built once at load time.

## What it doesn't do

- No type inference
- No trait solving
- No proc-macro execution
- No external-crate body materialization (external crates appear as
  name + version only)

It trades precision for speed.

## Quick example

```rust,no_run
use syn_workspace::Workspace;

let ws = Workspace::load(".")?;
for cr in ws.members() {
    for item in cr.pub_items() {
        println!("{} :: {}", cr.name, item.canonical);
    }
}
# Ok::<(), syn_workspace::Error>(())
```

## Features

- `dioxus` (default) — enables the `rsx!` / `dioxus::rsx!` macro-body
  parser. Disable with `default-features = false` if you don't need
  Dioxus component-path detection.

## Stability

Pre-1.0. The model is `Send + Sync` and the public API is stable enough
to build on, but breaking changes may still land in minor versions.
`pub use toml_edit;` is part of the public API contract: bumping
`toml_edit`'s major version triggers a `syn-workspace` major bump.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  http://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  http://opensource.org/licenses/MIT)

at your option.

[`syn`]: https://docs.rs/syn
