# feature-drift

Detects drift between a crate's `[features]` table and its
`#[cfg(feature = "...")]` usage in source.

A structural (build-free) lint — on by default at `warn`.

## What it checks

Two failures:

- A feature declared in `[features]` but never gated in source (dead
  declaration).
- A `#[cfg(feature = "...")]` reference to a feature that isn't declared in
  `[features]` (undeclared feature).

`default` is exempt — cargo handles it specially.

## Configuration

None. The lint is structural and takes no parameters — it is on by default.
Change its level (or turn it off) through `[lints]`:

```toml
[lints]
feature-drift = "deny"   # or "allow" to turn it off
```

## Silencing

Change the level through `[lints]` as above, or write a comment directive in
the crate's `Cargo.toml` (for a declared-but-unused feature) or an `expect`
directive at the `#[cfg]` site (for an undeclared one). Prefer `expect`
(warns via `stale-expect` once resolved) over the permanent `allow`.
