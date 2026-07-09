# workspace-lint

[![CI](https://github.com/akesson/workspace-lint/actions/workflows/ci.yml/badge.svg)](https://github.com/akesson/workspace-lint/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](crates/workspace-lint-marker/Cargo.toml)
[![MSRV](https://img.shields.io/badge/rustc-1.88%2B-blue)](https://github.com/rust-lang/rust/releases)
[![CRAP clean](https://img.shields.io/badge/CRAP-0%20over%20threshold-brightgreen)](https://github.com/minikin/cargo-crap)

A Rust CLI that enforces workspace quality standards via configurable lint checks.

Emits clippy-style human output, rustc-compatible JSON, or GitHub Actions
workflow commands so editor and CI integrations work without glue.

Two tiers, one binary. The **structural lints** (file/crate size, module
tree, dependency hygiene, …) are build-free: `cargo metadata`
plus parsed manifests and sources, no compilation. The **semantic lints**
(`unused-deps`, `unused-pub`, `architecture`) are judged on a
**rustc-fidelity engine**: the workspace is compiled (a cached `cargo
check`), a compiler-plugin extractor emits each crate's resolved reference
graph, and the lints query the assembled workspace-global view — the
compiler's own answer, seeing through macro expansions, re-export chains,
`#[cfg]` variants, trait dispatch, and type inference. The trade is
explicit: **the workspace must compile** for the semantic tier, and a
pinned nightly toolchain must be installed (the tool prints the exact
install commands if it isn't — and offers to run them on an interactive
terminal). `--fast-only` runs just the build-free tier
— no toolchain, no compile. See [The semantic engine](#the-semantic-engine).

## Installation

```sh
cargo install --path crates/workspace-lint
```

For per-site silencing, also add the zero-dep marker crate to consumer
workspaces:

```toml
[dependencies]
workspace_lint = { package = "workspace-lint-marker", version = "0.1" }
```

## Quick start

Create `.workspace-lint.toml` in your workspace root:

```toml
# Structural lints (centralized-deps, orphan-file, feature-drift, unused-deps,
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
```

Then run:

```sh
workspace-lint
```

Exit code `0` means no `deny`-level finding survived. Exit code `1` means at
least one did.

## Checks

Each check below has a one-paragraph summary here and full documentation in
its own `DOC.md` (linked, and shown by `workspace-lint explain <lint>` or
`workspace-lint check <lint> --help`).

### centralized-deps

Verifies that every workspace crate declares its dependencies with
`workspace = true` instead of pinning a version directly, so the whole
workspace shares one source of truth. A structural lint — on by default at
`warn`. `--fix` rewrites inline deps and seeds missing ones into
`[workspace.dependencies]`.

```toml
[lints]
centralized-deps = "deny"   # or "allow" to turn off
```

Full docs: [`centralized_deps/DOC.md`](crates/wl-lints/src/centralized_deps/DOC.md) · `workspace-lint explain centralized-deps`

### file-size

Enforces a maximum number of *code* lines per file (blank lines and comments
excluded). For `.rs` files the count is *shipped* source only, so test code
is excluded and the budget matches `crate-size`. A policy lint — add a rule
to enable it.

```toml
[[file-size.rules]]
glob = "**/*.rs"
max-code-lines = 500
```

Full docs: [`file_size/DOC.md`](crates/wl-lints/src/file_size/DOC.md) · `workspace-lint explain file-size`

### crate-size

Enforces a maximum number of *shipped* code lines per crate directory — the
maintained product code, not the test mass that often dwarfs it. A policy
lint — add a rule to enable it.

```toml
[[crate-size.rules]]
glob = "crates/*"
max-code-lines = 5000
include = ["*.rs"]
```

Full docs: [`crate_size/DOC.md`](crates/wl-lints/src/crate_size/DOC.md) · `workspace-lint explain crate-size`

### duplicate-code

Name-invariant (Type-2) duplicate detection: flags groups of structurally
identical code regions — whole functions/methods, nested blocks, and runs of
consecutive statements — **even when local variable names and literal values
differ**. Each region is normalized (local bindings α-renamed to positional
placeholders in first-occurrence order, literals abstracted per kind) and
hashed; identical structures bucket together, so the pass is near-linear and
finds cross-crate copy-paste for free. Statement runs are what keep a clone
in one piece: a span copied mid-body into two otherwise-different functions
is reported once, maximally — not as fragments of whichever nested blocks
happen to match. Renames must be *consistent*: swapping two variables
changes the structure and correctly breaks the match. Called functions,
types, and field/method names are kept verbatim — two blocks that call
different functions are never clones.

It goes further than flagging: it **classifies what refactoring each group
calls for** (keep-one-and-redirect, delete-the-dead-copy, default-trait-
method, method-on-type, extract-a-component) and reshapes the help
accordingly. Because that consults the rustc call graph, **`duplicate-code`
is a semantic lint** — it needs a compiling workspace and `--fast-only`
skips it (the `--stats` readout is the build-free exception). Advisory only:
`--fix` never rewrites duplicates. Two noise filters and an extraction-cost
gate keep it quiet on convergent boilerplate and data tables; see the docs.

The `[duplicate-code]` table (all fields optional) enables it — or try it on
any workspace with zero config via the ad-hoc form:

```sh
workspace-lint check duplicate-code                     # defaults, zero config
workspace-lint check duplicate-code --cross-crate-only  # cross-crate copies only
workspace-lint check duplicate-code --stats             # threshold-tuning readout
```

Adopting on a legacy tree with hundreds of clones? Point `baseline` at a
checked-in file and run `workspace-lint --baseline-write`: only *new* or grown
duplication then fails CI (the SonarQube new-code-gate model), keyed on a
portable content fingerprint so entries survive renames and file moves. See the
baseline-ratchet section of the docs.

Full docs: [`duplicate_code/DOC.md`](crates/wl-lints/src/duplicate_code/DOC.md) · `workspace-lint explain duplicate-code`

### cli-crate-version

Verifies that a locally installed CLI tool's version matches the version of a
crate in the workspace. A policy lint — add a rule to enable it.

```toml
[[cli-crate-version.rules]]
command = ["wasm-bindgen", "--version"]
pattern = "wasm-bindgen (\\S+)"
crate = "wasm-bindgen"
```

Full docs: [`cli_crate_version/DOC.md`](crates/wl-lints/src/cli_crate_version/DOC.md) · `workspace-lint explain cli-crate-version`

### unused-deps

Flags dependencies declared in `Cargo.toml` that nothing references, judged
on the rustc engine's resolved reference graph (facade-aware, rename-aware,
crediting doc-fence and feature usage). A semantic lint — on by default at
`warn`; `--fast-only` skips it. Dev-dependencies are judged only when a
test-compiling entry is in the
[`[engine]` config matrix](#the-semantic-engine); the default matrix has one.

```toml
[unused-deps]
ignore = ["prost", "tonic"]
```

Full docs: [`unused_deps/DOC.md`](crates/wl-lints/src/unused_deps/DOC.md) · `workspace-lint explain unused-deps`

### unused-pub

Detects `pub` items that are never used across the workspace, judged on the
rustc engine's resolved reference graph — it needs **no** SCIP index and **no**
`rust-analyzer` subprocess, so it runs the same locally and in CI. Items
re-exported via `pub use`
are always skipped (narrowing them would break the re-export). So are types that
appear in the *public signature* of a more-visible item (a `pub fn` return type,
a `pub` field, a trait-impl associated type, …) — tightening those would not
compile (E0446 / `private_interfaces`). That exemption also covers a type a
builder macro promotes into its generated public `build()` signature, recognized
from the attribute: `typed_builder`'s `#[builder(build_method(into = T))]` and
`derive_builder`'s `#[builder(build_fn(error = "T"))]`.

**Publish-aware.** The lint can't see consumers outside your workspace, so it
treats a crate's public API as off-limits **only when the crate declares it has
external consumers — `publish = true`** (or a registry list) in its `Cargo.toml`.
Every other crate — `publish = false`, or, by default, *no* `publish` field — is
treated as **workspace-internal**: its `pub` items are checked, and anything not
used by another workspace crate is flagged for narrowing to `pub(crate)`. This is
the whole point of the lint at the workspace level — over-exposed internal APIs
get caught. So: **mark genuinely-published crates with `publish = true`** (you
likely want this anyway), and leave internal crates as they are.

If a crate accumulates several findings, the lint emits a one-line hint
suggesting `publish = true` — in case the flood means the crate really is
published. Set `assume-all-public = true` to opt out entirely and treat every
crate as having an external API (the pre-publish-aware behavior).

Two findings:
- **used only inside the crate** → suggests narrowing to `pub(crate)`.
- **unused anywhere** → suggests `pub(crate)` (or deletion, under
  `--fix-auto-delete`).

```toml
[unused-pub]
exclude-crates = ["api"]
allowlist = ["*Error", "main"]
kinds = ["function", "struct"]
exclude-paths = ["generated/**"]
suppress-intra-crate = false
assume-all-public = false
publish-hint-threshold = 3
```

`--fix` narrows an item used only inside its crate to `pub(crate)`.
`--fix-auto-delete` (CLI-only, gated on a clean git tree) instead
whole-item-deletes items unused everywhere, cascading through the entire
dead chain in one pass and trimming any `use` left dangling — with a
cfg-shadow veto, a `-D warnings` guard, re-run-to-converge, and a
clippy-unmask guard. Options can be scoped per crate via
[`[crates.<name>.unused-pub]`](#per-crate-configuration). The docs cover the
deletion cascade in full.

Full docs: [`unused_pub/DOC.md`](crates/wl-lints/src/unused_pub/DOC.md) · `workspace-lint explain unused-pub`

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

Patterns use `::` as the separator (`*` = one segment, `**` = zero or more).
`use` bindings, glob imports, and bare fully-qualified call sites are all
inspected against the compiler's resolved canonical paths, so re-export
chains and macro-generated references are judged too. Only production code is
checked. `architecture` is TOML-only — no `check` subcommand — but
`workspace-lint explain architecture` still works.

Full docs: [`architecture/DOC.md`](crates/wl-lints/src/architecture/DOC.md) · `workspace-lint explain architecture`

### orphan-file

Source files under `src/` that no declared `[engine]` config compiles. A
semantic lint — on by default at `warn`, skipped under `--fast-only`. rustc
never opens a file no `mod` chain reaches, so `dead_code` can't see it and
clippy says nothing; reachability here is rustc's own record of which files it
opened, unioned across the config matrix. A file the source *names* but no
config compiles (a platform-gated module, a `#[cfg(test)] mod tests;` under a
matrix without `cargo test`) is reported as a coverage gap, never as something
to delete. Escalate or silence via `[lints] orphan-file`.

Full docs: [`orphan_file/DOC.md`](crates/wl-lints/src/orphan_file/DOC.md) · `workspace-lint explain orphan-file`

### feature-drift

Detects drift between a crate's `[features]` table and its
`#[cfg(feature = "...")]` usage. A structural lint — on by default at `warn`.
Flags features declared in `[features]` but never gated in source, and
`#[cfg(feature = "...")]` references to features that aren't declared.
`default` is exempt (cargo handles it specially). Escalate or silence via
`[lints] feature-drift`.

Full docs: [`feature_drift/DOC.md`](crates/wl-lints/src/feature_drift/DOC.md) · `workspace-lint explain feature-drift`

> **Note:** `pub`-visibility tightening (`pub` → `pub(crate)` for items used
> only inside their own crate) is part of `unused-pub`, which is resolver-backed
> and covers that ground plus unused-everywhere items. The former standalone
> `visibility` lint was folded into it — migrate `[checks] visibility = true`
> to `[lints] unused-pub = "warn"`.

### Always-on lints

These lints take no configuration and run on every invocation (silence with
`[lints] <name> = "allow"`). Each has full docs via
`workspace-lint explain <lint>`:

- **stale-git-index** — flags paths still tracked by git (`git ls-files`) that
  no longer exist on disk.
  [docs](crates/wl-lints/src/stale_git_index/DOC.md)
- **stale-expect** — fires when an `expect!` / `expect(...)` directive silences
  nothing because the underlying lint stopped firing (see
  [Silencing diagnostics](#silencing-diagnostics)). Only lints that actually
  ran are judged. `--fix` deletes a stale directive line for you (unless it
  also names a still-live or unjudged lint).
  [docs](crates/workspace-lint/src/docs/stale-expect.md)
- **config** — a structural problem in the config file itself: an unknown
  section or key (with a "did you mean …?" hint), or a policy lint enabled in
  `[lints]` with no rules table (so it would never fire).
  [docs](crates/workspace-lint/src/docs/config.md)
- **unknown-lint** — a lint name that doesn't exist, referenced either in
  `[lints]` or in an `expect!`/`allow!` directive — caught instead of silently
  doing nothing.
  [docs](crates/workspace-lint/src/docs/unknown-lint.md)

All default to `warn`; escalate or silence them through `[lints]` like any
other lint. `config` and `unknown-lint` have one exception: a blanket
`[lints] default = "allow"` will **not** silence them (only an explicit
`[lints] config = "allow"` does), so a typo'd config can't hide itself.

## Commands

### Run all checks

```sh
workspace-lint
```

Runs all enabled checks. Any configured `[expand]` rules apply only under
`--fix` (on a clean git tree), since they rewrite files — a plain run never
mutates.

#### Exit codes

| Code | Meaning |
|------|---------|
| `0`  | Clean — no `deny`-level findings survived. |
| `1`  | Lint findings: at least one `deny`-level diagnostic. |
| `2`  | Operational error — unusable config, a failed subprocess, an IO error, or a dirty tree under `--fix`. |

Code `1` means the *linted code* has findings; code `2` means the *tool itself*
couldn't run. Keep them distinct in CI so a broken config doesn't look like a
lint failure (and vice versa).

### Run a single check

```sh
workspace-lint check centralized-deps
workspace-lint check file-size --glob "**/*.rs" --max-code-lines 500
workspace-lint check duplicate-code --cross-crate-only
workspace-lint check unused-deps --ignore prost --ignore tonic
```

`workspace-lint check <lint> --help` prints that lint's full documentation
after its options (`-h` stays terse).

### Explain a lint

```sh
workspace-lint explain unused-pub
workspace-lint explain architecture   # works even for TOML-only lints
```

Prints a lint's full documentation to stdout — the same text `check <lint>
--help` shows, and the source of each lint's `DOC.md`. Accepts the short name
(`unused-pub`) or the full id (`workspace-lint::unused-pub`), and covers every
lint, including the pipeline meta lints `config` / `stale-expect` /
`unknown-lint`.

### Initialize a config

```sh
workspace-lint init
```

Writes a commented starter `.workspace-lint.toml` in the current directory. It
enables the batteries-included structural lints, escalates `centralized-deps` to
`deny`, declares the engine matrix at `["cargo build", "cargo test"]` (so
test-gated usage and dev-dependencies are judged — drop `"cargo test"` for a
single, faster pass), and includes ready-to-uncomment blocks for every policy
lint.

`init` must be run at the **workspace root**: the root `Cargo.toml` must declare
a `[workspace]` table and the current directory must be that root (not a member
subdir) — a lone `[package]` is rejected. It refuses to overwrite an existing
config (a standalone file or `[workspace.metadata.workspace-lint]`); pass
`--force` to replace an existing `.workspace-lint.toml`.

### Expand markers

```sh
workspace-lint expand --command "mise tasks" --glob "CLAUDE.md" --marker "MISE_TASKS" --auto-stage
```

Runs a command and injects its output between `<!-- MARKER_START -->` / `<!-- MARKER_END -->` comment pairs in matched files. With `--auto-stage`, modified files are `git add`ed automatically. Because it rewrites files, the subcommand requires a clean git working tree (override with `--allow-dirty`). Configured `[expand]` rules are also applied as part of a `--fix` run.

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

The config has these kinds of sections: **lints** (the `[lints]` table plus a
per-lint params table like `[file-size]`), the per-crate **`[crates.<name>]`**
tier (see [Per-crate configuration](#per-crate-configuration)), the **`expand`**
task (see [Expand markers](#expand-markers)), and **`macros`** resolver metadata
(see [External macro annotations](#external-macro-annotations)). Unknown sections
and keys are reported by the [`config`](#always-on-lints) lint.

### Migrating from the older format

The `[checks]` table and a standalone severity table are gone — everything lives
in `[lints]` now:

| Old | New |
|-----|-----|
| `[checks]`<br>`centralized-deps = true` | `[lints]`<br>`centralized-deps = "warn"` (or just rely on the on-by-default `warn`) |
| `[lints]`<br>`file-size = "deny"` (severity only) | unchanged — `[lints]` now also enables |
| `[checks]`<br>`visibility = true` | `[lints]`<br>`unused-pub = "warn"` (the `visibility` lint was folded in) |
| `kinds = ["method"]` | removed — `method`/`field`/`variant` were never real kinds |

### External macro annotations (obsolete)

The semantic engine compiles the workspace with rustc, so references made
inside macro expansions are ordinary reference-graph edges — no annotation
needed. The old `[[macros.external]]` config section now draws a `config`
finding telling you to delete it, and the `expansion_uses!` /
`# workspace-lint: expansion-uses(...)` source annotations are no longer read.

### Naming quirks the matcher smooths over

Macro-expansion contracts (a derive that references its runtime crate, an
attribute that names a module in a string) need no special handling: the
engine sees the expanded code, so those references are ordinary edges.
What remains heuristic is pure *naming*: a dep whose hyphen-stripped name
matches a referenced lib target is credited (`md-5` declares the crate
whose lib is `md5`), so a rename-by-convention never reads as unused. This
only ever *suppresses* a finding — never creates one. For everything else
there is the `[unused-deps] ignore` knob.

## The semantic engine

The semantic lints (`unused-deps`, `unused-pub`, `architecture`) are judged
on the compiler's own resolution, in two phases:

1. **Extract.** The binary embeds the source of a compiler-plugin extractor
   (a [Dylint](https://github.com/trailofbits/dylint) lint that never
   warns), materializes it to a per-version cache, builds it once per
   toolchain, and drives one `cargo check` per configured entry over your
   workspace with the plugin loaded. Each compiled crate writes an IR
   fragment — its definitions and resolved reference edges — under
   `target/workspace-lint/ir/<config>/`. Cargo's own caching applies:
   unchanged crates aren't recompiled and their fragments stay valid, so
   warm runs cost roughly a no-op `cargo check`.
2. **Assemble.** The stable side joins the fragments into a
   workspace-global reference graph (cross-crate identities via
   `DefPathHash`; results unioned across the config matrix) that the lints
   query.

### The config matrix

Each entry is a **real cargo command the project runs** — the declared
matrix *is* the support matrix, in the vocabulary you already use:

```toml
[engine]
# One extraction pass per entry; verdicts union across them. The first
# entry is primary. Absent table = ["cargo build", "cargo test"].
configs = [
    "cargo build",
    "cargo nextest run",
    "cargo build --target wasm32-unknown-unknown -p app",
]
```

Supported verbs: `build` / `check` / `clippy` (the plain lib+bins
universe), `test` / `nextest run` (the test universe), `bench`. Supported
flags: `--target`, `-p`/`--package`, `--features`/`-F` / `--all-features` /
`--no-default-features`, `--workspace`/`--all`, and `--tests`/`--benches`;
universe-neutral flags (`--locked`, `--quiet`, `--jobs`, …) are accepted and
ignored. The engine reproduces each
command's compilation universe with a `cargo check`-fidelity pass — sound
because check compiles the same units under the same cfgs; only codegen
differs (`cargo nextest run` and `cargo test` normalize to one pass). The
parser is deliberately strict: anything it doesn't positively recognize is
a config error with guidance, never silently dropped — a swallowed flag
would silently change which code the lints judge.

`#[cfg]`-gated code exists only under the config that compiles it, so the
matrix is what keeps test-gated usage from reading as dead: an item used
only from `#[cfg(test)]` code is *used* under `cargo test` and the union
clears it. A test entry is also what makes dev-dependencies judgeable at
all — which is why the default matrix includes one. The output names which
configs ran — code compiled under configs outside the matrix can still
cause false positives, and the diagnostics say so.

### Toolchain requirement

The extractor builds against a **pinned nightly** (it links `rustc`
internals; the pin ships inside the binary and moves only with tool
releases). The first semantic run checks for it and fails with the exact
commands if anything is missing:

```sh
rustup toolchain install <pin> --profile minimal \
    --component rustc-dev --component llvm-tools-preview
cargo install dylint-link --locked
```

On an interactive terminal the tool offers to run each of those for you
(`proceed? [y/N]`) and continues the run once they succeed — provisioning
a new machine is one keypress. Without a terminal (CI, pipes) it never
prompts and fails with the commands above.

Your own code never compiles on that nightly — it is the *extractor's*
build toolchain; your workspace compiles on whatever toolchain cargo would
normally pick.

### Failure semantics

If the workspace doesn't compile under a configured entry, the fast-tier
lints still report, then the run fails naming the config, with cargo's
diagnostics replayed verbatim. The full tier never silently degrades to a
weaker analysis — the explicit degradation path is `--fast-only`.

### Hooks and CI

- **pre-commit**: `workspace-lint --fast-only` — build-free, sub-second.
- **pre-push / CI**: `workspace-lint` — the full tier; warm runs are
  cheap, and CI caches the extractor build per toolchain pin.

### Performance

Warm runs are cheap, but the cost **scales with `[engine] configs`**: each entry
is a full `cargo check` freshness pass over the whole workspace on the pinned
nightly, so N entries cost roughly N× the warm floor — not a fixed overhead. That
check runs in the engine's own `target/dylint` build universe, separate from the
`target/` your everyday `cargo` uses, so the two never share artifacts (the first
full run after a normal build is cold). `--fast-only` skips the tier, and this
cost, entirely.

A workspace with a **non-deterministic `build.rs`** (e.g. Dioxus / `manganis`
asset codegen) can need **one extra warm run to settle**: the build script
re-fires inside the engine's freshness universe until its generated outputs
stabilize, so the first warm run after a change re-checks more than the steady
state (observed 45s → 23s → 6s across three consecutive runs). That is the build
script converging, not a workspace-lint regression — later runs are fast.

On a very large workspace the assemble phase holds every crate's IR fragment in
memory at once, so peak RSS grows with the workspace (multiple GB on
thousand-file trees).

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
unused-pub       = "allow"  # turn one off
```

**Precedence:** a per-lint entry beats `default`, which beats the built-in
baseline (`warn`). Use the kebab-case short name (no `workspace-lint::` prefix);
an unknown name is reported as [`unknown-lint`](#always-on-lints), not silently
ignored.

**What runs:** a lint runs when its effective level isn't `allow`, with one
extra condition for the *policy* lints (`file-size`, `crate-size`,
`cli-crate-version`, `architecture`, `duplicate-code`) — they're meaningless
without parameters (or, for `duplicate-code`, noisy enough to demand deliberate
opt-in), so they additionally require their config table to be present. The *structural*
lints (`centralized-deps`, `orphan-file`, `feature-drift`, `unused-deps`,
`unused-pub`) need no table and are therefore on by default. So:

- `default = "allow"` makes the whole tool opt-in — nothing runs until you set
  a lint to `warn`/`deny`.
- `default = "deny"` makes every enabled lint CI-failing.
- Leaving `default` unset keeps the batteries-included `warn` baseline.

**Exit code:** 1 iff at least one `deny`-level diagnostic survives suppression;
`allow`-ed diagnostics are dropped entirely before the tally.

### Per-crate configuration

Lint config cascades through three tiers, narrowest winning: the global
`[lints]` table → a per-crate `[crates.<name>]` block → in-code `expect!` /
`allow!` directives. The per-crate tier lives in the **same** root config (keyed
by Cargo package name), so the whole policy stays in one file.

```toml
[lints]                       # tier 1 — global
default   = "warn"
file-size = "deny"

[crates.legacy.lints]         # tier 2 — per-crate levels (mirrors [lints])
file-size = "allow"           # stop denying file-size in `legacy` only
default   = "allow"           # …or opt the whole crate out wholesale

[crates.api.lints]
unused-pub = "deny"           # turn a globally-loosened lint back on, here

[crates.api.unused-pub]       # tier 2 — per-crate params
allowlist = ["*Builder"]

[crates.worker.unused-deps]
ignore = ["prost", "tonic"]
```

**Per-crate levels — every lint.** A per-crate `[crates.<name>.lints]` entry
overrides the global level for that crate; a per-crate `default` sets the
crate's baseline (and, set to `allow`, opts the whole crate out). Keys with no
per-crate entry fall through to the global tier.

**Per-crate params — `unused-deps` and `unused-pub` only.** These are the lints
whose params are workspace-flat (an `ignore` list, an allowlist, …) with no glob
escape hatch. `file-size` and `crate-size` already scope per-crate
through their globs, so a `[crates.<name>.file-size]` (or crate-size) block is
a [`config`](#always-on-lints) error that redirects you to a glob rule — one
obvious way, not two. Any other per-crate params block (`cli-crate-version`,
`architecture`, …) is a `config` error too. A present
`[crates.<name>.unused-deps]` / `unused-pub` section **wholesale-replaces** the
global section for that crate (predictable: the crate's config is exactly what's
written).

**Validation.** A `[crates.<name>]` whose `<name>` isn't a workspace member is a
`config` error with a "did you mean …?" hint — centralized per-crate config
can't silently rot against a renamed or removed crate.

## Silencing diagnostics

Silence directives are author-written — every diagnostic prints the exact text
to paste, in one of two forms picked by file kind. The suggested
directive uses `expect!` (and its `expect(…)` comment form): it silences a
diagnostic but emits a `workspace-lint::stale-expect` warning if the underlying
lint stops firing, so silences don't quietly rot.

Rust files accept both a macro form (`workspace_lint::expect!(unused_pub);`) and
a **line-comment** form (`// workspace-lint: expect(unused-pub)` written above
the item) — the latter needs no `workspace-lint-marker` dependency.

**Rust files** — declarative macro from `workspace-lint-marker`:

```rust
workspace_lint::expect!(file_size);                // silence; warn if stale
workspace_lint::expect!(file_size, unused_pub);    // comma-separated list
workspace_lint::allow!(file_size);                 // silence permanently — no stale warning
```

**`Cargo.toml`, Markdown, anything non-Rust** — comment directive:

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
- `workspace-lint check <rule> [opts]` — run a single check
  (`check <rule> --help` shows that lint's full documentation).
- `workspace-lint explain <lint>` — print a lint's full documentation.
- `workspace-lint --message-format <human|json|github>` — pick the renderer.
- `workspace-lint --fast-only` — run only the build-free lints: no
  compile, no pinned-toolchain requirement. The semantic lints are
  *skipped* (a `check <semantic-lint> --fast-only` is a hard error rather
  than a silent no-op). The right mode for fast pre-commit hooks.
- `workspace-lint --fix` — apply every diagnostic's `MachineApplicable`
  structural rewrite in place. **Requires a clean git working tree**
  (override with `--allow-dirty`) so the whole change is reviewable as one
  `git diff`. Available structural fixes:
    - **centralized-deps** rewrites `serde = "1"` (or table forms) to
      `serde = { workspace = true }`, preserving `features`, `optional`,
      and `default-features` when present.
    - **unused-deps** deletes the dep line from `[dependencies]` /
      `[dev-dependencies]` / `[build-dependencies]`.
    - **unused-pub** tightens `pub fn`/`pub struct`/… to `pub(crate)` for
      items used only inside their own crate.
    - **stale-expect** deletes the whole directive line once the lint it
      silenced stops firing — the mechanical inverse of writing a silence
      directive. Withheld when the line also names a still-live or
      unjudged lint (deleting it would silence that lint too).

  `--fix` is non-destructive: it rewrites files but never deletes them.
  Idempotent: re-running on a clean tree is a no-op. It never *writes* a
  silence directive on your behalf — that's always a human decision (paste
  the directive the diagnostic prints); deleting a stale one, which only
  reduces suppression, is the one exception.
- `workspace-lint --fix-auto-delete` — everything `--fix` does, plus:
  **unused-pub** items that *appear unused entirely* are whole-item
  DELETED, cascading through the entire dead chain in one pass and
  trimming any `use` left dangling (see the unused-pub section). Only
  git-tracked-clean files are touched — the deletion's backup is
  `git checkout`. A manual, CLI-only operation by design: there is no
  config equivalent, so CI `--fix` runs can never delete code.
- `workspace-lint expand` — substitute command output into marker blocks.

### Generated code (`include!`)

Generated code **participates in analysis** — the semantic lints judge the
compiler's own view, where every `include!` (literal, `CARGO_*`, and
`OUT_DIR`-based) is already spliced, so a dependency or `pub` item used only
from generated code is seen as used. Findings anchored *in* a generated file
are dropped (a generated `pub fn` is never reported unused): the structural
lints' build-free walk resolves literal / `CARGO_*` includes for the drop
set, and the semantic lints skip anything under cargo's target directory.
