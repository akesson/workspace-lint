# centralized-deps

Verifies that every workspace crate declares its dependencies with
`workspace = true` instead of pinning a version directly, so the whole
workspace shares one source of truth in `[workspace.dependencies]`.

A structural (build-free) lint — on by default at `warn`.

## What it checks

Each member `Cargo.toml` is scanned for `[dependencies]`,
`[dev-dependencies]`, and `[build-dependencies]` entries that specify a
version (or other fields) inline rather than inheriting from the workspace
with `{ workspace = true }`. Every inline entry is one finding.

## Configuration

None. The lint is structural and takes no parameters — it is on by default.
Change its level (or turn it off) through the `[lints]` table:

```toml
[lints]
centralized-deps = "deny"   # escalate to a CI-failing error
# centralized-deps = "allow"  # or turn it off
```

## Fix behavior

`--fix` resolves both halves:

- A dep whose key already exists in `[workspace.dependencies]` is rewritten
  to `{ workspace = true }` in place, preserving any `features`, `optional`,
  and `default-features`.
- A dep *missing* from the workspace table is seeded there too —
  `name = "version"` inserted at its alphabetically sorted position (the
  table is created at end-of-file if absent) — and the member is rewritten
  in the same run.

The seed is withheld (left as a preview-only suggestion) when members
disagree on the version — align them first, since the tool can't know which
one is right — or when the dep is renamed (`{ package = "…" }`), which needs
the rename recorded in the workspace entry.

## Silencing

Silence one manifest with a comment directive above the dependency (or
anywhere in the crate's `Cargo.toml`):

```toml
# workspace-lint: expect(centralized-deps)
serde = "1.0"
```

Prefer `expect` (it warns via `stale-expect` once the finding is gone) over
the permanent `allow`.
