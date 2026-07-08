# stale-git-index

Flags paths still tracked by git that no longer exist on disk — the residue
of a file deleted with `rm` instead of `git rm`, which lingers in the index
and reappears on the next checkout.

A structural (build-free) lint — on by default at `warn`.

## What it checks

Every path reported by `git ls-files` is checked against the working tree;
a tracked path with no file on disk is reported. (git is always invoked
through the `GIT_*`-scrubbing chokepoint, so the check is safe inside linked
worktrees.)

## Configuration

None. The lint is structural and takes no parameters — it is on by default.
Change its level (or turn it off) through `[lints]`:

```toml
[lints]
stale-git-index = "allow"
```

## Silencing

Change the level through `[lints]` as above. The real fix is usually
`git rm <path>` (or restoring the file), which clears the finding at the
source.
