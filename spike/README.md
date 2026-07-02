# spike/ — rustc-fidelity IR (extraction + cross-crate assembly + cfg-matrix union + publish roots + findings channel + unused-deps)

Plumbing spikes for the pivot in `../SPIKE-rustc-fidelity-tree.md`. Proves the
Phase-1 → Phase-2 round-trip three ways, each producing **structurally
byte-identical IR** for `syn-workspace` (475 defs, 237 pub, + a 4 236-edge
reference graph), and now assembles the **whole workspace** (4 crates, 1 355
defs) into a cross-crate reverse index:

- **step 0** — a raw `rustc_driver` wrapper (`driver/`) run as
  `RUSTC_WORKSPACE_WRAPPER`. Proves we can link `rustc_private` and walk `TyCtxt`.
- **step 1** — the *same* `extract()` lifted verbatim into a **Dylint
  `LateLintPass`** (`wl-lint/`), then driven two ways: via the `cargo dylint` CLI,
  and via a **stable single binary** that embeds `dylint::run(opts)` (`embed/`).
  Proves the real target (Dylint lint host + single-bin packaging, SPIKE §3/§8/§11).

**Isolated from the main workspace** (repo-root `Cargo.toml` `exclude = ["spike"]`)
so none of this touches the stable build or the dogfood lint.

## Layout

- `../crates/wl-ir/` — the cross-phase serialization contract (plain serde). §6.
                Graduated to the main workspace in migration PR 1; the spike
                crates keep path-depping on it until they retire.
- `driver/`   — `wl-driver`: raw `rustc_driver` wrapper (step 0). Emits an
                `IrFragment` per primary crate → `$WL_IR_OUT/<crate>.json`.
- `assemble/` — `wl-assemble`: Phase-2 assembler. Reads *all* fragments and
                builds the workspace-global def index + **cross-crate reverse
                index** (keyed by the stable `DefPathHash`, not the display
                path), then reports the dependency matrix and the module-level
                unused-pub verdict — the usage-analysis consumption path.
- `fidelity/` — `wl-fidelity`: the transitional oracle (§10). Diffs the rustc IR
                against the syn resolver's model of the same crate and prints a
                scored fidelity delta (recall/precision/visibility). Depends on the
                being-retired `syn-workspace`.
- `wl-lint/`  — the Dylint `LateLintPass` (step 1). **Isolated workspace**, pins
                `nightly-2026-04-16` (dylint 6.0.1's toolchain). `extract()` is
                lifted verbatim from `driver/`. Drops `clippy_utils` (the
                extractor uses raw `rustc_middle`/`rustc_hir`).
- `embed/`    — `wl-embed`: **stable** binary that calls `dylint::run(opts)`
                directly (no `cargo-dylint` CLI) — the single-bin embed check.
                Now also carries the **completeness guard** (WS5.1): after the run
                it checks every expected fragment exists (from `cargo metadata`),
                and on a miss bumps the dylib mtime + re-lints once — closing the
                §11 caching gotcha in code.
- `probe-check/` — `wl-probe-check`: asserts the WS1 span-fidelity policy over the
                extractor's output for `probes/expansion` (21 assertions).
- `probes/expansion/` — a purpose-built **lint target** (isolated workspace) whose
                cross-file `macro_rules!` is the dangerous `--fix`-surface case.

Hardening scripts (WS1/WS4/WS5.1):

- `roundtrip-suggestion.sh` — captures a rustc-native `span_suggestion` through
  `--message-format=json`, asserts byte-exact `MachineApplicable`, applies it,
  and `cargo check`s the result.
- `kfn-flip-demo.sh` — runs the extractor over the actual `unused-pub` known-false
  fixtures: the impl-method KFN flips to a real finding; the `#[no_mangle]` KFP is
  honestly reported as *not* fixed (needs attribute capture).
- `test-completeness.sh` — the completeness-guard test (delete a fragment
  out-of-band → guard regenerates it with no registry-dep recompile).

The two nightly `[toolchain]` pins differ on purpose: `driver/` used the rolling
nightly it was written against (2026-05-01); `wl-lint/` adopts dylint 6.0.1's pin
(2026-04-16). Adopting dylint's pin is the production choice (see Status). The
`--fix` span surface, the toolchain-bump cost, the cost envelope, real-crate
fidelity breadth, the completeness guard, and a Linux CI smoke are all covered by
the **WS1–5 hardening pass** — see `../SPIKE-rustc-fidelity-tree.md` §12a/§12b.

## Build & run

```sh
REPO="$(git rev-parse --show-toplevel)"
LIB="$REPO/spike/wl-lint/target/debug/libwl_lint@nightly-2026-04-16-aarch64-apple-darwin.dylib"

# --- step 0: raw driver ---
(cd spike && cargo build)                                  # nightly + rustc-dev
WL_IR_OUT="$REPO/spike/ir-out" RUSTC_WORKSPACE_WRAPPER="$PWD/spike/target/debug/wl-driver" \
  cargo +nightly check -p syn-workspace
(cd spike && cargo +nightly run -p wl-assemble -- "$REPO/spike/ir-out")

# --- step 1a: Dylint LateLintPass via the cargo-dylint CLI ---
# (needs: cargo install cargo-dylint dylint-link)
(cd spike/wl-lint && cargo build)
WL_IR_OUT="$REPO/spike/ir-out-dylint" \
  cargo dylint --lib-path "$LIB" --no-deps -- -p syn-workspace

# --- step 1b: single-bin embed (stable binary, no CLI) ---
(cd spike/embed && cargo +stable build)
"$REPO/spike/embed/target/debug/wl-embed" \
  "$REPO" "$LIB" "$REPO/spike/ir-out-embed" syn-workspace

# --- cross-crate: orchestrate the WHOLE workspace (no -p ⇒ all members) ---
# One dylint::run; cargo fans out; the driver emits one fragment per crate.
"$REPO/spike/embed/target/debug/wl-embed" \
  "$REPO" "$LIB" "$REPO/spike/ir-out-workspace"          # 4 fragments
(cd spike && cargo run -q -p wl-assemble -- "$REPO/spike/ir-out-workspace")

# --- cfg-matrix union: one IR dir per cfg, then union across dirs ---
# Args after `--` are the cfg selector, forwarded to cargo check (Check.args).
"$REPO/spike/embed/target/debug/wl-embed" \
  "$REPO" "$LIB" "$REPO/spike/ir-out-matrix/default"            # default cfg
"$REPO/spike/embed/target/debug/wl-embed" \
  "$REPO" "$LIB" "$REPO/spike/ir-out-matrix/tests" -- --tests   # --test cfg
# Many dirs ⇒ union (lead iff unreached in EVERY cfg); one dir ⇒ single verdict.
# --ws <root> reads cargo-metadata publish/target-kind roots (step 5: DEAD vs
# published-API-surface). Omit it to fall back to the dependency-leaf proxy.
(cd spike && cargo run -q -p wl-assemble -- \
  "$REPO/spike/ir-out-matrix/default" "$REPO/spike/ir-out-matrix/tests" --ws "$REPO")

# The per-crate *structure* is byte-identical across all harnesses; the stable
# `key` (DefPathHash) additionally embeds the rustc version, so compare with the
# key columns stripped when the harnesses run different nightlies:
diff spike/ir-out/syn_workspace.json spike/ir-out-dylint/syn_workspace.json
diff spike/ir-out/syn_workspace.json spike/ir-out-embed/syn_workspace.json
```

## Status (2026-07-01)

**Steps 0 + 1 work.** The `LateLintPass` and the embedded `dylint::run` both emit
IR byte-identical to the raw driver. Key findings (folded into
`../SPIKE-rustc-fidelity-tree.md` §11 "Verified by the step-1 spike" and §12.10):

- **Embed API confirmed.** `dylint::run(&opts)` (feature `library_packages`) takes
  lib paths, package selection, `--no-deps`, and `cargo check` args; CWD selects
  the target workspace; `WL_IR_OUT` is inherited by the spawned driver. The stable
  binary drives the nightly dylib — quarantine intact.
- **The extractor pass must be `Warn`+, not `Allow`.** rustc won't schedule a
  `LateLintPass` whose lints are all `Allow`, so the pass silently never runs.
  It stays quiet by never emitting; `Warn` is just the run-switch.
- **`clippy_utils` belongs to the findings channel, not extraction.** Emission on
  this nightly is the struct-based `emit_span_lint`; the closure helpers live in
  `clippy_utils::diagnostics`. The pure extractor needs neither.
- **Treadmill: track dylint's pin, not the newest nightly.** Building the
  `LateLintPass` against dylint 6.0.1's `nightly-2026-04-16` took **one** fix
  (dup `extern crate rustc_lint`) vs. the raw driver's **four** `rustc_private`
  breakages — the four were rolling-nightly drift, not dylint.

## Fidelity (step-1 result)

```sh
(cd spike && cargo build -p wl-fidelity)

# Default-config IR: syn looks 47% precise — but only because it's cfg-blind and
# includes #[cfg(test)] code the single-config lib build omits.
./spike/target/debug/wl-fidelity "$REPO" "$REPO/spike/ir-out-embed/syn_workspace.json"

# Config-matched: build the ground truth with --test (see below), then diff the
# +test variant. cfg(test) inflation disappears at the source.
./spike/target/debug/wl-fidelity "$REPO" "$REPO/spike/ir-out-testmode/syn_workspace+test.json"
```

On `syn-workspace`, at syn's granularity (module-level named defs), **config-matched:
recall 100 %, precision 100 %, F1 100 %, visibility 100 %, `syn-only` = 0, `rustc-only`
= 0** — no syn-side heuristic. Every def syn's model is designed to hold, it holds
exactly, with correct visibility. The structural gaps the pivot closes are the two
classes syn *can't* reach: **236 associated items** (parent `impl`/`trait`) and **4
fn-local defs** (parent a fn body — no fn-body descent) — together ~⅓ of the crate.

Those exclusions are now classified from the **rustc-emitted parent `DefKind`**
(`ItemFact.parent_kind`), not a snake_case-path heuristic. That's strictly more
accurate: it moved 2 statics defined inside assoc-fns (`AmbientScope::empty::EMPTY`,
`IncludeCtx::none::EMPTY_ENV`) out of the assoc bucket into fn-local where they
belong, and reframed the 2 former "recall misses" (`NON_CANDIDATES`, `NEEDLE`,
consts in free fns) as the fn-local defs they structurally are — so recall on the
representable set is a clean 100 %, not 99.5 % with an asterisk.

Producing the config-matched IR (`--test` mode; the extractor keys its output file
on `sess.opts.test`, so the plain-lib and `+test` builds don't race):

```sh
# --test is a target flag, passed through cargo by dylint:
WL_IR_OUT="$REPO/spike/ir-out-testmode" \
  cargo dylint --lib-path "$LIB" --no-deps -- --tests -p syn-workspace
```

The oracle strips `--test` harness synthetics structurally (the generated `main`
has no span; each `#[test]` fn's `TestDescAndFn` descriptor is a `const` shadowing
a `fn` at the same path — 189/189 removed here).

## Reference graph (step-2 result)

`IrFragment` now carries `references: Vec<RefEdge>` — resolved `from → to` edges
(who-uses-whom), the rustc-fidelity answer to syn's text-based occurrence model
and the substrate for the usage lints (`unused-pub`, `unused-deps`, architecture).
On `syn-workspace`: **4 236 deduped edges (1 542 intra-crate, 2 694 cross-crate)**,
and it is **byte-identical across the raw driver and the dylib** — the reference
walk is as deterministic and toolchain-stable as the definition walk.

Extraction attributes each edge to the nearest enclosing item, in two passes:
- an HIR walk (`nested_filter::All`) resolving **name-resolved paths** (`path.res`
  — fn/type/trait/ADT refs, imports), **method calls** `x.f()` (via the enclosing
  body's `typeck`), and **type-relative value paths** `Type::assoc_fn` /
  `Type::CONST` (`qpath_res`);
- a `ty`-level pass over each item's **lowered signature** (`fn_sig`/`type_of`)
  collecting **type-position assoc projections** `<T as Trait>::Item` — whose HIR
  `PathSegment::res` is `Res::Err` (deferred past name-resolution), so text/HIR
  can't see them; the lowered `Alias(Projection)` `def_id` can.

Hand-verified against `walk::pick_target_kind`: every source reference appears —
both `TargetKind` enums with exactly the variants used, `Option`/`Some`/`None`,
the `for`-loop desugar, *and* the `.iter()`/`.any()` method edges. Assoc value
paths wired up `Type::new()`-style calls (every caller of local `ResolvedPath::new`),
dropping unused-pub candidates 174 → 166. The projection pass was verified on a
purpose-built probe crate (4 projections, all correct) and then found **1 real
projection in syn-workspace a source grep had missed** — `ModuleWalk::next`'s
`Option<Self::Item>` return, resolved to `Iterator::Item`. Remaining gap:
projections appearing *only* in bounds/`where`-clauses (need `predicates_of`) and
opaque/`impl Trait` targets.

`wl-assemble` demonstrates the Phase-2 consumption — edge digest, in-degree
ranking (top: `ResolvedPath` ↑102, `Visibility` ↑52), and a proto unused-pub
signal (166 pub items with zero *intra-crate* refs). That signal over-reports
until cross-crate edges land — see below.

## Cross-crate reverse index (step-4 result)

Orchestrating the **whole workspace** (`wl-embed` with no `-p`: one `dylint::run`,
cargo fans out, 4 fragments, ~19 s) lets `wl-assemble` union every crate's forward
edges into a workspace-wide reverse index — the thing a single per-crate process
provably can't build (SPIKE §5). Result on this repo: **4 crates, 1 355 defs,
11 969 edges; `workspace_lint → syn_workspace` exercises 199 edges.**

The load-bearing finding is the **join key**. `def_path_str` is *not* a stable
cross-crate identity: the defining crate renders a def at its definition path
(`syn_workspace::resolve::workspace::Workspace`) while a consumer renders the same
def at its re-export path (`syn_workspace::Workspace`, via rustc's visible-parent
map) — even impl blocks disagree (`<impl resolve::workspace::Workspace>` vs
`<impl syn_workspace::Workspace>`). So a path-equality join scores **0 / 215**
cross-crate. Switching the join to each def's **`DefPathHash`** (`ItemFact::key` /
`RefEdge::to_key`) — identical no matter which crate observes it — lands **199 / 215**
(the 16 non-joins are enum-variant `ctor`s, a `DefKind` we don't emit as items,
exactly as intra-crate). This is SPIKE §5.4's "stable keys, projected at emit
time" made concrete; the path stays for display only.

`DefPathHash` embeds the `StableCrateId`, which embeds the **rustc version**, so
the key is stable across *observers on one toolchain*, not across toolchains — the
driver (nightly 05-01) and dylib (04-16) now differ in the key column while their
*structure* (paths/kinds/vis/spans/edges) stays byte-identical. Production runs one
pinned driver, so all fragments in a run share a toolchain and the keys are
mutually consistent (they joined 199/215 here).

**Honest verdict on unused-pub.** Candidates are **module-level, inherent-impl,
*and* trait-impl** pub items — every real category is now judged (step 4a folded
the last one in). The split is rustc-emitted, not a text heuristic: `parent_kind`
+ `ItemFact::trait_item` (`opt_associated_item(..).trait_item_def_id()` — `Some`
only for a trait impl). Four corrections got the verdict here:

- **Split inherent vs trait-impl, then *judge* both (step 4a + trait-dispatch
  reachability).** Inherent methods used to be lumped with trait impls and excluded
  wholesale; now they're judged by direct-call edges, and trait-impl items are
  judged by **trait dispatch** (`Assembly::reach_of`): a trait-impl item is reached
  if it has a direct use-site, **or its trait is external** (std/serde/clap — those
  464 items are *sound roots*: external code dispatches `Display::fmt`/
  `Deserialize::deserialize` invisibly, so they can never be proven dead), **or its
  internal trait method is dispatched** anywhere. This replaces "excluded, count
  only" with a real judgment — and would catch a *pub* internal-trait impl that's
  never dispatched (this workspace has none: every internal trait — `Lint`,
  `ResolverPlugin` — is `pub(crate)`, so `int 0`; the branch is latent-but-correct).
- **Discount `use`/re-export edges (step 4b).** `RefEdge::import`, set by a
  `visit_use` override (a use-path's enclosing item is the *module*, so it can't be
  told apart after the fact). A `pub use` doesn't keep a name alive; every real use
  emits its own non-import edge. 496 discounted — without which `builtin_assertions`
  (crate-root `pub use`d, called only from `#[cfg(test)]`) was masked as a
  misleading 0.
- **cfg-matrix union (§7 — now implemented).** The IR is one config (cfg-strip runs
  before the driver sees `TyCtxt`), so a single run over-reports items used only
  under another cfg. `wl-assemble` now takes **N config dirs** and unions them: a
  pub item is a lead iff unreached in *every* config. Over `default` + `--tests`
  (whole workspace): **20 default-alone leads → 9 after the union, 11 retired** by
  test usage — `builtin_assertions`, `Manifest::empty`, `member_by_name`,
  `Workspace::load` (prod uses `load_with_options`; `load` is test-only), etc. The
  cross-config join is `(crate, def_path_str)` (config-stable — `DefPathHash` is
  *not*, see §7); synthetic `--test`-harness `main`s (span-`None`) are filtered so
  they don't masquerade as dead API.
- **Publish/root classification (step 5 — now implemented).** `--ws <root>` reads
  `cargo metadata` (`publish` + target kind) and splits survivors into **DEAD**
  (unused in every cfg *and* in a bin / non-published crate — a hard verdict) vs
  **PUBLISHED API SURFACE** (a published lib's pub API — external consumers
  possible, review for over-exposure not death). Union result: **0 dead + 9
  API-surface** (all syn-workspace). This *replaces* the earlier `referenced`
  dependency-leaf proxy — which mislabels a published **leaf** library (the
  `*-marker` crates, published libs that no workspace crate references) as "dead";
  metadata classifies them as API surface correctly. Bin `main`s are `pub(crate)`,
  already filtered by visibility, so they need no root handling.

Every survivor spot-checked held up as a *genuine* unused-pub item (no missed
edges) — including the fidelity win a text tool can't match: the flagged
`references_from` is correctly distinguished from the *used* `references_from_crate`
(grep conflates them), and `Crate::declared_deps` from the used
`Manifest::declared_deps`, by stable-key resolution. What the cross-crate join
*itself* fixes is false positives among inherent candidates — `Manifest::path`
(4 refs) etc. are unused *within* syn-workspace but used by `workspace_lint`; the
index clears 26. The **trait→impls linkage** (`trait_item` → impl keys) backs the
dispatch judgment and validates cleanly (`Lint::check ← 11 impls`,
`ResolverPlugin::claims_macro ← 4`). Step 5 (publish/root metadata) then splits
those survivors into **0 dead + 9 published-API-surface** — no provably-dead pub
item in the workspace; the 9 are syn-workspace's public API with no in-workspace
consumer, which for a published library is expected, not a kill.

## Findings channel (step-3 result)

Everything above is the **facts channel** — the IR extractor harvests data and the
assembler computes verdicts in plain Rust. The **findings channel** (SPIKE §4/§8)
is the *other* half of the design: real diagnostics emitted through Dylint's native
lint path and captured by the orchestrator. It's now proven end-to-end.

`wl-lint` registers **two** lints in one dylib: the silent `WL_IR_EXTRACT` extractor
(facts) and an *emitting* `WL_UNDOCUMENTED_PUB` demo (findings — warns on a
module-level `pub fn` with no doc comment). Two lints means the macro `register_lints`
won't do (it wires one), so it's hand-written — registering both passes so they run
in **one compilation against one `TyCtxt`** (the SPIKE §8 "same pass" claim, made
concrete). The `extract()` body is untouched, so the facts channel stays byte-
identical to the raw driver.

Three findings on the pinned nightly (`nightly-2026-04-16`), verified:

- **Emission is rustc-native — no `clippy_utils`.** The closure-based `span_lint`
  is gone on this toolchain; emission is struct-based `LintContext::emit_span_lint(
  lint, span, decorator)` where `decorator: impl Diagnostic`. `rustc_errors::
  DiagDecorator(|diag| diag.primary_message(..))` is a built-in closure adapter, so
  no `#[derive(LintDiagnostic)]` and no `clippy_utils` dependency (which would drag
  in the version-lockstep treadmill). The dylib stays `clippy_utils`-free.
- **Capture is `--message-format=json`.** Forwarded through `dylint::run`'s
  `Check.args` (via `embed … -- --message-format=json`), cargo emits a
  `{"reason":"compiler-message","message":{…}}` stream on stdout; the inner
  `message` is the rustc `Diagnostic`: `level`, `message`, `code.code` = the lint
  name, `children`, `rendered` (the clippy-style human text), and `spans[]` with
  `byte_start`/`byte_end` (the `--fix` write surface), line/col, `is_primary`, and
  the `suggested_replacement` / `suggestion_applicability` fields.
- **It's exactly the `workspace-lint` diagnostic shape.** That inner object is the
  `DiagnosticSpan`-shaped JSON the tool's `Diagnostic` already mirrors (and that
  rust-analyzer's `check.overrideCommand` consumes) — so findings flow into the
  existing human/json/github renderers unmodified. Controlled test: 3 undocumented
  pub fns flagged with correct byte spans; a `///`-documented fn and a private fn
  correctly skipped; a plain `//` comment correctly *not* treated as a doc.

The raw `driver/` stays extraction-only — the findings channel is Dylint-native (it
rides the `LintStore` the raw driver doesn't set up), exactly as the two-channel
design intends: extraction is harness-agnostic, findings are a Dylint capability.

## Next steps

1. ~~Emit the parent `DefKind` in `ItemFact`.~~ **Done** — assoc/fn-local
   classification is driven by `ItemFact.parent_kind`, not a path heuristic.
2. Grow `IrFragment`: ~~references (paths, methods, type-relative value paths,
   type-position assoc projections)~~ **done** → remaining: bound/`where`-clause
   projections + opaque targets, then macros, then cfg.
3. ~~Findings channel~~ **done** (see the section below): a second, *emitting*
   `LateLintPass` (`WlFindings`) coexists with the silent extractor in one dylib
   via a hand-written `register_lints`; it uses rustc-native `emit_span_lint` +
   `DiagDecorator` (**no `clippy_utils`**) and is captured via `--message-format=
   json` through `Check.args` as the exact rustc `DiagnosticSpan` JSON the
   `workspace-lint` renderers already consume.
4. ~~Multi-crate orchestration (§4) + cross-crate reverse indexes.~~ **Done** —
   whole-workspace `dylint::run`, a `DefPathHash`-keyed reverse index (path is
   *not* a stable cross-crate key: 0/215 → 199/215), an unused-pub verdict over
   ~~module-level~~ **module-level + inherent-impl** items, ~~`use`/re-export edge
   discounting~~ **done** (`RefEdge::import` via `visit_use`), and ~~inherent-vs-
   trait-impl split~~ **done** (`ItemFact::trait_item`, step 4a — 20 leads + the
   `trait_item`→impls linkage), ~~full trait-dispatch reachability~~ **done**, and
   ~~cfg-matrix union~~ **done**:
   - **Full trait-dispatch reachability** — **done** (`Assembly::reach_of`).
     Trait-impl items are no longer excluded: each is judged by dispatch — external
     trait ⇒ sound root (invisible external dispatch, 464 immune here), internal
     trait ⇒ reached iff its method is dispatched. The `trait_item`→impls map is the
     substrate. Would catch a *pub* internal-trait impl that's never dispatched
     (none here — every internal trait is `pub(crate)`, so that branch is latent).
   - **cfg-matrix union (§7)** — **done**. `wl-assemble <dir>..` takes N config dirs
     (one compile per cfg — the flags are load-bearing since cfg-strip precedes the
     driver) and unions: lead iff unreached in *every* config. Over `default` +
     `--tests`: **20 → 9 leads, 11 retired**. The join is the **dual** of the
     cross-crate one — cross-config on `(crate, def_path_str)` (config-stable;
     `DefPathHash` is *not*, verified 0/475), within-config on `DefPathHash`
     (observer-stable); neither key does both. Synthetic `--test` `main`s (span
     `None`) filtered. `embed` forwards the cfg via `-- --tests` into `Check.args`.
5. ~~Publish/root metadata~~ **done**. `wl-assemble --ws <root>` reads `cargo
   metadata` (`publish` + target kind, no compile) and classifies each union
   survivor by whether its crate's pub API is an **external boundary** — a
   *publishable library* (`publish != false` **and** has a lib target). Two buckets
   replace the flat lead list: **DEAD (verdict)** = unused in every cfg *and* in a
   bin / `publish=false` crate (nothing can reach it → remove); **PUBLISHED API
   SURFACE (root)** = unused in-workspace but a published lib's pub API (external
   consumers possible → review for over-exposure, not death). On this workspace the
   union gives **0 dead + 9 API-surface** (all `syn_workspace`); the single default
   config surfaces **1 dead** (`workspace_lint::DiagnosticBuilder::level` — bin
   crate, retired by the union since `--tests` uses it). This *replaces* the old
   `referenced` dependency-leaf proxy, which mislabels a published **leaf** library
   (the `*-marker` crates — published libs no workspace crate references) as
   "dead"; metadata calls them "API surface" correctly. Bin `main`s need no special
   root handling — they're `pub(crate)`, already filtered by visibility.
6. ~~A second lint on the same IR (`unused-deps`)~~ **done** (see the section
   below). Declared deps (`cargo metadata`) diffed against the reference graph,
   unioned across configs — no new extraction, a second query on the assembled
   model. Facade crates (`clap` → `clap_builder`) are cleared via resolved
   dependency **closures**; judgement scope (normal always, dev only with a
   `--tests` config, build/optional never) is stated in-band.
7. Production migration — **assessed, not started.** The real `workspace-lint`
   couples to `syn-workspace` across **17 files** (`Workspace`/`Crate`/`Item`/
   `ResolvedPath`/`manifest`/re-exported `toml_edit`/`walk_items`/`references`/…),
   so `syn-workspace` also bundles manifest/TOML/path utilities, not just semantic
   resolution — a clean swap must separate those. Decided shape: **backend swap**
   (nightly dylib extracts; stable binary assembles; existing lints query the IR;
   the whole suppression/`--fix`/renderer pipeline stays) — *not* rewriting lints as
   Dylint passes. Must-solve blocker surfaced: the **cargo caching gotcha** (below).
   See SPIKE §11 "Verified by the step-2/3 spike" for the full write-up.

## Unused-deps (second lint, step-3b result)

`wl-assemble` emits a second real lint off the *same fragments* the unused-pub
verdict loads — proving lints compose over the Phase-2 model without re-walking
`TyCtxt` (SPIKE §8 breadth). Needs `--ws` for the `cargo metadata` dep tables:

```sh
REPO="$(git rev-parse --show-toplevel)"
(cd spike && cargo run -q -p wl-assemble -- \
  "$REPO/spike/ir-out-matrix/default" "$REPO/spike/ir-out-matrix/tests" --ws "$REPO")
# … Unused-deps verdict (declared deps vs the reference graph):
#   workspace_lint  16 normal, 6 dev …   ✗ UNUSED  dev  cargo_husky
```

A declared dep is unused iff **no** edge from any of its owner package's compiled
targets meets its resolved dependency closure, across **every** provided config
(unioned like unused-pub). Key results on this workspace:

- **Facade crates handled.** `clap` has **0** edges to `clap`: it re-exports
  everything from `clap_builder`, so every `use clap::Parser` resolves to
  `clap_builder`. Crediting a dep when the referenced set meets `closure(dep)`
  (`clap_builder ∈ closure(clap)`, from the resolve graph) clears it soundly. 15/16
  normal deps matched by direct name; only the pure facade needed the closure. This
  is §6's re-export asymmetry on the dependency axis.
- **Sound scope, over-approximate in the safe direction.** Normal deps always
  judged; dev-deps only when a test target compiled (verified: default-only config
  does *not* flag a dev-dep — no false positive); build-deps never (`build.rs` isn't
  lint-passed); optional deps never (feature-gated). The closure can only *miss* a
  truly-unused dep, never flag a used one — the correct bias for a "delete it" lint.
- **Residual true finding:** `cargo-husky` (dev) — a side-effect/build-hook crate
  with zero code refs by design. A *correct* ref-graph absence, caveated in-band
  (don't remove side-effect crates), not a bug.
