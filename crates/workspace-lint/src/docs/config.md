# config

Reports a structural problem in the config file itself, rather than in the
code being linted.

An always-on meta lint: it takes no configuration and runs on every
invocation.

## What it checks

Two problems in `.workspace-lint.toml` (or the
`[workspace.metadata.workspace-lint]` table):

- An unknown section or key — with a "did you mean …?" hint when a close
  match exists (so a typo like `file-siz` is caught, not silently ignored).
- A policy lint enabled in `[lints]` but given no rules table, so it could
  never actually fire.

## Configuration

None — it is always on. Its level can be changed through `[lints]`, with one
guard: a blanket `[lints] default = "allow"` will **not** silence it (only an
explicit `[lints] config = "allow"` does), so a typo'd config can't hide
itself.

## Silencing

Fix the config (the intended path — the message names the offending key or
missing table), or, deliberately, silence one line with a comment directive
in the config file:

```toml
# workspace-lint: expect(config)
```

Or set the explicit `[lints] config = "allow"`.
