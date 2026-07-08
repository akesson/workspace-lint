# module-tree

Checks the structural integrity of a crate's `mod` graph.

A structural (build-free) lint — on by default at `warn`.

## What it checks

Two failures:

- A `mod foo;` whose target file doesn't exist — neither `foo.rs`,
  `foo/mod.rs`, nor a `#[path = "..."]` override resolves.
- An orphan `.rs` file under `src/` that no `mod` chain reaches, so it is
  never compiled.

## Configuration

None. The lint is structural and takes no parameters — it is on by default.
Change its level (or turn it off) through `[lints]`:

```toml
[lints]
module-tree = "deny"   # or "allow" to turn it off
```

## Silencing

Change the level through `[lints]` as above, or write an `expect` directive
in the affected file. Prefer `expect` (warns via `stale-expect` once
resolved) over the permanent `allow`.
