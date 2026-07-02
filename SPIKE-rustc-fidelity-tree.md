# Pivot: workspace-lint → a rustc-fidelity engine (Dylint driver)

**Status:** committed direction · step-0 + step-1 plumbing spikes verified (raw driver → Dylint `LateLintPass` → single-bin embed, all byte-identical) · app-layer pre-implementation · **Date:** 2026-07-01 · **Owner:** Henrik

`workspace-lint` is pivoting from the shallow, syn-based resolver to a
**compiler-correct** workspace tree built by a real `rustc` driver, harnessed by
Dylint. This is a **full pivot with a broader re-architecture**, not an additive
mode. This doc records the decision, the architecture, and the open design
questions the implementation must resolve.

It supersedes the positioning in `crates/syn-workspace/DESIGN-ir-pipeline.md` and
the "no rust-analyzer required / sub-second" thesis in `CLAUDE.md` / `README.md`
— those describe the engine being retired and must be rewritten as part of this
work.

## Decisions locked (2026-07-01)

1. **Must-compile is accepted.** The tool analyzes only workspaces that compile.
   Linting non-compiling / mid-refactor code is **out of scope** — no syn
   fallback is retained for it.
2. **Broader re-architecture.** The app layer (pipeline, config surface, output,
   IDE integration) is open for redesign alongside the engine, not just an
   engine swap under the existing shell.
3. **`syn-workspace` is retired.** The published crate (v0.5.0) is
   deprecated/yanked; the fast no-compile resolver is abandoned. Its *data model*
   (`Workspace`/`Module`/`Item`/`Occurrence`) may be salvaged as the driver-
   populated IR (§6), but the resolver, plugins, and SCIP-oracle machinery go.

## 0. Goal / non-goals

**Goal.** A single, compiler-correct workspace IR — resolved by real rustc, not
approximated — covering everything the shallow resolver got wrong:

- `#[cfg(feature = …)]` / cfg-gated code (per a chosen configuration)
- `include!` + `build.rs`/`OUT_DIR` generated code
- declarative (`macro_rules!`) **and** procedural (derive/attr/fn-like) macros
- type resolution — the use-site reference graph through methods, associated
  functions, trait dispatch, inferred types, cross-crate
- visibility resolution, including `pub(in path)`

with **byte-precise spans** preserved so `--fix` still works.

The tool *is* a Dylint lint host: the IR extractor is itself a Dylint lint, and
standard Dylint lints run in the same pass against the same `TyCtxt` (§8).

**Non-goals.**

- A syn fallback of any kind. Must-compile is a permanent property (decision 1).
- A single canonical tree across *all* feature/target configs — "correct" is
  per configuration; we union a declared matrix (§1, §7).
- A *stable public lint-authoring API on `rustc_private`*. Standard Dylint lints
  run (§8), but on Dylint's own unstable contract; our stable authoring surface
  is the Phase-2 IR, not compiler internals.

## 1. The permanent constraint

"Correct" is only well-defined relative to a pinned
`(feature set × target triple × toolchain version)`. `#[cfg]` means the item
set, impls, and reference graph differ per configuration; rustc resolves exactly
one. Across configs there is *no single tree* — it's the feature powerset. We
sample a **matrix** and **union** results (§7); a stronger claim hand-waves the
powerset.

And the engine **requires a compiling workspace** — it runs build scripts and
proc-macros (arbitrary code, network, non-hermetic, seconds to minutes) and is
**undefined for code that doesn't compile**. Per decision 1 this is accepted, not
mitigated. Two direct consequences to design around:

- Every run is a full build — including for lints that need no semantics
  (`file-size`, `crate-size`, `freshness`, `centralized-deps`, config audit).
  The re-architecture must decide whether those still ride the build or get a
  build-free path (§9, §12).
- The `rust-analyzer check.overrideCommand` integration (a fast, side-effect-free
  `cargo check` replacement) **no longer holds** — a Dylint run compiles and
  executes build.rs/proc-macros. The IDE story needs rethinking (§12).

## 2. Why a rustc driver (and why Dylint harnesses it)

The tree needs five layers; no single source covers them, and they split along
one seam — **syntactic tree + byte spans** vs. **reference graph** vs.
**manifest**.

| Source | Structure / visibility | Reference graph | Byte spans | Manifest | Cost |
|---|---|---|---|---|---|
| syn (retired) | good, approx. `pub(in)` | good | **yes** | **yes** | sub-second, no compile |
| rustdoc JSON | **best** | **absent** (`has_body` bit) | no | no | full compile, unstable format |
| SCIP | weak (no visibility) | **strong** | no (symbol ranges) | no | full compile |
| `-Zunpretty=expanded` | via reparse | strong (bodies) | **collapsed** | no | full compile |
| rustc artifacts (`.rmeta`/MIR/HIR) | complete | complete | complete | no | **unreadable** without `rustc_private` |
| RA as a library | strong | strong | **yes** | no | full compile, unstable API, **≈99% fidelity + silent degradation** |
| **rustc driver (Dylint)** | complete, faithful | complete | **yes** (`SourceMap`) | no | full compile, per-crate, treadmill |

The decisive comparison was **RA-as-lib vs. a rustc driver.** RA holds the whole
workspace in one `RootDatabase` (single-phase, ergonomic) but runs its *own*
inference — ≈99% agreement with rustc and **silent degradation** on the rest
(unresolved proc-macros, trait-solving divergence). For a fix-applying linter the
worst failure is a *silent* gap that becomes an invisible false positive. **A
rustc driver is rustc-faithful (no divergence axis) and fails loudly** (a compile
error), and yields byte-precise spans via `SourceMap`. We take fidelity + loud
failure over RA's ergonomics, and accept the per-crate model (§4–5) and treadmill
(§9) as the price.

**Dylint** is the harness: manages the `rust-toolchain` pin, builds the driver
dylib, integrates with cargo (inheriting cfg/features/build.rs/`OUT_DIR`), and
exposes `clippy_utils`. We consume it as a **library** (`dylint_linting`,
`dylint_testing`, `clippy_utils`), not distribute *as* a Dylint lint library.

## 3. What is still outside the engine

rustc has no idea `Cargo.toml` exists. The **manifest/config layer** stays a
`cargo metadata` + `toml_edit` concern: feature *tables*, dep sections,
`workspace = true`, publish flags, byte-located dep lines (for `--fix`), and our
own config in `[workspace.metadata.workspace-lint]`. It feeds `centralized-deps`,
`feature-drift`, config loading, and the declared-deps half of `unused-deps`.

Everything else (pipeline, renderers, suppression, `--fix`, CLI, output, IDE
integration) is **in scope for the re-architecture** (decision 2) — reused where
it still fits, redesigned where the must-compile world changes the shape.

**Packaging — a single binary.** `workspace-lint` is one stable binary that
**embeds the `dylint` orchestrator library** and calls `dylint::run(opts)`
directly — not a shell-out to `cargo-dylint` (whose `main.rs` is itself just a
thin clap wrapper over that call, with no `rustc_private`/nightly deps). The
nightly `rustc_private` work stays quarantined in (a) the **driver** `dylint`
builds and **spawns as a subprocess**, and (b) the separately-built lint dylib —
so linking `dylint` keeps our binary on stable. "Single bin" means one
user-facing binary, **not** zero-toolchain: the nightly toolchain + `rustc-dev`
must still be present at runtime (§9).

## 4. Architecture: two phases

The tree is global; `TyCtxt` is per-crate. That mismatch forces two phases (§5).

```
Phase 1 — per crate, inside the build, has TyCtxt
  ├─ EXTRACT resolved facts → per-crate IR fragment  (defs, refs, visibility,
  │                                                    byte-spans, macro refs)
  └─ RUN per-crate lints     → per-crate findings      (built-in per-crate +
       (both consume the SAME TyCtxt walk)              optional user custom)

        │  serialized artifacts, canonicalized (DefId → ResolvedPath)
        ▼
Phase 2 — main workspace-lint process, no TyCtxt, has the assembled IR
  ├─ IR fragments → assemble workspace tree + cross-crate reverse indexes
  ├─ findings     → diagnostic stream
  ├─ RUN cross-crate / workspace lints against the IR → more findings
  └─ merge all findings → suppression → levels → render / --fix
```

**Two output channels, strictly separated.** Phase 1 emits *IR facts* (→ tree)
**and** *findings* (diagnostics, never in the tree). Don't let lint output flow
into the IR — that conflates model (input) with diagnostics (output). Serialize
`IrFragment` and `Finding` as two distinct streams from day one.

**One pass, two consumers.** Extraction and per-crate lints share the same
`TyCtxt` visit — the resolver walk hands over the IR, the lints hand over
findings; it is not "lint, then extract."

## 5. Why the tree can't be built in Phase 1

rustc's execution model, not a preference. `cargo` invokes the driver **once per
crate compilation, as separate (often parallel) processes**, each with `TyCtxt`
for one crate plus deps' `.rmeta` (signatures, not bodies). Therefore:

1. **Cross-crate data isn't present.** A knows what A references, nothing about
   B/C/D. Reverse indexes (`unused-pub`, architecture) are unions over every
   crate's forward refs — uncomputable in one per-crate process.
2. **Process isolation forces serialize-and-merge** — the merge *is* Phase 2.
3. **A completion barrier is inherent** — the tree isn't done until the last
   crate compiles; that barrier is the phase boundary.
4. **DefIds/spans/symbols are per-session** — cross-crate stitching needs stable
   keys (`ResolvedPath`), i.e. a projection at emit time.

Phase 1 builds each crate's **subtree**; Phase 2 stitches and derives the
cross-crate indexes. Only an engine holding all crates in one process (RA) could
collapse to one phase — which reopens the §2 fidelity decision. Upside: Phase 1
runs concurrently with the build; Phase 2 is plain-data (off `rustc_private`).

## 6. The IR / serialization schema

Phase 2 has **no `TyCtxt`** — it sees only what Phase 1 serialized. So the emit
schema is a hard contract and is simultaneously the **internal IR** every lint
reads and the **public extension surface** (§8). Over-invest in it.

Start from the retired `syn-workspace` data model (`Workspace`/`Module`/`Item`/
`Occurrence`, already `Send + Sync`, already the shape lints consume) — salvaging
the *types* even as the resolver is dropped keeps the lint migration familiar.

Emit-time rules:

- **Canonicalize** every `DefId`/path to a stable `ResolvedPath` — raw rustc
  handles don't cross the process boundary.
- **Byte spans** from `SourceMap` (`Span → BytePos`), including the `pub`-token
  range. Items inside macro expansions map to `Span::source_callsite` and are
  reference-only (no editable span; findings there dropped, as for generated
  code today).
- **Two streams:** `IrFragment` (per crate) + `Finding` (per crate).

**Built so far (step-0/1/2 + cross-crate assembly, `spike/ir/`):** `IrFragment {
crate_name, items: Vec<ItemFact>, references: Vec<RefEdge> }`. `ItemFact` carries
path, **`key`** (stable `DefPathHash`, hex), kind, `parent_kind` (parent `DefKind`
→ module-level vs assoc vs fn-local), **`trait_item`** (the trait item a trait-impl
assoc item implements — `Some` only for trait impls, so `parent_kind==impl` +
`trait_item==None` is an *inherent* impl item), visibility, byte span.
`RefEdge { from, to,
from_key, to_key, to_kind, external, import }` is the resolved reference graph —
name-resolved paths, method calls, type-relative value paths
(`Type::assoc_fn`/`Type::CONST`), **and** type-position assoc projections
(`<T as Trait>::Item`, from a lowered-signature `fn_sig`/`type_of` pass), deduped +
sorted, attributed to the nearest enclosing item; verified **structurally**
byte-identical across the raw driver and the Dylint dylib (4 236 edges on
`syn-workspace`). Not yet emitted: bound/`where`-clause-only projections, opaque
targets, macro provenance, cfg.

**Cross-crate assembly (step-4, `spike/assemble/`):** the whole workspace runs
through one `dylint::run` (no `-p`; cargo fans out → one fragment per crate), and
`wl-assemble` unions the fragments into a def index + reverse index. The join key
is **`DefPathHash`, not the rendered path**: `def_path_str` renders a def at its
definition path in the defining crate but at its re-export path in a consumer, so
path-equality joins score **0/215** cross-crate on this workspace; the hash lands
**199/215**. This is the §5.4 "stable keys" requirement, discovered the hard way.
`DefPathHash` embeds the rustc version (via `StableCrateId`), so it's stable across
observers on *one* toolchain — exactly the two-phase invariant (one pinned driver
per run). Consequence for lints: **cross-crate reverse-index queries (`unused-pub`,
`unused-deps`, architecture) key on `ItemFact::key`, never on the display path.**

The unused-pub verdict then needs corrections to be real, all rustc-emitted (no
text heuristic): candidates = **module-level + inherent-impl** pub items, judged by
direct-call edges; **trait-impl** items excluded (dispatch-reachable, trait-
controlled — via `trait_item`, as rustc dead_code does); **`use`/re-export edges
discounted** (`RefEdge::import`, a `visit_use` override — a use-path's enclosing
item is the module, indistinguishable after the fact). With these the workspace
yields **20 leads** — `builtin_assertions` (module-level) + 19 inherent methods
(e.g. `Crate::declared_deps`, distinguished by stable key from the identically-named,
*used* `Manifest::declared_deps`). Every one is a *lead* not a kill: single-config
IR over-reports vs a cfg-matrix union (§7 — several are `--test`-used), and these are
published-crate API. The `trait_item`→impls linkage is emitted as the substrate for
the remaining gap (full trait-dispatch reachability, incl. external-trait roots);
that + the cfg-matrix union are what's left before the verdict is production-correct.

Consequence: a lint needing a *new* rustc fact requires extending the Phase-1
emit (our code) and re-running the build. Phase-2 lints are strong on
structural/reference/visibility queries but **bounded by the emit vocabulary** on
type-level ones — the same constraint the retired tool had (lints saw the model,
not raw syn).

## 7. Cargo / feature handling

- `cargo metadata` (per feature set) → members, targets, feature table, dep
  graph, publish, byte-located dep lines.
- **One run = one cfg — the flags are load-bearing.** cfg-stripping happens in the
  compiler frontend *before* the driver's `after_analysis`/`LateLintPass` sees
  `TyCtxt`; inactive-cfg items are already deleted, not marked. So a run reflects
  exactly the `--cfg` cargo invoked it with — there is no way to enumerate configs
  from one run. To cover a feature set, cargo must actually compile under it
  (`--features X` / `--all-features` / `--tests` in `dylint::run`'s `Check.args`);
  the driver reads whatever that compile produced. This is *why* the current IR is
  "one (non-test) config" and why `builtin_assertions` reads dead (its `#[cfg(test)]`
  callers were stripped pre-extraction). The `+test` filename keying
  (`sess.opts.test`) already proves the multi-run mechanism.
- **Matrix + union — implemented (2026-07-02).** Run per config in a declared
  matrix (`default`, `--tests`, `--all-features`, named combos); union item/
  reference sets — a def *exists* if present under ≥1 config, is *used* (reached)
  if referenced under any. Report which configs ran — silence reads as "all."
  `wl-assemble <dir>..` takes one IR dir per config (`embed … -- --tests` forwards
  the cfg selector into `Check.args`); the **first dir is the primary config** and
  defines the member-crate set, so a `--tests` config's integration-test crates
  contribute *usage* but never *candidates*. Measured on this workspace over
  `default` + `--tests`: **20 default-alone leads → 9 after the union, 11 retired**
  by test-only usage (`builtin_assertions`, `Manifest::empty`, `member_by_name`,
  `Workspace::load`, …). Two implementation notes: (a) synthetic `--test`-harness
  `main`s carry no source span (`ItemFact::span == None`) and are filtered from
  candidates — otherwise each test binary's generated `main` reads as dead pub API;
  (b) trait-impl items are judged, not excluded (see below).
- **The union needs a DIFFERENT identity key than the within-config join —
  verified 2026-07-02.** `DefPathHash` (`ItemFact::key`) is **not** stable across
  configs: default vs `--test` on `syn-workspace` (same toolchain) → **0/475 keys
  survive**. Cause: the top 64 bits are the `StableCrateId` (one value per crate,
  `dad999a7…` default vs `12b2f2ed…` test) which hashes cargo's `-Cmetadata` — and
  cargo folds the feature/test selection into it; the bottom 64 (local def-path
  hash) is `StableCrateId`-seeded so it moves too. **This is the dual of the
  cross-crate problem** (§6): `DefPathHash` is stable across *observers* (crates)
  but not *configs/toolchains*; the crate-qualified **definition path**
  (`(crate_name, def_path_str)`) is the reverse — stable across configs/toolchains
  (verified: all 475 items matched by path across the two configs, 475 unique, incl.
  229 impl items whose hash moved but path held) but *not* across crates (visible-
  parent-map renders a foreign def at its re-export path). Neither key does both
  axes. So the assembler is **two-level**: (1) within each config, join cross-crate
  on `DefPathHash` and normalize every edge endpoint to its `ItemFact`'s
  `(crate, definition-path)`; (2) union across configs on `(crate, definition-path)`.
  Residual risk at step 2: two `impl SelfType` blocks render identically via
  `def_path_str` — use the semantic key (Self-type + trait + item name) for impl
  items if collisions appear. **Verified 0 same-`def_path_str` collisions among
  candidates within a config on this workspace** (2026-07-02), so the plain path
  identity is safe here; the semantic-key fallback stays a documented contingency.
- **Trait-dispatch reachability — implemented (2026-07-02).** The union's per-config
  "reached?" primitive (`Assembly::reach_of`) judges trait-impl items instead of
  excluding them: a trait-impl item is reached if it has a direct use-site, **or its
  implemented trait is external** (std/serde/clap — a *sound root*, because external
  code dispatches `Display::fmt`/`Deserialize::deserialize` invisibly and it can
  never be proven dead), **or its (workspace-internal) trait method is dispatched**
  anywhere (`in_degree` of the `trait_item` key > 0, via `<T: Tr>` / `dyn Tr` calls
  that resolve to the trait-decl method). Module-level and inherent-impl items carry
  no `trait_item`, so they fall through to direct-use-or-unreached — the pre-4a
  judgment, now unified. On this workspace: **464 trait-impl items proven immune via
  external dispatch, 0 internal-dispatch** (every workspace-internal trait — `Lint`,
  `ResolverPlugin` — is `pub(crate)`, so its impls aren't *pub* candidates; the
  internal-dispatch branch is correct but latent). This is the "external-trait roots"
  the reachability gap named.
- **Publish/root metadata — implemented (2026-07-02, SPIKE §7 step 5).** `wl-assemble
  --ws <root>` reads `cargo metadata` (`publish` + target kind, no compile) and
  classifies each union survivor by whether its crate's pub API is an **external
  reachability boundary** — a *publishable library* (`publish != false` **and** has a
  lib target). Two buckets: **DEAD** (unused in every cfg *and* a bin / non-published
  crate — nothing can reach it, a hard verdict) vs **PUBLISHED API SURFACE** (a
  published lib's pub API — external consumers possible, review for over-exposure not
  death). Union result on this workspace: **0 dead + 9 API-surface** (all
  `syn_workspace`); the single default config surfaces **1 dead**
  (`workspace_lint::DiagnosticBuilder::level` — bin crate, retired by the union since
  `--tests` uses it). This **replaces the `referenced` dependency-leaf proxy** the
  earlier steps used as a stand-in: the proxy is right for crates that *are*
  referenced but mislabels a published **leaf** library (the `*-marker` crates —
  published libs no workspace crate references) as "dead"; metadata gets them right
  (verified: proxy tags both markers "verdict", metadata tags them "API surface").
  Bin `main`s need no special root handling — they're `pub(crate)`, already filtered
  by visibility. The roots come from cargo metadata as **plain data** into Phase 2,
  exactly as production `main.rs` would pass them; the assembler stays `rustc_private`
  -free. **The unused-pub track is now feature-complete for the spike** (reference
  graph → cross-crate join → import discounting → inherent/trait split → trait-
  dispatch reachability → cfg-matrix union → publish roots).

## 8. Extensibility & Dylint lints

The engine *is* a Dylint lint host, so hosting lints is a first-class capability,
not a bolt-on. Three tiers, by scope:

- **The IR extractor is itself a Dylint lint.** Phase-1 extraction is a
  `LateLintPass` in our Dylint library; it builds the tree from the same
  `TyCtxt` every lint sees.
- **Standard Dylint lints run in the same pass — supported (proven 2026-07-02).**
  Any Dylint lint (clippy-style, third-party, or a user's own) loaded via
  `[workspace.metadata.dylint.libraries]` runs in the same per-crate compilation
  against the same `TyCtxt`, and its findings flow into our pipeline (collect to
  our sink, §12.6). Demonstrated: `wl-lint` now registers **two** lints in one
  dylib — the silent `WL_IR_EXTRACT` extractor (facts) and an *emitting*
  `WL_UNDOCUMENTED_PUB` findings pass — via a **hand-written `register_lints`** (the
  `declare_late_lint!` macro wires only one; the dylint_linting docs prescribe
  manual registration for multi-lint libs). Both passes run in one compilation;
  `extract()` is untouched so the facts channel stays byte-identical to the raw
  driver. **Findings channel proven end-to-end:** emission is rustc-native
  `LintContext::emit_span_lint(lint, span, decorator)` with the built-in
  `rustc_errors::DiagDecorator(|diag| …)` closure adapter — **no `clippy_utils`**
  (the closure `span_lint` is gone on this nightly; `DiagDecorator` sidesteps
  `#[derive(LintDiagnostic)]` and the clippy_utils treadmill). Capture is
  `--message-format=json` through `dylint::run`'s `Check.args`: cargo's
  `compiler-message` stream carries the inner rustc `Diagnostic` (`level`,
  `code.code` = lint name, `spans[]` with `byte_start`/`byte_end` + line/col +
  `suggested_replacement`, `children`, `rendered`) — the exact `DiagnosticSpan` JSON
  the `workspace-lint` renderers and rust-analyzer already consume. This is exactly
  how `cargo dylint` already composes libraries — we inherit it.
- **Cross-crate lints against the assembled IR — first-class, stable.** A lint
  needing *whole-workspace* context is a Phase-2 lint over the plain-data IR,
  touching **zero `rustc_private`** (no pin, no `clippy_utils` lockstep, no
  treadmill). Cross-crate rules belong here — they can't run in Phase 1 (no IR
  yet).

Two boundaries to keep straight — physics, not policy:

- **Findings ≠ IR facts.** The tree is built by the *extraction* pass from the
  shared `TyCtxt`; a standard lint's *findings* are diagnostics, not tree data.
  "Reuse the data the lints run on" means the shared **input** (`TyCtxt`), not a
  lint's **output**.
- **Per-crate ≠ cross-crate.** A standard Dylint lint is per-crate: it can run
  and report, but can't answer cross-crate questions on its own — those come
  from Phase-2 assembly.

The one caution: we don't promise a *stable public lint-authoring API on
`rustc_private`*. Standard Dylint lints lean on **Dylint's** (unstable) contract,
not one we invent; our stable authoring surface is the Phase-2 IR.

## 9. Honest risks / costs

- **Toolchain treadmill — now load-bearing with no fallback.** The driver links
  `rustc_private`; each build pins a nightly + matching `clippy_utils` (version
  tracks rustc, e.g. `0.1.98 ↔ 1.98`). Periodically advance the pin, bump
  `clippy_utils`, fix internal-API breakage, re-run tests. **If a bump breaks the
  driver and can't be fixed same-day, the whole tool is down** — syn no longer
  catches us. This is the single biggest operational risk of the pivot.
- **Always a full build.** Seconds–minutes per run, including cheap lints (§1).
- **No fast IDE integration.** The `check.overrideCommand` story is gone; needs a
  replacement or an explicit drop (§12).
- **Emit-schema bottleneck.** Phase-2 lints see only what Phase 1 emits (§6).
- **Feature combinatorics.** "Correct" is per-config; we sample, not enumerate.
- **Migration blast radius.** Full pivot + re-architecture + crate retirement is
  a large change with no incremental fallback; sequencing (§11) must keep the
  test corpus meaningful throughout.
- **Install surface — "single bin" ≠ zero toolchain.** The binary embeds the
  `dylint` orchestrator (§3), but at runtime it still needs the nightly toolchain
  + `rustc-dev`, builds/spawns the driver subprocess, and may need `dylint-link`
  (which `dylint` can manage). Today only `--fix` needs an external toolchain
  (rust-analyzer); now every run does. Needs a preflight/bootstrap that ensures
  the toolchain is present.
- **`dylint` library API coupling.** Embedding `dylint` (`opts::Dylint`, `run`)
  tracks a 0.x API. But this is an ordinary **stable-toolchain** semver chore —
  distinct from, and far milder than, the `rustc_private` treadmill above; it
  buys typed invocation over fragile CLI flags.
- **Avoided by this choice:** RA's silent divergence — the driver fails loudly.

## 10. Validation

The existing syn-vs-RA differential oracle (`tests/scip_diff.rs`,
`tests/oracle.rs`, `scip_emit.rs`) is being **retired with syn-workspace**, so it
can't be the permanent spine. Two roles:

- **Transitional (during migration):** diff the new driver IR against the
  *current* syn behavior on the existing `tests/cases` corpus to catch
  regressions while porting lints — then drop it. **Built (step-1 spike,
  `spike/fidelity/`).** On `syn-workspace` itself, at the granularity syn supports
  (module-level named defs), **config-matched: recall 100 %, precision 100 %, F1
  100 %, visibility agreement 100 %** — with `syn-only = 0`, `rustc-only = 0`, and
  no syn-side heuristic. How the rigor was reached, and what it found:
  - **Config-matched, not heuristic-filtered.** In the *default* config syn looks
    only 47 % precise, purely because it's **cfg-blind**: 227/429 of its comparable
    defs are `#[cfg(test)]` code a single-config lib build omits. The fix is to
    compile the ground truth in the *matching* config (a §7 "correct is per-config"
    instance), driven via `--tests`. Two rustc-side subtleties, both handled:
      - `--cfg test` *alone* strips `#[test]` fns (the built-in attr expands them
        away without `--test` mode); only `--test` keeps them. So we run `--test`.
      - `--test` then injects harness synthetics — a generated `main` and one
        `TestDescAndFn` **const per `#[test]` fn, shadowing it at the same path**.
        The oracle strips these structurally (no source span; or a `const`
        shadowing a `fn`): 189/189 removed on this crate, `syn-only` → 0.
    The `+test` crate variant coexists with the plain-lib build (which compiles as
    the integration tests' dependency), so the extractor keys output on
    `sess.opts.test` to avoid a one-filename race.
  - **Exclusions are classified from the rustc-emitted parent `DefKind`**
    (`ItemFact.parent_kind`: `mod` ⇒ module-level; `impl`/`trait` ⇒ associated;
    a fn/const/static/closure body ⇒ fn-local), *not* a snake_case-path heuristic.
    That's strictly more accurate: it moved 2 statics defined inside assoc-fns out
    of the assoc bucket into fn-local, and reclassified the 2 former "recall misses"
    (function-local `const`s in free fns) as the fn-local defs they structurally
    are — so recall on the representable set is a clean 100 %, not 99.5 %.
  - **The structural gaps the pivot closes are large and clean:** rustc emits **236
    associated items** (impl/trait methods, derive impls like `<T as Debug>::fmt`)
    and **4 fn-local defs** syn's model can't represent at all — together ~⅓ of the
    crate's defs. syn descends into neither impl/trait bodies nor fn bodies.
  - **Visibility model is sound:** where both see an item they agree **100 %**.
- **Permanent (open, §12):** a new correctness spine. Candidates — a curated
  golden-IR fixture corpus asserted against the driver, and/or spot-diffs vs. RA
  as an independent second opinion. Must be decided before syn is deleted.

## 11. Component shape & rough sequence

Components:

- `cargo metadata` + manifest parse — **reused**.
- **Driver crate** (`cdylib`, `dylint_library!`) — walks `TyCtxt` in
  `after_analysis`, emits `IrFragment` + `Finding` per crate. Pins its own
  `rust-toolchain`; depends on `clippy_utils`.
- **Orchestrator = the `workspace-lint` binary itself** (stable) — embeds the
  `dylint` library and calls `dylint::run(opts)` **per feature-config** (one call
  per config; cargo fans out over crates internally — we never loop crates
  ourselves). `dylint` builds/loads the lint dylibs and builds/spawns the nightly
  `rustc_private` driver as a subprocess. The master mints a per-run `WL_IR_OUT`
  dir, then gathers `IrFragment`s from it + `Finding`s from the driver's
  diagnostics and hands off to the assembler. One user-facing binary; no separate
  `cargo-dylint` install.
- **Assembler** — fragments → workspace tree + reverse indexes (the driver-backed
  replacement for `Workspace::load`).
- **App layer** — re-architected pipeline/config/output over the assembled IR.
- **`syn-workspace`** — deprecated and removed once lints are ported.

Rough sequence (each step keeps the corpus green where possible):

1. Salvage the IR data model into a new engine-agnostic crate; define the lint
   trait over it (§12.5).
2. Stand up the driver: emit a minimal `IrFragment` (defs + visibility + spans)
   for one crate; assemble; diff vs. syn on a fixture.
3. Grow the emit to full fidelity (references, macros, cfg); port lints one by
   one onto the new IR, transitional-diffing each.
4. Re-architect the app layer for the must-compile world (config, output, IDE).
5. Establish the permanent correctness spine; delete syn-workspace + oracle.
6. Rewrite `CLAUDE.md` / `README.md` thesis.

### Verified by the step-1 spike (2026-07-01)

Step 2 of the sequence is de-risked end-to-end (`spike/wl-lint` + `spike/embed`):

- **Repackaging is faithful.** The raw driver's `extract()` lifted into a Dylint
  `LateLintPass::check_crate` **verbatim** (`cx.tcx` is the same `TyCtxt`) and
  produced **byte-identical IR** to the raw driver on `syn-workspace`
  (165 280 bytes, 475 items, 237 pub). Confirms §2's "Dylint harnesses the same
  driver" thesis.
- **Single-bin embed works** (→ §12.10 resolved). A **stable** binary drives the
  **nightly** lint dylib via `dylint::run(opts)`; nightly/`rustc_private` stay
  quarantined in the spawned driver + the dylib.
- **The extractor pass must be `Warn`+, never `Allow`.** rustc does not schedule
  a `LateLintPass` whose lints are all `Allow` — an `Allow` extractor's
  `check_crate` silently never runs (cost me a real debugging loop). It stays
  quiet by never calling `span_lint`; `Warn` is only the run-switch. *Design
  consequence:* the "always-on" IR harvest rides the lint-level machinery as a
  silent `Warn` pass — or, since the single bin controls its own driver spawn, it
  could hook `after_analysis` directly (level-independent) and reserve
  `LateLintPass` for genuine findings. Decide during app re-architecture.
- **`clippy_utils` is for the *findings* channel, not extraction.** The pure IR
  extractor needs only raw `rustc_middle`/`rustc_hir` — we dropped `clippy_utils`
  and sidestepped its nightly-lockstep. But diagnostic *emission* on this nightly
  is the struct-based `emit_span_lint(lint, span, impl LintDiagnostic)`; the
  ergonomic closure helpers are `clippy_utils::diagnostics::span_lint_*`. So
  findings-lints will likely still want `clippy_utils` (and its lockstep) even
  though the extractor does not. Refines §2/§9/§11's "depends on `clippy_utils`".
- **Adopting dylint's pinned nightly tames the treadmill.** dylint 6.0.1 pins
  `nightly-2026-04-16` + `clippy_utils @ f6d31069`. Building the `LateLintPass`
  against *that* pin took **one** fix (a duplicate `extern crate rustc_lint` the
  `declare_late_lint!` macro injects) vs. the raw driver's **four**
  `rustc_private` breakages — the four were drift from the rolling nightly, not
  dylint. Data point for §12.4: track *dylint's* pin, not the newest nightly.

### Verified by the step-2/3 spike (2026-07-02)

Steps 3 (second lint) and the front half of 4/5 (migration readiness) advanced;
the fidelity oracle was re-run to re-baseline.

- **Fidelity re-baselined — no regression.** Re-ran `wl-fidelity` on freshly
  extracted IR (default + `--tests`). Config-matched on `syn-workspace`:
  **recall 100 %, precision 100 %, F1 100 %, visibility 100 %, `syn-only` = 0,
  `rustc-only` = 0** — unchanged since the extractor grew (`trait_item`, synthetic
  filter, signature projections). The number the pivot rests on: syn is a *perfect
  subset at its own granularity*, blind to **236 associated items + 4 fn-local
  defs** (~⅓ of the crate) and the entire **4 236-edge** reference graph.
- **A second lint rides the same IR (`unused-deps`) — §8 breadth made concrete.**
  The assembler now emits a real `unused-deps` verdict off the *same fragments* the
  unused-pub verdict loads: declared deps (`cargo metadata`) diffed against the
  reference graph, unioned across configs exactly like unused-pub. No new
  extraction; a second query on the assembled model. Confirms lints compose over
  Phase-2 without re-walking `TyCtxt`.
  - **Facade-crate finding (and fix).** The naive "edge target's crate == declared
    dep" match has a false-positive class: a **facade crate** (`clap`) re-exports
    everything from an impl crate (`clap_builder`), so every `use clap::Parser`
    edge resolves to `clap_builder` — `clap` reads unused. 15/16 normal deps
    matched by direct name; only the pure facade failed. **Fixed soundly** by
    crediting a dep when the referenced-crate set meets its resolved dependency
    **closure** (`clap_builder ∈ closure(clap)`), read from `cargo metadata`'s
    resolve graph. Over-approximate on purpose — it can only *miss* a truly-unused
    dep (false negative), never flag a used one, the safe direction for a "delete
    it" lint. This is the **same re-export asymmetry** §6 documents for the
    cross-crate join, resurfacing on the dependency axis.
  - **Sound judgement scope, stated in-band.** Normal deps always judged; dev-deps
    only when a test target was compiled (a `--tests` config); build-deps never
    (`build.rs` isn't lint-passed); optional deps never (feature-gated). Verified
    the default-only config correctly does *not* flag a dev-dep (its target wasn't
    compiled → not a false positive). Residual finding on this repo: `cargo-husky`
    (a side-effect/build-hook dev-dep, zero code refs by design) — a *true*
    ref-graph absence the output caveats, not a bug.
- **Migration readiness measured (not guessed).** The real `workspace-lint` couples
  to `syn-workspace` across **17 files**, with a broad query surface — `Workspace`
  / `Crate` / `Item` / `ItemKind` / `ResolvedPath` / `Visibility` /
  `manifest::DepSection`, re-exported `toml_edit`, and `walk_items` / `references` /
  `re_exports` / `declared_deps` / `exposed_in_public_signature` /
  `resolved_publish` / `load_with_options`. **Consequence:** `syn-workspace` is not
  *only* a semantic resolver — it also bundles **manifest/TOML parsing and path
  utilities** the lints use. A clean migration must split those: *semantic
  resolution* → replaced by the assembled IR; *manifest/toml/path utils* → kept or
  relocated to a small non-nightly crate. This sizes step-3/5 honestly: a
  multi-PR backend swap, not a drop-in.
- **Migration shape decided — backend swap, not "lints as Dylint passes."** Two
  options were weighed. **(A) IR as the `syn-workspace` replacement:** the nightly
  dylib does *only* silent extraction; the stable binary assembles and the existing
  semantic lints query the assembled IR instead of `Workspace`; suppression,
  `expect!`/`allow!`, `--fix`, and the human/json/github renderers all stay. **(B)
  lints as Dylint `LateLintPass`es** emitting via `emit_span_lint`: discards the
  mature pipeline. **A wins** — it reuses everything and swaps only the data source.
  The step-3 *findings channel* (proven: native `emit_span_lint` + `--message-format=json`
  capture) is therefore a **fallback / for genuinely HIR-shaped lints**, not the
  default production path.
- **Caching gotcha — RESOLVED (2026-07-02), design chosen and verified.**
  Symptom: `WL_IR_OUT` is **not** in cargo's fingerprint, so changing it alone never
  re-runs the extract pass — cargo reports the crate up-to-date, **replays cached
  compiler stderr** (a "wrote IR" line prints from a *prior* invocation's env, even
  pointing at a stale dir), and the `LateLintPass` never executes. My original bug
  was **redirecting the output dir** per run, so a warm-cache crate's IR never landed
  in the new dir.

  Rather than fight cargo, I characterized its actual behavior end-to-end on
  `syn-workspace` via `spike/embed` (each row verified, not assumed):
  - **code change → recompile → fresh IR** (pass re-runs, file rewritten);
  - **dylib/schema change → re-lint → fresh IR** — dylint **fingerprints the lint
    dylib** (touching the `.dylib` alone forces a re-lint, 0.02 s → 0.17 s), so a new
    extractor schema auto-re-extracts even against unchanged target code;
  - **no change → cache hit → the prior IR at its canonical path persists and is
    valid**, and the replayed message even reports where it lives;
  - **sole residual failure:** the IR file removed *out-of-band* while dylint's cache
    stays warm — the cache hit won't recreate it.

  **The IR is a deterministic output of compilation, so let cargo own its lifecycle.**
  The three brainstormed options resolve as: (a) force-clean and (b) `RUSTFLAGS`
  nonce are both **rejected** — they discard incrementality by forcing a recompile
  every run; (c) "IR lifecycle = compilation lifecycle" is **right in principle** and
  is realized simply *without* piping IR through the message stream:
  1. write each fragment to a **canonical, stable path** keyed on crate (`+test`
     suffix for the test cfg), **never a per-run dir** — this alone kills the
     original bug;
  2. rely on cargo's fingerprint (code + dylib, both verified) to keep fragments
     fresh-or-valid;
  3. a **completeness guard** for the sole residual failure: the orchestrator knows
     the expected member set from `cargo metadata`; after `dylint::run`, if any
     fragment is missing, force a re-lint — **bump the dylib mtime** (verified to
     force re-lint globally) or surgically clean the unit in **`target/dylint`**
     (dylint keeps its *own* target dir — a plain `cargo clean -p` on the main
     `target/` is a no-op for it, verified). In steady state the guard never fires.

  This is cargo-cooperative, needs **no extractor change**, and keeps incremental
  builds. Resolves the blocker; refines §12.1/§12.6.

## 12. Open questions

1. **New app shape.** What does the pipeline/CLI/output look like when every run
   is a full build? What (if anything) replaces `check.overrideCommand`?
2. **Cheap lints.** Do `file-size`/`freshness`/`centralized-deps`/config-audit
   still ride the build, or get a build-free path? (They need no `TyCtxt`.)
3. **Permanent correctness spine** (§10) — golden fixtures, RA spot-diff, or both?
4. **Toolchain cadence in practice** — bump the pin once; measure breakage +
   `clippy_utils` lockstep effort. Is "hold for months" realistic vs. the Rust
   versions we must analyze?
5. **Engine-agnostic lint trait** — can lints be written once against the IR,
   independent of how it was produced? (Determines how clean the port is.)
6. **Diagnostic capture** — mechanism/cost of routing Phase-1 lint findings into
   the pipeline (custom `DiagCtxt`/emitter vs. emit-to-sink shim).
7. **`--fix` span fidelity** — confirm `SourceMap` gives the exact `pub`-token
   and item byte ranges; verify macro-callsite mapping for generated code.
8. **syn-workspace deprecation** — yank vs. leave with a deprecation notice;
   downstream consumers, if any.
9. **Cost envelope** — wall-clock, single config and a 2–3-config matrix.
10. **`dylint` embed API** — ✅ **RESOLVED (step-1 spike, 2026-07-01).** A stable
    binary (`spike/embed`) embeds `dylint = "6.0.1"` (feature `library_packages`)
    and calls `pub fn dylint::run(opts: &dylint::opts::Dylint) -> anyhow::Result<()>`.
    All four requirements map onto `opts`: load a specific lib →
    `LibrarySelection.lib_paths`; select packages / `--no-deps` →
    `Check.packages` / `Check.no_deps`; pass `cargo check` args (e.g.
    `--features`, `--message-format=json`) → `Check.args`; target workspace → set
    CWD; thread `WL_IR_OUT` → set on this process (the spawned driver inherits it,
    confirmed). The embed produced **byte-identical IR to the `cargo dylint` CLI
    and to the raw driver**. The proto-C fallback is unneeded.

## 13. Decision record (one line)

Full pivot: `workspace-lint` becomes a rustc-faithful, loud-failing,
two-phase (per-crate extract → global assemble) linter built on a Dylint-
harnessed `rustc_driver`, retiring `syn-workspace` and re-architecting the app
for a must-compile world — accepting the toolchain treadmill (now without
fallback) and the loss of broken-code/fast/IDE-check linting, in exchange for
eliminating silent semantic divergence and getting compiler-correct
cfg/macro/type/visibility resolution with byte-precise spans.
