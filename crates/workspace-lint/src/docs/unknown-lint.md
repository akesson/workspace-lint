# unknown-lint

Reports a lint name that doesn't exist — referenced either in the `[lints]`
table or in an `expect!` / `allow!` directive — instead of silently doing
nothing (a typo'd lint name would otherwise enable/silence nothing at all).

An always-on meta lint: it takes no configuration and runs on every
invocation.

## What it checks

Every lint name in `[lints]` and in every source directive is checked
against the known set (`LintId::ALL`). An unrecognized name is reported, with
a "did you mean …?" hint when a close match exists.

## Configuration

None — it is always on. Its level can be changed through `[lints]`, with one
guard: a blanket `[lints] default = "allow"` will **not** silence it (only an
explicit `[lints] unknown-lint = "allow"` does), so a typo can't hide itself.

## Silencing

Fix the name (the intended path — the message suggests the closest real
lint), or set the explicit `[lints] unknown-lint = "allow"`. A comment
directive naming a nonexistent lint is itself what triggers this lint, so the
resolution is always to correct or remove the offending name.
