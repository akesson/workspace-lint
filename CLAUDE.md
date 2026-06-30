# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`workspace-lint` is a Rust CLI that enforces quality standards on a cargo
workspace via configurable lint checks. It emits clippy-style human output,
rustc-compatible JSON (for rust-analyzer's `check.overrideCommand`), or GitHub
Actions workflow commands. Its distinguishing trait: lints that need semantic
information are backed by `syn-workspace`, a from-scratch resolver built on
`syn` — **no rust-analyzer / SCIP subprocess required**. (One exception: to
resolve `OUT_DIR`-based generated code the binary runs a scoped `cargo check`
for crates that have both a `build.rs` and an `include!`; pass `--no-build-env`
to skip it. See **Generated code** in `README.md`.)

User-facing docs live in `README.md`; read it for the config surface and per-lint
options. This file covers the internal architecture.

## Workspace layout

Four crates under `crates/` (`members = ["crates/*"]`):

- **`workspace-lint`** — the binary. All lints, config loading, the diagnostic
  pipeline, and renderers. This is where ~all work happens.
- **`syn-workspace`** — publishable library. Loads a cargo workspace and
  resolves imports, `use ... as` renames, `pub use` re-export chains, cross-file
  module trees, and macro-body references. Deliberate non-goals: no type
  inference, no trait solving, no proc-macro execution (precision traded for
  sub-second speed). The resolved model is `Send + Sync`.
- **`workspace-lint-marker`** — zero-dep crate exporting the `expect!` / `allow!`
  macros (expand to nothing; workspace-lint scans the *source text* for them).
- **`syn-workspace-marker`** — zero-dep crate exporting `expansion_uses!` so
  macro authors annotate which items a macro's expansion references (avoids
  false positives in `unused-deps` / `unused-pub` / architecture lints).

The two marker crates and `syn-workspace` are published; CI gates them with
`cargo publish --dry-run`.

## Common commands

```sh
# Build / install the CLI
cargo build
cargo install --path crates/workspace-lint

# Run the tool against the current workspace (dogfoods this repo's own config)
cargo run -p workspace-lint
cargo run -p workspace-lint -- --fix          # apply MachineApplicable fixes
cargo run -p workspace-lint -- check file-size --glob "**/*.rs" --max-code-lines 500

# Tests (CI uses nextest)
cargo nextest run --workspace --locked
cargo test                                    # plain runner also works
cargo test --test cases                       # fixture taxonomy harness
cargo test --test dogfood                     # tool must pass on its own repo
cargo test --test fix_fixtures                # whole-tree --fix snapshots
cargo test -p syn-workspace                   # resolver tests

# Lint / format (CI denies warnings)
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check

# Coverage + CRAP gate (complexity-weighted coverage; CI fails on regressions)
cargo cov                                      # writes lcov.info
cargo cov-crap --fail-above
```

### Blessing snapshots after a deliberate output change

```sh
cargo insta accept                                       # inline snapshots in src/messages.rs
WORKSPACE_LINT_BLESS=1 cargo test --test cases           # tests/cases/**/expected.stderr
WORKSPACE_LINT_BLESS=1 cargo test --test fix_fixtures     # tests/fixtures/fix__*/expected/
```

Always review the diff before committing a blessed change.

## Architecture: the diagnostic pipeline

`main.rs` is the spine. For the default (no-subcommand) run:

1. **`config::load`** — config lives in *exactly one* of: standalone
   `.workspace-lint.toml`, or `[workspace.metadata.workspace-lint]` in
   `Cargo.toml`. Loading both is an error. Returns `(Config, Vec<Diagnostic>)`:
   the second is config-validation findings (`config` / `unknown-lint`) from
   `config::audit`, merged into the stream. The `config` module is a directory:
   `config/mod.rs` (schema + loading), `config/types.rs` (`LintLevel`,
   `LintLevels`, `GlobPattern`, `Globs`), `config/audit.rs` (the raw-TOML key
   audit).
2. **`run_all`** → `lints::registry(config)` builds `Vec<Box<dyn Lint>>`. A lint
   is enabled iff its effective level (`[lints]` override → `default` → built-in
   `warn`) isn't `allow`, and — for `LintId::requires_config` (policy) lints —
   its config table is present. Structural lints are on by default. The runner
   only pays `Workspace::load` if some enabled lint sets `needs_workspace`.
3. **`apply_suppression`** — scans for `expect!` / `allow!` macros and
   `# workspace-lint: expect(...)` comments (`directives.rs`), builds a
   `SuppressionMap` (`suppress.rs`), filters the stream, then appends
   `stale-expect` (unmatched `expect`s) and `unknown-lint` (directives naming a
   nonexistent lint), running both back through the map.
4. **`apply_lint_levels`** — rewrites each diagnostic to its effective level and
   **drops** `allow`-ed ones. Runs *after* suppression so appended findings are
   leveled too. `level_is_explicit` diagnostics (an `architecture` rule's own
   `severity`) are left untouched. Only a surviving `Deny` flips exit to 1.
5. **`fix::run`** (if `--fix`) — applies only `MachineApplicable` suggestions as
   byte-range replacements directly (no rustfix). `--fix` never inserts silence
   directives and never deletes files (except `unused-pub auto-delete`, gated on
   a clean git state as backup).
6. **`report_and_exit`** — `human` → stderr, `json`/`github` → stdout. Exit 1 iff
   any surviving diagnostic is `Deny`.

The `Lint` trait, `Requirements`, `LintContext`, and `registry` all live in
`lints/mod.rs`. `LintId` (the canonical list of every lint name, its
`workspace-lint::<kebab>` id, and short name) lives in `lints/lints_id.rs` and is
the single source of truth tying lints to config keys, snapshots, and fixtures.

`Diagnostic` (`diagnostic/mod.rs`) mirrors rustc's `DiagnosticSpan` so JSON output
is consumable by rust-analyzer unmodified. Build diagnostics with the grain-matched
helpers in `diagnostic/builder.rs` (`at_workspace` / `at_crate` / `at_file` /
`at_line`) so the `SilenceAnchor` — where the "silence with:" hint points — is
correct. Renderers are in `diagnostic/render/{human,json,github}.rs`.

## Adding a new lint

(From `lints/mod.rs` and `lints/lints_id.rs` — keep these in sync or tests fail.)

1. Create `lints/<name>/{mod.rs,config.rs,tests.rs}` implementing `Lint`.
2. Add a `LintId` variant + wire its `id()`/`short()` arms and `LintId::ALL`
   (kept alphabetical-by-id; asserted by a test).
3. Add one line in `lints::registry` gating it on its config block.
4. Add a scenario in `messages::scenarios()` (the registry-coverage test asserts
   every `LintId::ALL` variant has one).
5. Add fixtures under `tests/cases/<name>/`, and — if the lint emits a
   `MachineApplicable` structural fix — a `tests/fixtures/fix__<name>/` dir and
   list the variant in `FIXTURABLE_LINTS`.

## Testing model

- **`tests/cases/` (`cases.rs`)** — the primary harness, with a four-kind
  taxonomy per lint: `true_positives/`, `true_negatives/`,
  `known_false_positives/`, `known_false_negatives/`. Each case has a
  `workspace/` subtree (copied to a tempdir per run) and a path-normalized
  `expected.stderr`. The `known_*` buckets are a forcing function: if a known
  false positive stops firing (or a known false negative starts), the test fails
  to prompt promotion/deletion.
- **`tests/dogfood.rs`** — runs the built binary against this repo and asserts a
  clean exit. **This is the load-bearing quality gate.** Any new lint or stricter
  threshold must keep it green or ship a paired `expect!` directive documenting
  the exception. The dogfood config is `.workspace-lint.toml` at the repo root.
- **`src/messages.rs`** — every distinct diagnostic next to its rendered output
  in all three formats, as inline `insta` snapshots. Read top-to-bottom to audit
  the entire user-facing message surface.
- **`tests/fix_fixtures.rs`** — whole-tree before/after snapshots for `--fix`.

## Dogfooding

The tool lints itself, and the dogfood test enforces it. When you change a lint,
a threshold, or add files, run `cargo run -p workspace-lint` locally and resolve
findings either with a real fix or a load-bearing `expect!` (preferred over
`allow!` — `stale-expect` nudges you to remove it once the issue is gone). The
root `.workspace-lint.toml` carries explanatory comments for every `ignore` /
`expect` already in place; mirror that style when adding exceptions.

## Conventions worth knowing

- **`expect!` over `allow!`** everywhere except permanent, genuinely-unreachable
  silences. `expect` rots loudly via `stale-expect`; `allow` is silent forever.
- **`--fix` is author-respecting**: it resolves findings structurally but never
  writes a silence directive on your behalf — that's always a human decision
  (paste the directive the diagnostic prints).
- **`dioxus-rsx` is pinned to an exact patch** (`=0.7.9`) in the root
  `[workspace.dependencies]` — it's an unstable internal Dioxus crate. Upgrade
  procedure is documented inline in `Cargo.toml`; bump in lockstep with the
  `plugins/dioxus_rsx` AST walker.
- All workspace crates use `workspace = true` deps (enforced by the
  `centralized-deps` lint, on for this repo).
- Edition 2024, MSRV 1.88 (`[workspace.package] rust-version`).
