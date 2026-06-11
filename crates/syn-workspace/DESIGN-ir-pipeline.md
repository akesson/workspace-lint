# Design: occurrence IR + unified macro-lowering for `syn-workspace`

Status: **living design doc** — the IR refactor (§7) has landed and the framework
Phase B hook (§4) is in active use. Scope: `crates/syn-workspace` internals + its
public model. The project thesis, SCIP-oracle rationale, testing strategy, and
project-level non-goals (formerly the standalone `docs/ROADMAP.md`, retired once
the phased build-out completed — its development log lives in git history) now
live in §9–§12 below.

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

**Landed (Phase 4, increment 1).** The hook shipped as `ResolvePass`
(`plugins/mod.rs`), with one deliberate divergence from the `&mut
ResolvedWorkspace` sketch above: a pass is `fn contribute(&self, crates: &[Crate])
-> Vec<ContributedRef>` — it *reads* the resolved members and *returns* reference
edges rather than mutating the tree, which makes "independent pure contributors
merged deterministically" hold by construction (the single writer unions the
edges into `references_by_crate` before `canonical_refs_by_path` is built; set
union is order-free). The first pass, `DioxusComponentPass`, links bare `Foo {}`
rsx invocations to the same-crate `pub fn Foo`, reading them straight from the
occurrence IR.

Making the IR carry those bare usages required one Phase A change: the
macro-lowering dispatch previously fired only on *item-position* `syn::Item::Macro`,
but `rsx!` lives in fn bodies. The walk now also visits each item's nested bodies
(`NestedMacroLowering` in `module_tree.rs`) and dispatches claimed macros, taking
only the **structured** (`ScanPlus`/`Exact`) output — the baseline token scan
already covers fn-body macro *tokens*, so `TokenScan` lowerers are skipped to
avoid double-counting. Bare component names become `Origin::Component` occurrences
(unresolved by the central resolver, excluded from the SCIP projection like
`Origin::Macro`) that the pass binds. Gated on the `dioxus` feature (the only
structured lowerer today), so a feature-off build skips the nested walk entirely.
This is what keeps the plugin pure — it reads the model, never re-parses source —
and gives any future structured lowerer fn-body capture for free.

## 5. What it buys (against the north star — see §9)

- **Deletes concepts:** 8 mechanisms → core + 1 trait; three reference channels + the "combine use-bindings" step → one occurrence list; the quote no-op-gate hack and the `ResolveContext` placeholder both vanish.
- **Two SCIP diff points:** raw occurrences (Phase A) localize *extraction* bugs; resolved symbols (Phase B) localize *resolution* bugs. This is what makes the iteration loop converge instead of flail.
- **Spans survive to the model** → the SCIP emitter is a straight map; today positions are discarded for everything except use-bindings.
- **Resolution centralized + table-testable** as one function.

## 6. Honest risks

- **Occurrence volume / memory.** Raw occurrences carry spans and aren't deduped → larger than today's `BTreeSet<ResolvedPath>`. Bound it by keeping candidate-selection in Phase A (don't emit every bare ident); dedup is a view concern. These trees are small; measure, don't pre-optimize.
- **Behavior drift.** Steps 1–4 must be semantics-preserving. Guardrail: the existing `cases.rs` snapshots + fixture assertions + inline unit tests (and later the SCIP diff). Anything that changes output is a separate, test-gated step.
- **Layer 3 nuance.** Folding external macros into `MacroLowerer` *can* make attribution per-site instead of workspace-wide — strictly more precise, but a semantics change. Deferred to a follow-up so it doesn't ride in on the mechanical refactor.
- **Scope — this changes extraction/resolution faithfulness, not analysis depth.** The IR does *not* close gaps that need type info or cross-item reasoning, and those stay tracked misses: trait-method dispatch via `dyn`/generics, `pub(crate) use` re-export hops (re-export-index design limit, not extraction), transitive architecture violations (a lint-side graph analysis), and **`pub` items inside `impl` blocks** (an item-*enumeration* gap — `item_from_syn` only walks module-level items; fixable in Phase A but explicitly out of the mechanical refactor — tracked in §12).

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
5. *(landed)* SCIP emitter over resolved occurrences. Shipped
   as `Workspace::scip_occurrences() -> Vec<ScipOccurrence>` (`src/scip_emit.rs`):
   a normalized, SCIP-aligned projection rather than a foreign `scip::types::Index`,
   so the published crate gains **no `scip`/`protobuf` dependency** and the diff
   harness (`tests/scip_diff.rs`) stays `serde_json`-only. A feature-gated
   `to_scip_index()` wrapper that emits the real foreign type is deferred until a
   consumer needs to produce a `.scip` (none in Phase 1). The empty Phase B
   `resolve()` hook remains a Phase 4 item.
6. *(framework Phase B semantics — landed; see §4)* the
   `ResolvePass` hook + `DioxusComponentPass` (see §4). Per-site external macros
   remain a later test-gated follow-up.

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

**Pinned oracle toolchain (last bless, 2026-05-30 spike):** rust-analyzer 1.95
(`scip`), nightly 1.97 (rustdoc JSON `format_version` 57, the
`EXPECTED_RUSTDOC_FORMAT` constant in `tools/oracle-bless`). The bless tool
asserts the rustdoc format and bails on drift; re-validate the RA symbol scheme
when bumping it.

---

> **§9–§12 were the standalone `docs/ROADMAP.md`.** They are the evergreen *why*
> (thesis, oracle rationale, testing strategy, project non-goals) that outlived the
> phased build-out. The phase-by-phase development journal that used to sit here is
> in git history; it is not reproduced.

## 9. Project thesis — a deliberately shallow resolver

`syn-workspace` is a **deliberately shallow approximation of a name resolver**: it
loads a cargo workspace, builds module trees, resolves `use` chains and `pub use`
re-exports, and token-scans code for path references — with **no type inference, no
trait solving, no proc-macro execution**. Its only job is to feed `workspace-lint`
enough resolution to produce **low-false-positive** lints, fast. Sub-second
whole-workspace analysis is the point; anything needing the full rust-analyzer
frontend is out of scope at runtime (it may appear in *tests* as an oracle — §8,
§10).

Four consequences drive everything:

1. **The approximation's failure mode on valid code is a *missed reference*, not a
   crash.** A missed reference surfaces in the lints as a **false positive**:
   `unused-pub` flags an item that *is* used, `unused-deps` flags a crate that *is*
   imported, `architecture` misses a real edge.
2. **So testing this tool is primarily a false-positive hunt on valid code** —
   "does clean code stay clean, and do real references get recorded?"
3. **Two metrics, in priority order — false positives first, true positives
   second.** A lint that fires on good code trains users to ignore it, so a low FP
   rate is the top priority; a lint that never fires is also useless, so a high TP
   rate is the close second. SCIP precision / in-class recall (§10) are *proxies*;
   when a proxy and lint correctness disagree, lint correctness wins.
4. **Misses on either axis are acceptable only when documented.** A surviving FP or
   a missed TP is tolerable *if* it is a tracked `known_false_positive` /
   `known_false_negative` with a one-line rationale (§11). Undocumented misses are
   bugs; documented ones are known limits with a forcing function.

## 10. SCIP as a differential oracle

[SCIP](https://github.com/sourcegraph/scip) is the index rust-analyzer emits
(`rust-analyzer scip`): per file, occurrences `(range, symbol, roles)` where
`symbol` is a fully-resolved canonical path — **exactly the ground truth our
token-scan approximates**, produced by the real resolver with full type inference.
We use it as a **one-directional, class-restricted** oracle, never as drop-in
expected output:

- **Precision** (of the occurrences we emit, how many match RA) = our
  **false-positive rate**. Target ~100%.
- **In-class recall** (of RA's occurrences *in the classes we intend to produce* —
  path-form references and item defs; excluding method calls, field access,
  inferred paths, macro-expansion, locals) — how many we catch.

Global recall against SCIP is **permanently capped well below 100% by design**
(method calls and inferred types dominate idiomatic Rust and we have no types);
chasing it is a category error. We measure *in-class* recall + precision instead.
**Best fit: the dependency lints** — SCIP symbols carry the package name, so "is
crate X referenced anywhere" is complete ground truth even through `x.foo()`; for
`unused-pub` it is a false-positive detector, not a completeness checker.

The harness commits the oracle index per fixture like a snapshot (RA is slow),
parses it on the fast path (`serde_json` only — no rust-analyzer or nightly in CI),
and re-blesses behind a flag when the pinned toolchain bumps (`tools/oracle-bless`).
Current gate (`tests/scip_diff.rs`, `multi_crate`): **precision 100%, in-class
recall floor 12/18** — the misses are RA per-path-*segment* occurrences and
field/method references the resolver structurally can't produce. Encoding and
normalization specifics are §8. Complementary oracles: rustdoc JSON for the public
item/visibility graph (committed net in `tests/oracle.rs`) and cargo-udeps as a
compiler-backed `unused-deps` oracle (noted, not core).

## 11. Testing strategy

Four altitudes, cheapest/most-precise at the base:

1. **Table-driven unit tests** on resolver functions (`bindings_from_use`,
   `resolve_occurrence`, each `MacroLowerer`) — microsecond, pinpoint failures;
   where the variant matrix below is exercised.
2. **Curated fixture crates** with hand-authored expectations — `workspace-lint`'s
   `tests/cases/` four-kind taxonomy.
3. **Public-crate corpus** (`corpus/` submodules) with the SCIP differential —
   scale and realism on code we did not author.
4. **No-panic / property net** — load the corpus; assert termination and no panic.

**The four-kind taxonomy** (`tests/cases/<lint>/`): every lint's fixtures sort into
`true_positives` / `true_negatives` / `known_false_positives` /
`known_false_negatives`. This is the forcing function for documented misses (thesis
point 4): `true_negatives` must stay clean and `true_positives` must keep firing,
while surviving FPs / missed TPs live in the `known_*` buckets with a one-line
rationale. **A KFP that stops firing, or a KFN that starts firing, fails the
test** — signalling "the resolver improved, promote it." Nothing is hidden.

**The variant matrix** — the dimensions of valid Rust this tool must not choke on,
ranked by where a token-scanner is most likely to silently miss:

- **`use` forms:** nested groups, `as` rename, glob, `self`/`crate`/`super`/`super::super`, leading `::`, `use {a, b}`, `pub`/`pub(crate)`/`pub(in path)`, raw idents.
- **Reference positions (the scanner's weak spot):** turbofish, `<T as Trait>::m`, trait bounds, generic/const-generic args, associated types, macro-call paths, attribute & derive paths, paths in patterns / struct literals, `impl Trait`, paths in closures/async/const blocks.
- **Module structure:** inline vs file `mod`, `mod.rs` vs `m.rs`, `#[path]`, nested dirs, `#[cfg]`-gated mods, mods inside fn bodies, re-export chains (single + glob).
- **Macro bodies:** `quote!`/`quote_spanned!`, `rsx!`/`dioxus::rsx!`, `macro_rules!`, format-string args, nested & opaque user macros.
- **Manifest / workspace shape:** renamed deps (`package=`), `foo.workspace=true`, optional deps + feature gating, dev/build/`target.'cfg()'` deps, multi-target crates (lib+bin+examples+tests+benches+build.rs+proc-macro), workspace globs/exclude/default-members, editions 2015/2018/2021/2024.

## 12. Project non-goals / honest limits

Distinct from §0 (which scopes the *refactor*); these are the **project-level**
limits, by design:

- **We will never match SCIP globally.** Method calls, field access, type
  inference, and proc-macro expansion are out of scope. Success is *in-class*
  precision + recall as a proxy for the real targets (low FP first, high TP
  second), not global SCIP equality.
- **SCIP is the means, lint correctness is the end.** Do not add resolver
  complexity to chase RA behavior no lint consumes.
- **Phase B plugins are independent pure contributors** merged deterministically —
  never order-dependent or mutually-aware (§4). Core resolution (use-bindings,
  re-export, cross-crate attribution) stays core, not pluggable.
- **Standing tracked misses** — each a forcing-function fixture or a documented
  structural non-goal: transitive architecture violations (need a call-graph /
  type-signature tier), `pub` methods in `impl` blocks (an item-enumeration gap; no
  lint can consume the def anyway, since method *calls* are `x.method()` receiver
  syntax that needs type inference), `#[cfg_attr]` / `include!` path resolution,
  external-crate glob exports (need rustdoc JSON), block doc comments (`/** … */`),
  trait dispatch via `dyn`/generics, and `#[derive(...)]`-driven uses.

## 13. Reference-evidence tiers: resolving vs. asserting plugins (design)

Status: **design — not yet implemented.** Motivated by the 2026-06-11
own-workspace audit: after the increment-4 core fixes, every remaining
`unused-deps` false positive came from *macro-contract* knowledge the resolver
can't parse — `#[derive(EnumString)]` expanding to code that references
`strum`, `#[serde(with = "::serde_with::…")]` naming a path inside a string
literal, `#[wasm_bindgen_test]` requiring `wasm-bindgen` at expansion time, the
`md-5` package exposing a `md5` lib target. Each is fixable, but **not by
parsing more syntax** — only by *asserting* a contract. That is a different
kind of evidence than the `rsx!` lowerer produces, and conflating them would
rot the precision story. So: name the tiers, give every reference edge a
provenance, and hold each tier to its own contract.

**Tier R — resolving (evidence: parsed syntax).** What exists today.
- *R1, Phase A `MacroLowerer`* (§3): parses the actual macro body with the real
  grammar (`dioxus-rsx`), emits span-carrying occurrences. Contract: a wrong
  occurrence is a **bug**; structured output is fixture- and corpus-gated.
- *R2, Phase B `ResolvePass`* (§4): binds captured-but-unresolved names against
  the resolved model (`MacroCallPass`, `GlobImportPass`,
  `DioxusComponentPass`). Contract: by-name binding may **over-link, only in
  the FP-safe direction** (suppresses findings, never creates them), with the
  tradeoff documented per pass; origins excluded from the SCIP projection.

**Tier H — asserting (evidence: a declared upstream contract).** The new tier.
An assertion says "when trigger X appears, refs Y exist", citing the upstream
crate's documented behavior — a *vendored* `expansion_uses!`, exactly the
shape the user-facing `[[macros.external]]` config already has. Sub-kinds:
- *H1, expansion assertions*: derive/attribute/macro name → implied crate or
  item refs. `EnumString|Display|…` (strum_macros) → `strum`;
  `#[wasm_bindgen_test]` → `wasm_bindgen`; `#[tokio::main]` →
  `tokio::runtime`.
- *H2, string-path assertions*: an attribute whose **value is a path by the
  framework's contract** — `#[serde(with = "…")]`, `#[serde(crate = "…")]`.
  The trigger names the attribute key; the value is parsed as a path.
- *H3, manifest assertions*: knowledge about dep naming, not code —
  separator-insensitive dep matching (`md-5` ↔ `md5::…`) inside the dep lint.

**One mechanism, three sources.** Built-in H rules, user `[[macros.external]]`
entries, and in-source `expansion_uses!` / comment directives are the same
concept at three ownership levels. Implementation: generalize the config shape
into a `UsageAssertion { id, trigger, implies }` model; built-ins ship as a
static data table (no code per rule). Application is **trigger-narrowed**: an
assertion fires only in modules where its trigger actually appears (derive
list, attribute path, macro invocation), emitting occurrences with
`Origin::Asserted { rule }` *at the trigger's span* — which also delivers the
long-promised narrowing of the Layer-3 workspace-wide broadcast, and gives
every suppression an addressable site.

**Traceability invariants** (what keeps the tiers honest):
1. **Provenance everywhere.** Every reference edge carries its mechanism:
   `Origin` for occurrences (extended with `Asserted{rule}`), a provenance id
   on `ContributedRef`. A future `--explain <dep|item>` walks from a would-be
   finding to the evidence that suppressed it ("used: asserted by
   `strum-derive` at src/html_tag.rs:3").
2. **Registry with forcing function.** Each built-in rule = `{id, trigger,
   implies, upstream citation, guarding fixture}`. A coverage test asserts
   every rule has its `true_negatives` fixture (the `LintId::ALL` ↔
   `messages::scenarios()` pattern). A rule that can't cite an upstream
   contract doesn't ship — it's config, not a built-in.
3. **Tier contracts in CI.** R1 stays SCIP-diffable; R2/H origins stay out of
   the SCIP projection so the precision gate measures only parsed evidence.
   Removing a wrong H rule is always correct; "patching" one (making it
   conditional) means it was really R-shaped and should be promoted.
4. **Assertions never create findings.** All H evidence flows into reference
   sets only — it can suppress an unused-finding, never trigger one. (Same
   FP-safe direction R2 already commits to.)

Out of scope for tier H: `cargo-husky`-style side-effect-only deps (no code
contract to assert — that's the `[unused-deps] ignore` knob's job) and
trait-method dispatch (`.context()` → `anyhow`), which no assertion can see —
that's the SCIP-backend option's territory, not a plugin's.
