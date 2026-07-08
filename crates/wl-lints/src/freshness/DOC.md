# freshness

Checks that tracked files (e.g. `CLAUDE.md`) are newer than their
dependencies — a cheap way to keep documentation from silently falling
behind the source it describes.

A policy lint: it does nothing until you give it at least one rule.

## What it checks

For each rule, every file matched by `glob` must have a modification time no
older than every file matched by `depends-on`. A target older than any of
its dependencies is reported. The check is skipped automatically when the
`CI` environment variable is set (freshness is a local-authoring aid, not a
CI gate).

## Configuration

One `[[freshness.rules]]` table per relationship. Presence of the table
enables the lint.

```toml
[[freshness.rules]]
glob = "**/CLAUDE.md"
depends-on = "**/*.rs"
```

- `glob` — the tracked files that must stay fresh.
- `depends-on` — the files they must be newer than.

Ad-hoc (no config) form:

```sh
workspace-lint check freshness --glob "**/CLAUDE.md" --depends-on "**/*.rs"
```

Once you have brought a target up to date, `workspace-lint done` touches
every freshness target so it reads as newer than its dependencies.

## Silencing

Change the level (or turn it off) through `[lints]`:

```toml
[lints]
freshness = "allow"
```

Or write a comment directive in the target file. Prefer `expect` (warns via
`stale-expect` once resolved) over the permanent `allow`.
