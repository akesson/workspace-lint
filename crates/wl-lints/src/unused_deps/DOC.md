# unused-deps

Flags dependencies declared in `Cargo.toml` that nothing references, judged
on the rustc engine's resolved reference graph.

A semantic lint — on by default at `warn`. It needs a compiling workspace
and the extraction tier (the same cost class as `unused-pub`); `--fast-only`
skips it.

## What it checks

Every `[dependencies]` / `[dev-dependencies]` / `[build-dependencies]` entry
is checked for at least one resolved reference edge into it. The judgment is:

- **facade-aware** — a dep is credited when anything in its resolved
  dependency closure is referenced, so `clap` counts even though every
  symbol resolves into `clap_builder`.
- **rename-aware** — `package = "…"` renames are followed.
- **feature- and doc-aware** — doc-fence code blocks and feature plumbing
  also credit a dep.

Dev-dependencies are judged only when a test-compiling entry (`cargo test`)
is in the `[engine]` config matrix — without one they are skipped, never
guessed. The default matrix includes one.

Two dep shapes are never judged, because no config run on one host can
observe them: `optional = true` deps (feature-gated — only compiled when
their feature is enabled, whether or not a `[features]` table names them)
and deps declared under `[target.<cfg>.…]` tables (platform-gated — only
compiled when the cfg matches the build host).

A member that no `[engine]` config compiles (e.g. a platform-gated crate
absent from a `-p`-scoped matrix) produces no compiler output, so its deps
cannot be judged. Rather than flag them all, the lint emits one non-failing
`warn` coverage note naming the crate — add a config that builds it (a
`--target <triple>` universe checks platform code without linking) to judge
its deps, or `ignore` them.

A subtler case: a dep used *only* behind a `#[cfg(...)]` your `[engine]`
matrix never compiles. Here the crate itself compiles and is judged, but the
reference edge lives in shadowed code, so the dep can read as unused. It is a
`warn` and never breaks a build; add a `--target <triple>` universe that
compiles the gated code (extraction is `cargo check`, no linking) to judge
the dep exactly, or `ignore` it.

A dep whose hyphen-stripped name matches a referenced lib target is credited
(`md-5` declares the crate whose lib is `md5`), so a rename-by-convention
never reads as unused. This only ever *suppresses* a finding.

## Configuration

The `[unused-deps]` table is optional (the lint is on by default):

```toml
[unused-deps]
ignore = ["prost", "tonic"]
```

- `ignore` — dependency names to never report.

The `ignore` list can be scoped to a single crate via
`[crates.<name>.unused-deps]` when a dep is unused in only one member.

Ad-hoc (no config) form:

```sh
workspace-lint check unused-deps --ignore prost --ignore tonic
```

## Fix behavior

Removing a dependency is a deletion, so it is quarantined behind
`--fix-auto-delete` (the same flag that gates unused-pub's whole-item
deletion): it deletes the dep line from its `[dependencies]` /
`[dev-dependencies]` / `[build-dependencies]` table. Plain `--fix` never
removes a dep — the finding's own verdict is "possibly unused", and a
suggestion the lint itself hedges on must not auto-apply under the flag
that promises never to delete. Under plain `--fix` the removal is reported
as withheld with this reason.

## Silencing

Write a comment directive above the dependency (or anywhere in the crate's
`Cargo.toml`):

```toml
# workspace-lint: expect(unused-deps)
prost = { workspace = true }
```

Prefer `expect` (warns via `stale-expect` once the dep is used or removed)
over the permanent `allow`. For a dep the lint structurally can't see, the
`ignore` list is the better tool.
