# Design: occurrence IR + unified macro-lowering for `syn-workspace`

Status: **proposed** · Scope: `crates/syn-workspace` internals + its public model
· Companion: [`/docs/ROADMAP.md`](../../docs/ROADMAP.md) (why this exists / where it leads)

> **No backwards-compatibility constraint.** This crate is unpublished and has
> no external consumers. The public API (`Workspace`/`Crate`/`Module`/`Item`/…)
> is free to change; `workspace-lint` (the only consumer) migrates in lockstep.
> The single invariant we protect *during* the refactor is **observable lint
> behavior** — the `tests/cases/` snapshots and fixture assertions stay green at
> each step. That's a correctness guardrail, not an API contract.

## 0. Goals / non-goals

**Goals**
- Collapse the *eight* reference-producing mechanisms feeding *four* channels into **one occurrence model** with origin tags.
- Pull resolution (crate/self/super peeling + use-binding substitution + sibling rewrite) **out of extraction** into one central pass — yielding two diff points (raw occurrences vs. resolved symbols) for the SCIP oracle.
- Align the internal model with SCIP: **occurrence-oriented, span-carrying, provenance-tagged**.
- Add a default-empty **Phase B** hook for framework semantics.

**Non-goals (out of *this* refactor)**
- No change to resolution *semantics* — steps 1–4 below are snapshot-identical.
- No framework Phase B logic; no per-site external-macro change (test-gated follow-ups).

## 1. What's there today (so the collapse is concrete)

Eight mechanisms, four output channels, canonicalization smeared across ~5 call sites:

| Mechanism | Trigger | Output channel | Canonicalizes where |
|---|---|---|---|
| `extract_code_paths` | every non-use/mod/macro item | `Module.references` | inline (`resolve_code_path`) |
| `extract_macro_paths` (autodetect) | `macro_rules!` def | `Module.macro_implicit_refs` | inline (`resolve_macro_path`) |
| `expansion_uses!` (annotation) | marker macro | `Module.macro_implicit_refs` | inline |
| `plugins::matches`+`refs` (quote, rsx) | matched macro invocation | `Module.macro_implicit_refs` | inline |
| `glob_targets_from_use` | `use a::b::*` | `Module.references` | peeled in `use_tree` |
| `extern crate foo` | extern-crate item | `Module.references` | none |
| `bindings_from_use` | `use` | `Module.use_bindings` | peeled in `use_tree` |
| `register_external_macro_uses` (Layer 3) | caller config | `Workspace.external_macro_refs` (workspace-wide) | parsed strings |

Two tells that this is the right thing to refactor:
- **`quote`'s plugin is a no-op gate** — `references()` returns `Vec::new()` purely so `matches()` can flip on the token scan (`plugins/quote/mod.rs:32`). The trait can't express "just scan," so it fakes it.
- **`ResolveContext` is an empty placeholder** "for when scope-aware resolution lands" (`plugins/mod.rs:47`) — the read-phase context is already known to be under-built.

## 2. The IR

Per-module, **raw** (unresolved), occurrence-oriented. Bindings stay a distinct
channel (a rename map, not a reference), but a `use` *also* emits an occurrence
for the imported path — folding in the "references_by_crate combines
use-bindings + references" step at `resolve/mod.rs:634`.

```rust
/// One raw reference site, before resolution. Spans are always present for
/// parser-produced occurrences (the SCIP emitter maps 1:1 over these).
struct Occurrence {
    /// Path segments exactly as written — NO crate/self/super peeling,
    /// NO use-binding substitution. `Bar::baz()` -> ["Bar", "baz"].
    segments: Vec<String>,
    span: SourceSpan,
    origin: Origin,
}

enum Origin {
    Code,                          // fn body / type sig / attr path
    Use,                           // imported path of a `use` (also yields a UseBinding)
    GlobUse,                       // `use a::b::*` prefix
    ExternCrate,                   // `extern crate foo`
    Macro { lowerer: LowererId },  // emitted by a macro lowerer (see §3)
}

struct ModuleIr {
    name: String,
    canonical: ResolvedPath,
    visibility: Visibility,
    file: Option<PathBuf>,
    submodules: Vec<ModuleIr>,
    broken_mod_decls: Vec<BrokenModDecl>,
    items: Vec<Item>,             // unchanged
    bindings: Vec<UseBinding>,    // unchanged (Tier 1) — rename map for substitution + re-export
    occurrences: Vec<Occurrence>, // THE unification: replaces references + macro_implicit_refs
    cfg_features: Vec<String>,
}
```

**Key move:** `segments` are raw. Today `extract_code_paths` peels and
substitutes *during* the scan; here it does only *candidate selection* (keep
multi-segment runs, plus single idents whose name matches a binding — the
binding-name set is already available, since bindings are collected first). All
peeling/substitution moves to §4.

**End-state public surface (no compat shim):** the resolved model exposes
`occurrences` directly (each resolved to a canonical `ResolvedPath` + span +
origin), with ergonomic accessors `Module::references()` (origin ∈
{Code, Use, GlobUse, ExternCrate}) and `Module::macro_refs()` (origin = Macro).
`workspace-lint` is rewritten to consume these. The old `references` /
`macro_implicit_refs` `Vec<ResolvedPath>` fields are **removed**, not preserved.

> **Correctness trap — the crate reference set unions *all* origins, including
> Macro.** Today `references_by_crate` = `use_bindings ∪ references ∪
> macro_implicit_refs` (`resolve/mod.rs:1048`). `unused-deps` and `unused-pub`'s
> `referring_crates` query depend on that union: a dep or item used *only* inside
> a macro body must still count as referenced, or it regresses into a false
> positive. So `references_by_crate` must be built from **every occurrence
> (all origins)** — `Module::macro_refs()` is *additional*, not a partition that
> excludes macro refs from the reference set. Only the per-crate suppression
> channel `Workspace::macro_implicit_refs_for` filters to `origin = Macro`.

## 3. The unified Phase-A extension point: `MacroLowerer`

The *only* extension point is macro-body lowering — exactly what four of the
eight mechanisms do. `MacroBodyParser` becomes:

```rust
trait MacroLowerer: Send + Sync {
    fn claims(&self, mac: &MacroSite) -> bool;          // which macro paths/defs this owns
    fn lower(&self, mac: &MacroSite, cx: &LowerCtx) -> Lowered;
}

enum Lowered {
    TokenScan,                  // run the baseline token scan over the body (old Layer-1)
    Exact(Vec<Occurrence>),     // structured parser fully replaces the scan
    ScanPlus(Vec<Occurrence>),  // scan baseline AND these structured extras
}
```

`Lowered`'s three variants are exactly the three behaviors the current code
hand-codes as separate `if` branches in `collect_module_contents`:

| Today | Becomes a built-in `MacroLowerer` returning |
|---|---|
| `macro_rules!` autodetect | `claims` ident-defs → `TokenScan` |
| `expansion_uses!` annotation | `claims` marker path → `TokenScan` (over args) |
| `quote` (no-op gate) | `claims` quote → `TokenScan` *(the fake-empty hack disappears)* |
| `dioxus rsx` | `claims` rsx → `ScanPlus(component occurrences)` |
| Layer 3 external | `claims` configured invocation → `Exact(declared occurrences)` |

Lowerers emit **raw** occurrences (segments + span) — they no longer
canonicalize, so `ResolveContext`'s reason-to-exist evaporates and `LowerCtx`
can be minimal. That genuinely *shrinks* the plugin contract.

**Guardrail:** code-path extraction, use-tree walking, mod resolution,
extern-crate stay **core**, not lowerers. The trait is `MacroLowerer`, not a
general `Lowerer`. Resisting "everything is a plugin" is what keeps this
maintainable.

## 4. The two phases

**Phase A — Lower** (per file, syntactic, lossy → `ModuleIr` tree):
parse syn → sibling names + items + mod-decl resolution (structure) → use-trees
→ bindings; run the `MacroLowerer` registry over each macro site →
`Origin::Macro` occurrences; run the core code-path extractor over regular items
→ raw `Origin::Code` occurrences; glob-use + extern-crate occurrences. Output is
**raw and unresolved**.

**Phase B — Resolve** (per crate / workspace, mechanical):

```rust
fn resolve_occurrence(occ: &Occurrence, scope: &Scope,
                      bindings: &[UseBinding], siblings: &HashSet<String>)
                      -> Option<ResolvedPath>
```

This is the *current* `resolve_code_path` / `resolve_macro_path` /
`peel_path_prefix` logic — now **one pure function applied centrally over the
IR**, not interleaved in three extractors. Then: build `ReExportIndex` from
bindings (unchanged); build `references_by_crate` / `canonical_refs_by_path`
(unchanged, now sourced from resolved occurrences); finally the **Phase B plugin
hook**:

```rust
fn resolve(&self, tree: &mut ResolvedWorkspace) {}  // default no-op
```

Framework semantics (Dioxus `#[component]` ↔ rsx cross-linking) land here later,
demand-driven by a failing SCIP/case test.

## 5. What it buys (against the north star — see ROADMAP)

- **Deletes concepts:** 8 mechanisms → core + 1 trait; three reference channels + the "combine use-bindings" step → one occurrence list; the quote no-op-gate hack and the `ResolveContext` placeholder both vanish.
- **Two SCIP diff points:** raw occurrences (Phase A) localize *extraction* bugs; resolved symbols (Phase B) localize *resolution* bugs. This is what makes the iteration loop converge instead of flail.
- **Spans survive to the model** → the SCIP emitter is a straight map; today positions are discarded for everything except use-bindings.
- **Resolution centralized + table-testable** as one function.

## 6. Honest risks

- **Occurrence volume / memory.** Raw occurrences carry spans and aren't deduped → larger than today's `BTreeSet<ResolvedPath>`. Bound it by keeping candidate-selection in Phase A (don't emit every bare ident); dedup is a view concern. These trees are small; measure, don't pre-optimize.
- **Behavior drift.** Steps 1–4 must be semantics-preserving. Guardrail: the existing `cases.rs` snapshots + fixture assertions + inline unit tests (and later the SCIP diff). Anything that changes output is a separate, test-gated step.
- **Layer 3 nuance.** Folding external macros into `MacroLowerer` *can* make attribution per-site instead of workspace-wide — strictly more precise, but a semantics change. Deferred to a follow-up so it doesn't ride in on the mechanical refactor.
- **Scope — this changes extraction/resolution faithfulness, not analysis depth.** The IR does *not* close gaps that need type info or cross-item reasoning, and those stay tracked misses: trait-method dispatch via `dyn`/generics, `pub(crate) use` re-export hops (re-export-index design limit, not extraction), transitive architecture violations (a lint-side graph analysis), and **`pub` items inside `impl` blocks** (an item-*enumeration* gap — `item_from_syn` only walks module-level items; fixable in Phase A but explicitly out of the mechanical refactor — see ROADMAP Phase 3).

## 7. Migration sequence (each step green on the existing suite)

1. Introduce `Occurrence`/`Origin`/`ModuleIr`; have `collect_module_contents`
   build the IR, then a **temporary** adapter flattens IR → today's resolved
   `ModuleContents`. Snapshots unchanged. (Adapter is scaffolding, removed by
   step 4 — not a permanent compat layer.)
2. Move peel+substitute out of `extract_*` into central `resolve_occurrence`.
   Snapshots unchanged.
3. Replace the four macro `if`-branches with the `MacroLowerer` registry +
   `Lowered`; port autodetect/annotation/quote/rsx. Snapshots unchanged.
4. Make occurrences the primary model surface; add `references()`/`macro_refs()`
   accessors; **rewrite `workspace-lint` to consume them**; delete the old
   fields and the step-1 adapter. Snapshots unchanged (behavior identical,
   call sites updated).
5. *(landed — ROADMAP Phase 1)* SCIP emitter over resolved occurrences. Shipped
   as `Workspace::scip_occurrences() -> Vec<ScipOccurrence>` (`src/scip_emit.rs`):
   a normalized, SCIP-aligned projection rather than a foreign `scip::types::Index`,
   so the published crate gains **no `scip`/`protobuf` dependency** and the diff
   harness (`tests/scip_diff.rs`) stays `serde_json`-only. A feature-gated
   `to_scip_index()` wrapper that emits the real foreign type is deferred until a
   consumer needs to produce a `.scip` (none in Phase 1). The empty Phase B
   `resolve()` hook remains a Phase 4 item.
6. *(later, test-gated — ROADMAP Phase 4)* per-site external macros; framework
   Phase B semantics.

## 8. Spike-validated normalization + encoding spec (2026-05-30)

A throwaway harness diffed the resolver against rustdoc JSON and rust-analyzer
SCIP on a controlled fixture and on this repo. Results the Phase 1 emitter /
diff harness must encode:

**Identity normalization** — project all three schemes to canonical segments
(`Vec<String>`):

- syn-workspace `ResolvedPath::segments()` is the reference form.
- rustdoc: `paths[id].path` is already the segment vector.
- SCIP: `[package.name] ++ descriptor names` via `scip::symbol::parse_symbol`
  (which un-escapes backtick-wrapped non-ASCII idents like `` `café` ``). Two
  required rewrites:
  - **package name** is the cargo (hyphenated) name; syn uses the code name →
    map `-` → `_` (`syn-workspace` → `syn_workspace`). Sysroot crates carry a
    *URL* in the version field, but the package name is still the 3rd
    space-delimited token.
  - **inherent methods** are encoded `impl#[Type]method().`, **not**
    `Type#method` → needs an `impl#[T]m` → `T::m` rewrite. Only relevant once the
    impl-block item-enumeration gap (`item_from_syn`, §6) closes, or for
    reference-level diffs; module-level defs need no rewrite.
- Measured: **27/27 (100%)** of this repo's module-level public def-kind items
  (all in `syn-workspace`'s public API; the binary + marker crates expose none)
  normalize to identical segments across syn + SCIP; **4/4** on the fixture
  across all three schemes (incl. the non-ASCII and re-exported cases).

**Range encoding** — rust-analyzer's SCIP sets
`Document.position_encoding = UTF8CodeUnitOffsetFromLineStart`, and the measured
width of `café` was 5 (UTF-8 bytes), so SCIP columns align with
`SourceSpan.byte_range` **for rust-analyzer**. The harness must *read*
`position_encoding` rather than assume — another SCIP producer may emit
UTF-16/32. Keep a non-ASCII fixture (`café`) as the regression guard.

**`pub(crate)`-hop clarification (sharpens §6).** A *public* re-export chain
cannot pass through a `pub(crate)` hop — `pub use` of a `pub(crate)` item is a
hard error (E0364, verified). So the re-export index dropping `pub(crate)` hops
can only ever miss *crate-internal* references (which matter for `unused-pub`'s
within-workspace usage), never a *public-surface* false negative; the rustdoc
public oracle neither catches that case nor needs to.

**Syntactic vs. effective visibility (private-tree oracle).** The committed net's
`--document-private-items` rustdoc oracle validates the *full* module tree
(including private / `pub(crate)` items) and a visibility **tier**
(public / crate / internal) against the resolver. It is deliberately scoped to
those three tiers: `syn-workspace` records **syntactic** visibility (the written
`pub(super)` / `pub(in …)`), whereas rustdoc reports **effective** visibility, and
the two legitimately diverge for `pub(super)`/`pub(in …)`. A `pub(super)` item in a
*crate-root* module, for instance, is effectively crate-visible — rustdoc renders
it `"crate"`, not `"restricted"`. Reconciling those would require an
effective-visibility model the resolver intentionally doesn't build, so the oracle
excludes them and checks only `public`/`crate`/`private`, where syntactic and
effective coincide. (Found while adding the private-tree oracle: a `pub(super)`
probe tripped the tier check with `syn PubSuper vs rustdoc "crate"`.)

**Parsing choices that worked.** rustdoc JSON parsed via `serde_json::Value`
(sidesteps the `rustdoc-types` ↔ format-version lock; a typed harness must pin
the release whose `FORMAT_VERSION == 57`); SCIP via `scip` 0.7.1
(`Index::parse_from_bytes`, `parse_symbol`, `Document.position_encoding`) with
`protobuf = "=3.7.2"`.
