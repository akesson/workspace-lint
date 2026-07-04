# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`workspace-lint` is a Rust CLI that enforces quality standards on a cargo
workspace via configurable lint checks. It emits clippy-style human output,
rustc-compatible JSON, or GitHub Actions workflow commands. Its distinguishing
trait: the semantic lints (`unused-pub` / `unused-deps` / `architecture`) are
backed by a **rustc-fidelity engine** — a vendored, nightly-pinned Dylint
extractor emits per-crate IR fragments and a stable-side assembler joins them
into the workspace-global reference graph the lints judge. The workspace must
compile; `--fast-only` runs just the build-free structural lints.

User-facing docs live in `README.md`; read it for the config surface and per-lint
options. This file covers the internal architecture.

## Workspace layout

Six crates under `crates/` (`members = ["crates/*"]`), plus the excluded
nightly `extractor/` package:

- **`workspace-lint`** — the binary. The diagnostic pipeline, config loading,
  and `registry.rs` (the composition root that binds enabled lints to a loaded
  `Config`). Thin now that lints and the diagnostic types have moved out.
- **`wl-lints`** — every lint implementation (one `<name>/` dir each), the
  `Lint` trait / `LintId` / `LintContext` vocabulary, the config *primitives*
  (`config.rs`: `LintLevel` / `GlobPattern` / …) and per-lint `*Config` structs,
  and the shared `git` (the `GIT_*`-scrub chokepoint) and `util` helpers. The
  binary's `Config` re-exports the per-lint config structs so the TOML schema is
  unchanged; the *registry* stays in the binary (it's where lint impls meet
  config loading).
- **`wl-diagnostic`** — the diagnostic vocabulary (`Diagnostic` / `Span` /
  `SilenceAnchor` / `Suggestion`), the `DiagnosticBuilder`, and the three
  renderers (`human` / `json` / `github`). A leaf crate consumed by both
  `wl-lints` and the binary pipeline.
- **`wl-engine`** — stable library: both tiers' data layer. `fast/` is the
  build-free `FastModel` (cargo metadata, manifests, a lean syntactic module
  walker); `semantic/` assembles extracted IR fragments into the
  `SemanticModel` (cross-crate join on `DefPathHash`, cfg-matrix union);
  `orchestrate/` vendors, builds, and drives the extractor dylib.
- **`wl-ir`** — the serde-only IR contract between the extractor and the
  assembler (publishable; schema-versioned).
- **`workspace-lint-marker`** — zero-dep crate exporting the `expect!` / `allow!`
  macros (expand to nothing; workspace-lint scans the *source text* for them).
- **`extractor/`** (workspace-excluded, own toolchain pin + lockfile) — the
  nightly Dylint `LateLintPass` that walks `TyCtxt` and emits IR fragments.
  Ships vendored inside the binary; its gate is the probe suite in
  `extractor/tests`.

Strict layering: `workspace-lint` → `wl-lints` → {`wl-diagnostic`, `wl-engine`}.
`wl-lints` and `wl-diagnostic` are `publish = false`, so the deny-level
`unused-pub` dogfood judges their `pub` APIs workspace-internally (which is why
a helper reachable only from another crate's *test* code needs an allowlist
entry — see the `render_one` note in `.workspace-lint.toml`).

`workspace-lint-marker` and `wl-ir` are published; CI gates them with
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
cargo test --test corpus -- --ignored         # full-tier corpus subset (compiles them)
(cd extractor && cargo test)                  # extractor probe suite (pinned nightly)

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
   `config/mod.rs` (schema + loading), `config/audit.rs` (the raw-TOML key
   audit); the strongly-typed primitives (`LintLevel`, `LintLevels`,
   `GlobPattern`, `Globs`) live in `wl_lints::config` and are re-exported by
   `config/mod.rs`.
2. **`run_all`** → `registry::registry(config)` builds `Vec<Box<dyn Lint>>`. A lint
   is enabled iff its effective level (`[lints]` override → `default` → built-in
   `warn`) isn't `allow`, and — for `LintId::requires_config` (policy) lints —
   its config table is present. Structural lints are on by default. Each lint
   declares `Requirements { needs_fast, needs_semantic }`: the runner pays
   `FastModel::load` (cargo metadata + manifests, build-free) and the
   rustc-backed extraction only if some enabled lint asks. Under `--fast-only`
   the semantic lints are *retained out* of the registry (skipped, never run
   model-less). A memberless workspace skips the semantic tier entirely.
   See **The engine** below for what the semantic tier does.
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
   a clean git state as backup). The IR's byte offsets are ON-DISK positions
   (CRLF-safe — see `wl_ir::Span`), which is what makes them a valid write
   surface.
6. **`report_and_exit`** — `human` → stderr, `json`/`github` → stdout. Exit 1 iff
   any surviving diagnostic is `Deny`.

The `Lint` trait, `Requirements`, and `LintContext` live in `wl-lints`'s
`lib.rs`; `registry` (+ `level_on`) is the binary's `registry.rs`, the
composition root that binds them to a `Config`. `LintId` (the canonical list of
every lint name, its `workspace-lint::<kebab>` id, and short name) lives in
`wl-lints/src/lints_id.rs` and is the single source of truth tying lints to
config keys, snapshots, and fixtures.

`Diagnostic` (the `wl-diagnostic` crate) mirrors rustc's `DiagnosticSpan` so JSON
output is consumable by rust-analyzer unmodified. Build diagnostics with the
grain-matched helpers in `wl_diagnostic::builder` (`at_workspace` / `at_crate` /
`at_file` / `at_line`) so the `SilenceAnchor` — where the "silence with:" hint
points — is correct. Renderers are in `wl_diagnostic::render::{human,json,github}`;
`render_one` renders a single diagnostic (used by the message-surface tests).

## The engine (semantic tier)

Two phases, forced by rustc's per-crate compilation model:

- **Phase 1 — extract** (`wl-engine::orchestrate`): the binary vendors the
  `extractor/` sources (embedded at compile time via build.rs), materializes
  them to `~/.cache/workspace-lint/<binary-version>/`, builds the dylib once
  per toolchain, and runs one `dylint::run` (a wrapped `cargo check`) per
  `[engine] configs` entry with `--workspace` (a non-virtual workspace would
  otherwise make members mere dependency units — unlintable when warm). Each
  crate's `LateLintPass` writes an `IrFragment` (defs + resolved reference
  edges, `wl-ir` schema) to `target/workspace-lint/ir/<config>/` under a
  canonical name (`<crate>[@bin][+test].json` — a package's bin may share the
  lib's crate name; build scripts emit references-only `<pkg>@build.json`,
  package-keyed since every one is crate `build_script_build`). Members
  compiled a second time as Build-mode host deps (another member's build.rs
  or a proc-macro consumer) are skipped — their `DefPathHash` generation
  differs and would clobber the Check-mode fragment. Cargo freshness keeps
  fragments valid without re-runs; the **completeness guard** covers the one
  hole (`WL_IR_OUT` isn't in cargo's fingerprint): expected-vs-present check,
  one forced re-lint, then a hard error. The re-lint force lever is a dylib
  *generation* bump (`orchestrate/relink.rs`): the dylib reaches dylint via
  an mtime-keyed hard-link path, so a mtime bump changes the `DYLINT_LIBS`
  value every member unit env-dep-tracks — including units whose dep-info
  lost the dylib *file*-dep by recompiling as a non-primary unit (dylint's
  driver only file-deps the dylib when `CARGO_PRIMARY_PACKAGE` is set, e.g.
  a `test = false` lib under `--tests`; a plain mtime bump alone bricked
  such workspaces). Build fragments
  are enforced *across* the run's config dirs (a build unit compiles once per
  shared target dir) and deduped newest-wins. Whole-workspace runs also
  **prune** stale fragments (renamed crates, older naming schemes) so they
  can't silently assemble forever.
- **Phase 2 — assemble** (`wl-engine::semantic`): pure stable data work.
  Cross-crate join on `DefPathHash` (`ItemFact::key` ↔ `RefEdge::to_key` —
  display paths are NOT stable across crates), **global across configs**
  (`join.rs`: the config dirs share one cargo target dir — one compilation
  universe — so a `+test`/bench unit's edge to a dependency's *plain* rlib,
  a generation cargo freshness leaves only in the primary dir, resolves by
  exact hash and is translated onto the referring config's own def for that
  identity; if the referring config never extracted the target crate at all —
  `test`/`bench = false` targets — the reach is credited at identity level,
  `ForeignReach`). Build fragments alone fall back to a display-path join,
  their Build-mode hash generation never being extracted anywhere. Across
  configs, verdicts union on the `(crate, def_path)` identity (hash and
  identity are duals: the hash is exact within the universe, the identity
  stable across re-extraction). Derived indexes (reachability,
  re-export chains, signature exposure, dispatch, dep matrix) live here, not
  in the extractor: the emit vocabulary stays minimal ground facts, every
  derivation testable on stable.

Failure semantics: toolchain preflight errors carry paste-able remediation
(snapshotted in `messages.rs`; `EngineError::remediation` exposes the same
command as an argv — lockstep-tested — and on an interactive terminal the
CLI offers to run it, `provision.rs`); a compile failure names the config and
replays cargo's diagnostics; the tier never silently degrades — the explicit
degradation is `--fast-only`.

**Bumping the pinned nightly** (`extractor/rust-toolchain.toml`; track
*dylint's* pin, not the newest nightly): update the pin file, expect the
`register_late_pass` signature edit in `extractor/src/lib.rs` (documented
inline at the call), run the probe suite (`cd extractor && cargo test`), and
update the CI pin strings (`ci.yml`, `corpus.yml`, `extractor.yml` cache
keys). Verified cadence: a ~10-week jump needed exactly one edit.

## Adding a new lint

(Spans `wl-lints` and the binary — keep them in sync or tests fail. The trait
lives in `wl-lints/src/lib.rs`, the registry in the binary's `registry.rs`.)

1. Create `crates/wl-lints/src/<name>/{mod.rs,config.rs,tests.rs}` implementing
   `Lint`. Export the lint struct + its constructor and (if any) `*Config` `pub`
   so the registry can reach them; keep internal helpers `pub(crate)`.
2. Add a `LintId` variant in `wl-lints/src/lints_id.rs` + wire its `id()`/`short()`
   arms and `LintId::ALL` (kept alphabetical-by-id; asserted by a test).
3. Add one line in the binary's `registry::registry` gating it on its config
   block (and re-export its `*Config` from `config/mod.rs` if it has one).
4. Add a scenario in the binary's `messages::scenarios()` (the registry-coverage
   test in `registry.rs` asserts every `LintId::ALL` variant has one).
5. Add fixtures under `tests/cases/<name>/`, and — if the lint emits a
   `MachineApplicable` structural fix — a `tests/fixtures/fix__<name>/` dir and
   list the variant in `FIXTURABLE_LINTS` (in the binary's `registry.rs`).

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
- **`extractor/tests/probe.rs`** — the golden-IR spine, tier 1: compiles the
  probe crate and asserts extraction policy (span projection, vis tokens,
  edge metadata) on the emitted fragment. Runs on the pinned nightly
  (`cd extractor && cargo test`); its own 3-OS CI workflow triggers on
  extractor/wl-ir changes.
- **`wl-engine/src/semantic/tests.rs`** — tier 2: hand-crafted fragment
  fixtures through the assembler on stable, in every `cargo test`.
- **`tests/corpus.rs`** — real third-party crates (git submodules under
  `corpus/`): a fast-tier smoke over every checked-out crate in the default
  suite (skips cleanly when submodules are absent), and an `#[ignore]`d
  full-tier subset run driven by the scheduled `corpus.yml` workflow.

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
- All workspace crates use `workspace = true` deps (enforced by the
  `centralized-deps` lint, on for this repo).
- **Never `Command::new("git")` directly** — spawn git via `wl_lints::git::command`
  (src) or `common::git` (tests). Both scrub the repo-pinning `GIT_*` environment:
  git exports an absolute `GIT_DIR` to hooks in linked worktrees, and an
  unscrubbed child git operates on the *invoker's* repository (this once let
  the test suite, run by the pre-push hook, commit fixture trees onto the
  developer's real branch).
- Edition 2024, MSRV 1.88 (`[workspace.package] rust-version`). The pinned
  nightly is the *extractor's* build toolchain only.
- `SPIKE-rustc-fidelity-tree.md` is the pivot spike's historical design
  record — the `SPIKE §…` references in `extractor/` comments point there.
  It describes the exploration, not the current tree.

# Approach

1. This software is not released: no backwards-compatibility is needed.
2. Prioritise architectural soundness over effort. Spend all the effort needed to get to a clean tech-debt free situation.
3. Use a object-oriented Rust style.
