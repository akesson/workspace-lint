# stale-expect

Fires when an `expect!` / `expect(...)` directive silences nothing because
the underlying lint stopped firing — the mechanism that makes `expect`
preferable to the permanent `allow`, since a silence that outlives its cause
tells you so instead of rotting quietly.

An always-on meta lint: it takes no configuration and runs on every
invocation.

## What it checks

Every `expect` directive is matched against the findings actually produced;
one that matched nothing is reported (with a `--fix`-deletable suggestion).
Only lints that actually *ran* are judged: a directive for a lint skipped by
`--fast-only`, disabled via `allow`, or outside a `check <lint>` run is never
reported stale (it was never given the chance to fire).

## Configuration

None — it is always on. Change its level through `[lints]` like any other
lint:

```toml
[lints]
stale-expect = "allow"
```

## Fix behavior

`--fix` deletes the whole stale directive line for you — the mechanical
inverse of writing a silence. It is withheld when the line also names a
still-live or unjudged lint (deleting it would silence that lint too); remove
those by hand.

## Silencing

The intended resolution is to delete the now-pointless directive (which is
what `--fix` does). Change the level through `[lints]` as above if you need
to defer that.
