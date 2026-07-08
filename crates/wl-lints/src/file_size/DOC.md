# file-size

Enforces a maximum number of *code* lines per file (blank lines and
comments excluded, counted by
[tokei](https://github.com/XAMPPRocky/tokei)).

A policy lint: it does nothing until you give it at least one rule.

## What it checks

Every file matched by a rule's `glob` is counted and compared against that
rule's `max-code-lines`.

For `.rs` files the count is *shipped* source only — the same
production-only definition `crate-size` uses, so the two budgets agree to
the line. Excluded from the count: anything gated by exactly `#[cfg(test)]`,
`#[test]` / `#[wasm_bindgen_test]` functions, files that are out-of-line
`#[cfg(test)] mod x;` targets, and the `tests/` / `benches/` / `examples/`
dev-target trees. Ambiguous code is counted rather than under-counted:
`#[cfg(any(test, …))]` stays in, and a file that fails to parse is counted
whole. Non-Rust globs (e.g. `**/*.ts`) keep tokei's raw whole-file count.

## Configuration

One `[[file-size.rules]]` table per budget. Presence of the table is what
enables the lint.

```toml
[[file-size.rules]]
glob = "**/*.rs"
max-code-lines = 500
```

- `glob` — files this budget applies to (workspace-relative).
- `max-code-lines` — the ceiling; files above it are reported.

Ad-hoc (no config) form:

```sh
workspace-lint check file-size --glob "**/*.rs" --max-code-lines 500
```

## Silencing

Write an `expect` directive at the top of the offending file:

```rust
workspace_lint::expect!(file_size);
```

Prefer `expect` (warns via `stale-expect` once the file drops back under
budget) over the permanent `allow`.
