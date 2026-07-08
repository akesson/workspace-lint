# crate-size

Enforces a maximum number of *shipped* code lines per crate directory —
the maintained product code, not the test mass that often dwarfs it.

A policy lint: it does nothing until you give it at least one rule.

## What it checks

Every directory matched by a rule's `glob` has its Rust source counted (via
[tokei](https://github.com/XAMPPRocky/tokei)) and compared against
`max-code-lines`. Test code is excluded: the `tests/`, `benches/`, and
`examples/` dev-target directories wholesale, plus in-file test items
(anything gated by exactly `#[cfg(test)]`, `#[test]` / `#[wasm_bindgen_test]`
functions, and out-of-line `#[cfg(test)] mod x;` files).

## Configuration

One `[[crate-size.rules]]` table per budget. Presence of the table enables
the lint.

```toml
[[crate-size.rules]]
glob = "crates/*"
max-code-lines = 5000
include = ["*.rs"]
```

- `glob` — crate directories this budget applies to.
- `max-code-lines` — the ceiling; crates above it are reported.
- `include` — override which files count. Non-Rust types are counted
  as-is, with no test-stripping.

Ad-hoc (no config) form:

```sh
workspace-lint check crate-size --glob "crates/*" --max-code-lines 5000 --include "*.rs"
```

## Silencing

Change the level (or turn it off) through `[lints]`:

```toml
[lints]
crate-size = "deny"
```

Or write an `expect` directive at the top of a file in the offending crate.
Prefer `expect` (warns via `stale-expect` once resolved) over `allow`.
