# workspace-lint

[![CI](https://github.com/akesson/workspace-lint/actions/workflows/ci.yml/badge.svg)](https://github.com/akesson/workspace-lint/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](crates/workspace-lint-marker/Cargo.toml)
[![MSRV](https://img.shields.io/badge/rustc-1.88%2B-blue)](https://github.com/rust-lang/rust/releases)
[![CRAP clean](https://img.shields.io/badge/CRAP-0%20over%20threshold-brightgreen)](https://github.com/minikin/cargo-crap)

A Rust CLI that enforces workspace quality standards via configurable lint checks.

Emits clippy-style human output, rustc-compatible JSON, or GitHub Actions
workflow commands so editor and CI integrations work without glue.

## Installation

```sh
cargo install --path crates/workspace-lint
```

For per-site silencing, also add the zero-dep marker crate to consumer
workspaces:

```toml
[dev-dependencies]
workspace_lint = { package = "workspace-lint-marker", version = "0.1" }
```

## Quick start

Create `.workspace-lint.toml` in your workspace root:

```toml
# Structural lints (centralized-deps, module-tree, feature-drift, unused-deps,
# unused-pub) are on by default at `warn`. The `[lints]` table is where you
# loosen (`allow`) or escalate (`deny`) — and where policy lints get enabled.
[lints]
centralized-deps = "deny"   # escalate to a CI-failing error
unused-pub       = "allow"  # turn one off

# Policy lints have no meaning without parameters, so their config table is the
# opt-in:
[[file-size.rules]]
glob = "**/*.rs"
max-code-lines = 500

[[freshness.rules]]
glob = "**/CLAUDE.md"
depends-on = "**/*.rs"
```

Then run:

```sh
workspace-lint
```

Exit code `0` means no `deny`-level finding survived. Exit code `1` means at
least one did.

## Checks

### centralized-deps

Verifies that all workspace crates use `workspace = true` for dependencies
instead of specifying versions directly. A structural lint — on by default at
`warn`. Escalate or silence it via the `[lints]` table:

```toml
[lints]
centralized-deps = "deny"   # or "allow" to turn off
```

### file-size

Enforces maximum code lines per file (blank lines and comments excluded, counted by [tokei](https://github.com/XAMPPRocky/tokei)).

```toml
[[file-size.rules]]
glob = "**/*.rs"
max-code-lines = 500
```

### crate-size

Enforces maximum total code lines per crate directory. Optionally filter which files to count with `include`.

```toml
[[crate-size.rules]]
glob = "crates/*"
max-code-lines = 5000
include = ["*.rs"]
```

### freshness

Checks that tracked files (e.g. `CLAUDE.md`) are newer than their dependencies. Useful for ensuring documentation stays up to date with source changes.

```toml
[[freshness.rules]]
glob = "**/CLAUDE.md"
depends-on = "**/*.rs"
```

Skipped automatically when the `CI` environment variable is set.

### cli-crate-version

Verifies that a locally installed CLI tool version matches the version of a crate in the workspace.

```toml
[[cli-crate-version.rules]]
command = ["wasm-bindgen", "--version"]
pattern = "wasm-bindgen (\\S+)"
crate = "wasm-bindgen"
```

### unused-deps

Scans workspace crates for dependencies declared in `Cargo.toml` that don't appear to be used in source files.

```toml
[unused-deps]
ignore = ["prost", "tonic"]
```

### unused-pub

Detects `pub` items that are never used outside the crate that declares them.
Resolver-backed (built on `syn-workspace`): it needs **no** SCIP index and
**no** `rust-analyzer` subprocess, so it runs the same locally and in CI. Items
that form part of a library crate's public API — re-exported via `pub use` or
otherwise externally reachable — are skipped automatically.

Two findings:
- **used only inside the crate** → suggests narrowing to `pub(crate)`.
- **unused anywhere** → suggests `pub(crate)` (or deletion, with `auto-delete`).

```toml
[unused-pub]
exclude-crates = ["api"]
allowlist = ["*Error", "main"]
kinds = ["function", "struct"]
exclude-paths = ["generated/**"]
suppress-intra-crate = false
auto-delete = false
```

| Option | Description |
|--------|-------------|
| `exclude-crates` | Crate names to skip entirely. |
| `allowlist` | Glob patterns matched against an item's canonical path (e.g. `*Error`, `main`). |
| `kinds` | Item kinds to check: `function` (alias `fn`), `struct`, `enum`, `union`, `trait`, `type`, `const`, `static`, `module` (alias `mod`), `macro`. Omit (empty) to check all kinds. An unrecognized kind is a config error. |
| `exclude-paths` | Glob patterns for source file paths to skip. |
| `suppress-intra-crate` | When `true`, report only items unused *anywhere* and drop the "used only inside the crate, consider `pub(crate)`" findings. Default `false`. |
| `auto-delete` | When `true`, the fix for an item that's unused everywhere becomes deletion instead of `pub(crate)` narrowing — but only when the containing file is git-tracked and clean (git is the backup). Dirty or untracked files downgrade the suggestion so `--fix` skips it. Default `false`. |

### architecture

Enforces workspace layering: which crates may import which canonical paths.
Each rule names a set of importing crates (`from`) and forbidden import targets
(`deny`), with an optional per-target `exceptions` escape hatch. Enabled
whenever at least one rule is present.

```toml
[[architecture.rules]]
name = "domain stays pure"          # optional, shown in the diagnostic
from = ["domain-*"]                 # importing-crate name globs (required)
deny = ["*::infra::*", "sqlx::*"]   # forbidden canonical-path globs (required)
exceptions = ["infra::Id"]          # canonical paths allowed despite `deny`
severity = "deny"                   # "warn" (default) or "deny"
reason = "domain must not depend on infrastructure"  # note: line
suggest = "move the shared type into a `core` crate" # help: line
```

Patterns use `::` as the segment separator; `*` matches one segment, `**`
matches zero or more. Only `use` bindings are inspected — a fully-qualified
call site (`other_crate::infra::Thing::new()`) without a `use` won't fire.

### module-tree

Structural integrity of the `mod` graph. A structural lint — on by default at
`warn`. Flags a `mod foo;` whose target (`foo.rs`, `foo/mod.rs`, or a
`#[path = "..."]` override) doesn't exist, and orphan `.rs` files under `src/`
that no `mod` chain reaches. Escalate or silence via `[lints] module-tree`.

### feature-drift

Detects drift between a crate's `[features]` table and its
`#[cfg(feature = "...")]` usage. A structural lint — on by default at `warn`.
Flags features declared in `[features]` but never gated in source, and
`#[cfg(feature = "...")]` references to features that aren't declared.
`default` is exempt (cargo handles it specially). Escalate or silence via
`[lints] feature-drift`.

> **Note:** `pub`-visibility tightening (`pub` → `pub(crate)` for items used
> only inside their own crate) is part of `unused-pub`, which is resolver-backed
> and covers that ground plus unused-everywhere items. The former standalone
> `visibility` lint was folded into it — migrate `[checks] visibility = true`
> to `[lints] unused-pub = "warn"`.

### Always-on lints

These lints take no configuration and run on every invocation (silence with
`[lints] <name> = "allow"`):

- **stale-git-index** — flags paths still tracked by git (`git ls-files`) that
  no longer exist on disk.
- **stale-expect** — fires when an `expect!` / `expect(...)` directive silences
  nothing because the underlying lint stopped firing (see
  [Silencing diagnostics](#silencing-diagnostics)).
- **config** — a structural problem in the config file itself: an unknown
  section or key (with a "did you mean …?" hint), or a policy lint enabled in
  `[lints]` with no rules table (so it would never fire).
- **unknown-lint** — a lint name that doesn't exist, referenced either in
  `[lints]` or in an `expect!`/`allow!` directive — caught instead of silently
  doing nothing.

All default to `warn`; escalate or silence them through `[lints]` like any
other lint. `config` and `unknown-lint` have one exception: a blanket
`[lints] default = "allow"` will **not** silence them (only an explicit
`[lints] config = "allow"` does), so a typo'd config can't hide itself.

## Commands

### Run all checks

```sh
workspace-lint
```

Runs expand rules first (if configured), then all enabled checks.

### Run a single check

```sh
workspace-lint check centralized-deps
workspace-lint check file-size --glob "**/*.rs" --max-code-lines 500
workspace-lint check freshness --glob "**/CLAUDE.md" --depends-on "**/*.rs"
workspace-lint check unused-deps --ignore prost --ignore tonic
```

### Mark freshness targets as up-to-date

```sh
workspace-lint done
```

Touches all files matched by freshness rules so they appear newer than their dependencies.

### Expand markers

```sh
workspace-lint expand --command "mise tasks" --glob "CLAUDE.md" --marker "MISE_TASKS" --auto-stage
```

Runs a command and injects its output between `<!-- MARKER_START -->` / `<!-- MARKER_END -->` comment pairs in matched files. With `--auto-stage`, modified files are `git add`ed automatically.

## Configuration

Configuration lives in **one** of two places (not both):

1. **Standalone file**: `.workspace-lint.toml` in the workspace root
2. **Cargo.toml metadata**: under `[workspace.metadata.workspace-lint]`

```toml
# In Cargo.toml:
[workspace.metadata.workspace-lint.lints]
centralized-deps = "deny"

[[workspace.metadata.workspace-lint.file-size.rules]]
glob = "**/*.rs"
max-code-lines = 500
```

The config has three kinds of sections: **lints** (the `[lints]` table plus a
per-lint params table like `[file-size]`), the **`expand`** task (see
[Expand markers](#expand-markers)), and **`macros`** resolver metadata (see
[External macro annotations](#external-macro-annotations)). Unknown sections and
keys are reported by the [`config`](#always-on-lints) lint.

### Migrating from the older format

The `[checks]` table and a standalone severity table are gone — everything lives
in `[lints]` now:

| Old | New |
|-----|-----|
| `[checks]`<br>`centralized-deps = true` | `[lints]`<br>`centralized-deps = "warn"` (or just rely on the on-by-default `warn`) |
| `[lints]`<br>`file-size = "deny"` (severity only) | unchanged — `[lints]` now also enables |
| `[checks]`<br>`visibility = true` | `[lints]`<br>`unused-pub = "warn"` (the `visibility` lint was folded in) |
| `kinds = ["method"]` | removed — `method`/`field`/`variant` were never real kinds |

### External macro annotations

Resolver-backed lints can't see items referenced only inside a macro's
expansion. For macros defined *outside* the workspace, declare the paths their
expansion references so `unused-deps` / `unused-pub` / `architecture` don't
report false positives:

```toml
[[macros.external]]
path = "tokio::main"                       # the macro (documentation only, for now)
expansion-uses = ["tokio::runtime::Builder"]
```

For macros defined *inside* the workspace, annotate the definition at the source
instead, with the zero-dep `syn-workspace-marker` crate
(`workspace_syn::expansion_uses!(...)`) or the equivalent
`// workspace-syn: expansion-uses(...)` comment directive.

## Output formats

`--message-format` picks the renderer (default `human`):

**`human`** (clippy-style, written to stderr):

```
warning: file exceeds 500 code lines (612)
 --> crates/web-api/src/handler.rs:1:1
  |
  = help: split #[cfg(test)] modules into separate test files
  = help: extract related structs, enums, or trait impls into their own modules
  = note: configured by [[file-size.rules]] glob = "**/*.rs"
help: if intentional, silence with:
  |
1 + workspace_lint::expect!(file_size);
  |
  = note: `#[warn(workspace_lint::file_size)]` on by default

workspace-lint: generated 1 warning
```

**`json`** (rustc-compatible per-line, written to stdout). Set rust-analyzer's
`check.overrideCommand` to `["workspace-lint", "--message-format=json", ...]`
and IDE squiggles + "Apply suggestion" code actions work without further glue.

**`github`** (Actions workflow command, written to stdout):

```
::warning file=crates/web-api/src/handler.rs,line=1,col=1,title=workspace-lint%3A%3Afile-size::file exceeds 500 code lines (612)
```

## Lint levels

`[lints]` is the one place a lint is enabled and leveled. Each value is
`allow`, `warn`, or `deny`, and the reserved `default` key sets the baseline
for every lint:

```toml
[lints]
default          = "warn"   # baseline for every lint (optional; built-in = "warn")
file-size        = "deny"   # per-lint override beats `default`
unused-pub        = "allow"  # loosen one off
```

**Precedence:** a per-lint entry beats `default`, which beats the built-in
baseline (`warn`). Use the kebab-case short name (no `workspace-lint::` prefix);
an unknown name is reported as [`unknown-lint`](#always-on-lints), not silently
ignored.

**What runs:** a lint runs when its effective level isn't `allow`, with one
extra condition for the *policy* lints (`file-size`, `crate-size`, `freshness`,
`cli-crate-version`, `architecture`) — they're meaningless without parameters,
so they additionally require their config table to be present. The *structural*
lints (`centralized-deps`, `module-tree`, `feature-drift`, `unused-deps`,
`unused-pub`) need no table and are therefore on by default. So:

- `default = "allow"` makes the whole tool opt-in — nothing runs until you set
  a lint to `warn`/`deny`.
- `default = "deny"` makes every enabled lint CI-failing.
- Leaving `default` unset keeps the batteries-included `warn` baseline.

**Exit code:** 1 iff at least one `deny`-level diagnostic survives suppression;
`allow`-ed diagnostics are dropped entirely before the tally.

## Silencing diagnostics

Silence directives are always author-written — `--fix` never inserts them
on your behalf. Every diagnostic prints the exact text to paste; two
forms, picked by file kind. The suggested directive uses `expect!` (and
its `expect(…)` comment form): silences a diagnostic but emits a
`workspace-lint::stale-expect` warning if the underlying lint stops
firing, so silences don't quietly rot.

**Rust files** — declarative macro from `workspace-lint-marker`:

```rust
workspace_lint::expect!(file_size);                // silence; warn if stale
workspace_lint::expect!(file_size, unused_pub);    // comma-separated list
workspace_lint::allow!(file_size);                 // silence permanently — no stale warning
```

**`Cargo.toml`, Markdown, anything non-Rust** — comment directive:

<!--
The `expect(unused-deps)` line below is illustrative; workspace-lint's own
scanner would treat it as a real directive against README.md and flag a
stale-expect on the next run. Silence it for this file:
workspace-lint: allow(stale-expect)
-->

```toml
# workspace-lint: expect(centralized-deps)
[dependencies]
serde = "1.0.200"

# workspace-lint: allow(unused-deps)   # permanent: lint can't reach this scope
```

Reach for `allow!` (and `# workspace-lint: allow(...)`) only when you want
a permanent silence — e.g. a file the lint genuinely can't reach (an
`unused-pub` item inside `exclude-crates`), or a constraint that will
never relax. `expect` is preferred everywhere else.

## Updating expected outputs

Two test-data sources have an auto-bless workflow:

```sh
# Inline diagnostic snapshots in src/messages.rs
cargo insta accept

# Whole-tree --fix fixtures under tests/fixtures/fix__*/
WORKSPACE_LINT_BLESS=1 cargo test --test fix_fixtures
```

Run either after a deliberate change to the rendered output, review the
diff, commit. The fix-fixture driver wholesale-replaces `expected/` so
removed files in the post-fix tree propagate correctly.

## CLI flags

- `workspace-lint` — run all configured checks.
- `workspace-lint check <rule> [opts]` — run a single check.
- `workspace-lint --message-format <human|json|github>` — pick the renderer.
- `workspace-lint --fix` — apply every diagnostic's `MachineApplicable`
  structural rewrite in place. Lints that don't ship a structural fix are
  reported but left untouched — `--fix` will never edit a file to suppress
  a finding it didn't actually resolve. Available structural fixes:
    - **centralized-deps** rewrites `serde = "1"` (or table forms) to
      `serde = { workspace = true }`, preserving `features`, `optional`,
      and `default-features` when present.
    - **unused-deps** deletes the dep line from `[dependencies]` /
      `[dev-dependencies]` / `[build-dependencies]`.
    - **unused-pub** tightens `pub fn`/`pub struct`/… to `pub(crate)` for
      items used only inside their own crate, by default. With
      `[unused-pub] auto-delete = true`, items that *appear unused
      entirely* are deleted — but only if the file is tracked by git AND
      has no uncommitted changes (git serves as the backup). When the
      file is dirty or untracked the deletion suggestion is downgraded
      to `MaybeIncorrect` and `--fix` skips it; the diagnostic carries
      a `note:` explaining why.

  `--fix` is non-destructive: it rewrites files but never deletes them.
  Idempotent: re-running on a clean tree is a no-op. To suppress a
  diagnostic without resolving it, paste the directive shown in the
  diagnostic's "if intentional, silence with:" hint manually — that's
  always an author decision, never `--fix`'s.
- `workspace-lint done` — mark `freshness` targets up-to-date.
- `workspace-lint expand` — substitute command output into marker blocks.
