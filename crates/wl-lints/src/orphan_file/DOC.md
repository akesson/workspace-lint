# orphan-file

Finds `src/**.rs` files that no declared config ever compiles.

A semantic lint — it needs the rustc-backed engine, so it does not run
under `--fast-only`. On by default at `warn`.

## What it checks

rustc never opens a file that no `mod` chain reaches. So a source file left
behind by a rename is invisible to the entire toolchain: `dead_code` cannot
see it, and clippy says nothing. This lint is the only thing that will.

Reachability is taken from rustc itself — the union, across every `[engine]`
config, of the source files the compiler actually opened. That matters,
because the module graph is only settled *after* macro expansion. All of the
following are resolved for free, and none can be resolved by reading source:

    #[cfg_attr(unix, path = "unix.rs")] mod imp;   // the platform idiom
    macro_rules! declare { ($n:ident) => { mod $n; }; }
    static TABLE: [u8; 4] = include!("table.rs");  // expression position
    const S: &str = include_str!("snippet.rs");

Two findings are emitted:

- **orphan source file** — no config compiled it, and nothing in the crate's
  source names it. It is safe to delete.
- **no declared config compiles this file** — the crate's source *does* name
  it, but no config in the `[engine]` matrix opens it. Nothing in it is
  checked. This is a coverage gap, never a suggestion to delete, and it is
  always a warning: `orphan-file = "deny"` cannot turn it into a build
  failure.

The second finding is what a platform-gated module produces when the matrix
has no config for that platform, and what a `#[cfg(test)] mod tests;` in its
own file produces when the matrix omits `cargo test`. Widen the matrix:

    [engine]
    configs = ["cargo build", "cargo test"]

Being named is only proof that the file must not be *accused*, not that it is
alive. A module gated on a `cfg` no target satisfies is named, and dead, and
indistinguishable from a platform module you simply never built. The lint
stays silent about the difference rather than risk telling you to delete a
live file.

Deleting a file is only ever suggested when *both* tiers fail to see it, so a
gap in either one degrades to the harmless finding rather than a destructive
one.

## Configuration

None. The lint is structural and takes no parameters. Change its level (or
turn it off) through `[lints]`:

```toml
[lints]
orphan-file = "deny"   # or "allow" to turn it off
```

A member that no `[engine]` config compiles at all — as with a scoped
`configs = ["cargo build -p foo"]` — is skipped entirely rather than having
every one of its files reported.

## Silencing

Change the level through `[lints]` as above, or write an `expect` directive
in the affected file. Prefer `expect` (warns via `stale-expect` once
resolved) over the permanent `allow`.
