# Direction & Roadmap

Where `workspace-lint` / `syn-workspace` are going, the ideas behind it, and the
phases to get there. Companion to the component-level design in
[`crates/syn-workspace/DESIGN-ir-pipeline.md`](../crates/syn-workspace/DESIGN-ir-pipeline.md).

---

## Working constraints

- **Unpublished, pre-1.0, no backwards-compatibility obligation.** No semver
  contract, no deprecation cycles. The public API evolves freely; `workspace-lint`
  (the only consumer of `syn-workspace`) migrates in lockstep. The one invariant
  we protect across refactors is **observable lint behavior** — the
  `tests/cases/` snapshots.
- **Speed is a feature.** Sub-second whole-workspace analysis is the point.
  Anything that needs the full rust-analyzer frontend is out of scope at runtime
  (it may appear in *tests* as an oracle — see Phase 1).

---

## The thesis

`syn-workspace` is a **deliberately shallow approximation of a name resolver**:
it loads a cargo workspace, builds module trees, resolves `use` chains and
`pub use` re-exports, and token-scans code for path references — with **no type
inference, no trait solving, no proc-macro execution**. Its only job is to feed
`workspace-lint` enough resolution to produce **low-false-positive** lints, fast.

These consequences follow, and they drive everything below:

1. **The approximation's failure mode on valid code is a *missed reference*** —
   not a crash. A missed reference flows up into the lints as a **false
   positive**: `unused-pub` flags an item that *is* used, `unused-deps` flags a
   crate that *is* imported, `architecture` misses a real edge.
2. **Therefore, testing this tool is primarily a false-positive hunt on valid
   code.** "Does it handle all kinds of valid Rust?" means "does clean code stay
   clean, and do real references get recorded?"
3. **Two metrics, in priority order — false positives first, true positives
   second.** A lint that fires on good code trains users to ignore it, so **low
   false-positive rate is the top priority**. But a lint that never fires is also
   useless, so a **high true-positive rate** (catching the real issues) is the
   close-second goal. Both are real targets; SCIP closeness and occurrence recall
   are *proxies* for them. When a proxy and lint correctness disagree, lint
   correctness wins. (Guard against overfitting the resolver to rust-analyzer
   quirks no lint cares about.)
4. **Misses on either axis are acceptable only when documented — clearly but
   briefly.** A surviving false positive or a missed true positive is tolerable
   *if* it's a tracked `known_false_positive` / `known_false_negative` with a
   one-line rationale. Undocumented misses are bugs; documented ones are known
   limits with a forcing function (see the taxonomy below). Keep the note terse —
   a sentence, not an essay.

---

## The core ideas

### A. An occurrence IR (the enabling architecture)

Today the resolver has eight overlapping mechanisms for "what does this code
reference," writing into four channels, with positions thrown away. We restructure
around a single **occurrence model** — every reference site is a
`(raw segments, span, origin)` record — split into two phases:

```
syn (per file) ──lower──▶ IR (raw, unresolved, occurrence-oriented) ──resolve──▶ resolved model
```

- **Phase A (lower):** syntactic, lossy. Macro-body lowering is the single
  extension point (`MacroLowerer`). Everything else (code-path scan, use-trees,
  mod resolution) is core.
- **Phase B (resolve):** mechanical. One central `resolve_occurrence` does all
  crate/self/super peeling + use-binding substitution. A default-empty plugin
  hook here is where *framework semantics* will eventually live.

Why it matters: it **consolidates** the macro sprawl (fewer concepts), it
**preserves spans** (so the model aligns with SCIP), and it gives **two diff
points** — raw occurrences vs. resolved symbols — so a failing oracle diff
localizes to *extraction* vs. *resolution*. Full spec:
[`DESIGN-ir-pipeline.md`](../crates/syn-workspace/DESIGN-ir-pipeline.md).

### B. SCIP as a differential oracle

[SCIP](https://github.com/sourcegraph/scip) is the index rust-analyzer emits
(`rust-analyzer scip`): per file, a list of occurrences `(range, symbol, roles)`
where `symbol` is a fully-resolved canonical path and `roles` distinguishes
definitions from references. **It is exactly the ground truth our token-scan
approximates** — produced by the real resolver with full type inference.

We use it as a **one-directional, class-restricted** oracle, never as drop-in
expected output:

- **Precision** (of the occurrences *we emit*, how many match RA) = our
  **false-positive rate**. Target: ~100%.
- **In-class recall** (of RA's occurrences *in the classes we intend to
  produce* — path-form references, item defs; excluding method calls, field
  access, inferred paths, macro-expansion, locals), how many we catch. Target:
  reachable ~100% because we excluded the impossible.

Global recall against SCIP is **permanently capped well below 100% by design**
(method calls and inferred types dominate idiomatic Rust and we have no types).
Chasing it is a category error. We measure *in-class* recall + precision instead.

**Best fit: the dependency lints.** SCIP symbols carry the package name, so
"is crate X referenced anywhere" is *complete* ground truth that survives every
granularity problem — even `x.foo()` resolves to a symbol tagged with X. For
`unused-pub`/references, SCIP is a **false-positive detector**, not a
completeness checker.

Practicalities: `rust-analyzer scip` is slow (full analysis) → **commit the
`.scip` index per fixture** like a snapshot; the test parses the committed index
(fast, deterministic, no RA on the common path) and re-blesses behind a flag when
the pinned RA version bumps. Parse with the `scip` crate (Sourcegraph's protobuf
bindings).

Complementary oracles (noted, not core): **rustdoc JSON** for the public
item/visibility graph (no occurrence data — complements SCIP for the
re-export / `pub_items` side); **cargo-udeps** as a compiler-backed oracle
specifically for `unused-deps`.

**Spike-validated (2026-05-30).** A throwaway differential harness (a controlled
fixture + this repo) confirmed the approach end-to-end on the pinned toolchain —
rust-analyzer 1.95 `scip`; nightly 1.97 rustdoc JSON `format_version` 57; `scip`
0.7.1 / `protobuf` 3.7.2 (SCIP gen: ~1.6 s fixture, ~4.6 s repo). What it changes:

- **Buildable before Phase 0.** The rustdoc def/visibility oracle and the
  set-level SCIP dependency oracle run against *today's* `pub_items()` /
  `references_from_crate()` — land them first as a semantic regression net
  *ahead of* the IR refactor. The rustdoc oracle flagged the `impl`-block method
  enumeration gap with zero tuning.
- **Dep oracle = intersect, not equate.** SCIP's per-document package set also
  contains sysroot crates (`core`/`std`) and the crate itself, so `unused-deps`
  must compare *declared deps* ∩ SCIP-packages, not raw sets.
- **Cross-validate the oracles.** rustdoc ⇄ SCIP agreed on the re-export
  canonical; make oracle-vs-oracle agreement a Phase 1 guard so a weak
  normalization can't pass for the wrong reason.
- **RA `scip` quirks to tolerate:** duplicate `crate/` symbols across
  bin/example/test targets and occasional "definition should have been in an
  SCIP document" / nested-in-fn misses (rust-analyzer#18771). Commit the index
  and dedupe/skip defensively.
- Normalization + range-encoding specifics:
  [`DESIGN-ir-pipeline.md` §8](../crates/syn-workspace/DESIGN-ir-pipeline.md).

### C. Testing at four altitudes

A pyramid, cheapest/most-precise at the base:

1. **Table-driven unit tests** on the resolver functions (`bindings_from_use`,
   `resolve_occurrence`, each `MacroLowerer`). Microsecond, pinpoint failures.
   This is where the **variant matrix** (below) is exercised.
2. **Curated fixture crates** with hand-authored expectations — the existing
   `tests/cases/` 4-kind taxonomy + a "kitchen-sink" valid crate per lint.
3. **Public-crate corpus** with the SCIP differential — scale and realism on
   code we didn't author (Phase 2).
4. **No-panic / property net** — generate valid Rust (AST → `quote` → parse) or
   replay the corpus; assert termination, no panic, idempotence.

### The four-kind taxonomy (already in `tests/cases/`)

Each lint's fixtures are sorted into `true_positives` / `true_negatives` /
`known_false_positives` / `known_false_negatives`. This is the **forcing function
for documented misses** (thesis point 4), and it maps cleanly onto the two
priority axes:

- **FP axis (top priority):** `true_negatives` must stay clean; surviving false
  positives live in `known_false_positives` with a one-line rationale.
- **TP axis:** `true_positives` must keep firing; missed detections live in
  `known_false_negatives` with a one-line rationale.

A KFP that stops firing or a KFN that starts firing **fails the test** —
signalling "the resolver improved, promote it." Nothing is hidden; every known
miss is a tracked entry with a terse note.

### The variant matrix (the breadth checklist)

The dimensions of valid Rust this tool must not choke on, ranked by where a
token-scanner is most likely to silently miss:

- **`use` forms:** nested groups, `as` rename, glob, `self`/`crate`/`super`/`super::super`, leading `::`, `use {a, b}`, `pub`/`pub(crate)`/`pub(in path)`, raw idents.
- **Reference positions (scanner's weak spot):** turbofish, `<T as Trait>::m`, trait bounds, generic/const-generic args, associated types, macro-call paths, attribute & derive paths, paths in patterns / struct literals, `impl Trait`, paths in closures/async/const blocks.
- **Module structure:** inline vs file `mod`, `mod.rs` vs `m.rs`, `#[path]`, nested dirs, `#[cfg]`-gated mods, mods inside fn bodies, re-export chains (single + glob).
- **Macro bodies:** `quote!`/`quote_spanned!`, `rsx!`/`dioxus::rsx!`, `macro_rules!`, format-string args, nested & opaque user macros.
- **Manifest / workspace shape:** renamed deps (`package=`), `foo.workspace=true`, optional deps + feature gating, dev/build/`target.'cfg()'` deps, multi-target crates (lib+bin+examples+tests+benches+build.rs+proc-macro), workspace globs/exclude/default-members, editions 2015/2018/2021/2024.

---

## Phases

Each phase is gated by the previous one and produces a durable artifact. Phases
0–2 are the spine; 3–4 are the long tail the harness makes tractable.

### Phase 0 — Occurrence IR refactor
**Goal:** the architecture that makes the SCIP loop possible.
Restructure `syn-workspace` around the occurrence IR + two-phase pipeline; unify
the eight reference mechanisms into core + the `MacroLowerer` trait; centralize
resolution. Rewrite `workspace-lint` onto the new model surface.
**Guardrail:** `tests/cases/` snapshots stay green throughout.
**Spec:** [`DESIGN-ir-pipeline.md`](../crates/syn-workspace/DESIGN-ir-pipeline.md).
**Done when:** occurrences are the model's primary reference surface, resolution
is one pure function, and the old fragmented channels are deleted.

**Landed (Phase 0).** The resolver is built around the `Occurrence { segments,
path, span, origin }` model (`Origin = Code | GlobUse | ExternCrate | Macro`),
lowered per-file then resolved centrally by a single `resolve_occurrence`
(Phase A → Phase B). Macro-body lowering is the one extension point
(`plugins`/`MacroLowerer`); `use` imports live in `use_bindings`. A module's
reference surface is its `use_bindings` canonicals + resolved occurrences. The
`tests/cases/` snapshots stayed green across the refactor.

### Phase 1 — SCIP emitter + differential harness
**Goal:** turn "how good is the resolver" into a number Claude can iterate on.
- Emit a SCIP-subset from the resolved occurrences (`Workspace → scip::Index`).
- Build the committed-index diff harness: for a fixture, parse `expected.scip`
  (from a pinned rust-analyzer), filter both sides to the in-class occurrence
  set, report **precision** and **in-class recall**. Bless flow mirrors
  `WORKSPACE_LINT_BLESS`.
- **Start with the dependency lints** (cleanest, complete oracle). Add RA as a
  CI job allowed to be slow / nightly; the common test path stays fast.
**Done when:** `cargo test` reports precision/recall against a committed SCIP
index for at least the `unused-deps` fixtures, and a discrepancy points at a
specific occurrence.

**Landed (pre-Phase-0, committed regression net):** the "buildable today"
slice is in `crates/syn-workspace/tests/oracle.rs` — a fast (`serde_json`-only,
no RA/nightly) differential test that diffs committed, normalized
rustdoc + SCIP oracles against the live resolver across four dimensions
(def/visibility, an independent SCIP def witness, re-export canonicalization,
and the set-level `unused-deps` dependency oracle). Regeneration lives in the
detached `tools/oracle-bless` crate (keeps `scip`/`protobuf` + the nightly/RA
toolchain off the common path); it pins rustdoc `format_version` 57 and fails
loudly on toolchain drift.

**Landed (Phase 1, occurrence-level harness):** `Workspace::scip_occurrences()`
(`crates/syn-workspace/src/scip_emit.rs`) projects the resolved model into a
normalized, SCIP-aligned occurrence list — **not** a foreign `scip::Index`. That
"lean projection" keeps `scip`/`protobuf` out of the published crate's
dependency surface entirely; the literal `Workspace → scip::Index` wrapper is a
feature-gated future addition, deferred until a consumer needs to *emit* a real
`.scip`. `tests/scip_diff.rs` diffs that projection against a committed
per-occurrence rust-analyzer oracle (`expected/scip-occurrences.json`, distilled
by `oracle-bless` with an `impl`/`Method`-suffix in-class filter) and reports
**precision** (gated at 100 %) and **in-class recall** (a ratcheting matched
floor) for the first cut: **cross-crate references** + a symbol-level def
witness. On the `multi_crate` fixture: precision 100 %, in-class recall 12/18 —
the misses are all rust-analyzer's per-path-*segment* occurrences (bare crate /
module prefixes) and field references the resolver structurally can't produce.
Range comparison derives UTF-8 byte columns from byte ranges (the `café` fixture
guards non-ASCII). Still fast-path / `serde_json`-only.

### Phase 2 — Public-crate corpus
**Goal:** confidence at scale, on code we didn't write.
Vendor / submodule a curated, diverse set of real crates spanning the variant
matrix (macro-heavy, framework-using, deep module trees, edition spread, varied
workspace shapes). For each crate, three gates:
- **Smoke:** `Workspace::load` does not panic and terminates.
- **Differential:** SCIP precision ~100%; in-class recall tracked over time.
- **Lint FP audit:** clean crates produce zero diagnostics, or each spurious one
  is filed as a `known_false_positive`.
**Done when:** the corpus runs in CI and regressions in precision or new FPs fail
the build.

**Landed (Phase 2, corpus harness first cut):** real crates are vendored as git
submodules under `corpus/` (`anyhow`, `bitflags`, `heck`, `itertools`), pinned to
release SHAs and `exclude`d from the workspace. A key enabler:
`Workspace::load` now runs `cargo metadata --no-deps` (`walk.rs`) — the resolver
only materializes workspace members, so dependency resolution was pure overhead;
dropping it makes loading work **offline for any crate**. The three gates:
- **Smoke** (`syn-workspace/tests/corpus.rs`): each crate loads without panic,
  terminates (ceiling-gated), yields ≥1 member + ≥1 item. Copies to a tempdir so
  the read-only checkout is never mutated.
- **Differential** (`oracle.rs`): the re-export-immune **set-level** dep oracle
  generalized across the corpus (`itertools`: rust-analyzer proves it references
  `either`; assert `either` is visible to the resolver). The occurrence-level
  diff stays `multi_crate`-only — occurrence precision into *registry* deps is
  unreachable (re-export blindness), so crate granularity is the corpus gate, per
  §B. Vacuous-safe (a dep-free crate proves nothing).
- **Lint FP audit** (`workspace-lint/tests/corpus_fp.rs`): runs `unused-deps` and
  snapshots diagnostics. It **found real false positives** — `unused-deps`
  flagging dev-dependencies used only in doc-tests (`anyhow`'s `futures`),
  external glob imports + derive-attribute paths (`bitflags`'s `serde_lib`), and
  `#[cfg]`-gated test modules (`serde_test`) — all tracked in
  `tests/corpus_fp/README.md` with a snapshot forcing-function. (`unused-pub` is
  excluded: it flags a standalone library's whole public API by construction.)

All gates run fast-path / `serde_json`-only against committed oracles (the
`test` CI job checks out submodules; no RA/nightly in CI); the corpus crates'
zero/light dep graphs keep `cargo metadata --no-deps` network-free. **Deferred:**
heavier / multi-member corpus crates; resolver fixes for the found FP classes
(doc-tests, external globs, derive paths, cfg-gated mods — Phase 3); a
`workflow_dispatch` RA/nightly re-bless job.

### Phase 3 — Close the in-class gap (the iterable loop)
**Goal:** grind the resolver toward in-class completeness.
With harness + corpus in place, every in-class SCIP miss or spurious occurrence
is a concrete, localized bug (extraction = Phase A; resolution = Phase B). Fix,
re-bless, promote `known_false_positives` → `true_negatives` as precision
climbs. This is the open-ended, Claude-friendly loop the whole architecture
exists to enable.
**Done when:** in-class recall plateaus and the remaining misses are documented
non-goals.

**Landed (Phase 3, increment 1 — module-file directory resolution).** The first
corpus FP the loop closed: the resolver now implements the `foo.rs`-owns-`foo/`
module convention. `resolve_mod_file` resolves a plain `mod foo;` in the
declaring module's *owning directory* (`dir_owning_children`: the file's own dir
for a crate root / `mod.rs`, else `<dir>/<stem>/`), and inline `mod a { mod b; }`
owns a deeper `a/` dir — threaded as `mod_dir` through `collect_module_contents`.
`#[path]` stays relative to the declaring file's directory (unchanged). This was
the real root cause of bitflags' `serde_lib`/`serde_test` `unused-deps` FPs — the
referencing file (`src/external/serde.rs`) was simply never loaded, not a
derive/glob/cfg gap as first hypothesized. The bug was invisible to `dogfood` and
every prior fixture because they all use the `mod.rs` convention; the corpus
(real third-party code) surfaced it. Guarded by `nested_modules` fixture tests
(`file_module_owns_subdir`, `inline_mod_in_file_module_resolves_nested_dir`) and
the now-clean `corpus_fp/bitflags.stderr` snapshot.

**Landed (Phase 3, increment 2 — target-root regression fix).** A follow-up to
increment 1: `dir_owning_children` must NOT apply to *target roots* (each cargo
target's `src_path` — lib/bin/example/test/bench/build-script), which own their
*containing* directory regardless of filename. The fix threads the owning dir
explicitly (roots pass `file.parent()`; `mod foo;`-descended files pass
`dir_owning_children`), so e.g. `tests/it.rs`'s `mod common;` resolves to
`tests/common/mod.rs` again. The **module-tree lint is now enabled + denied on
dogfood** (`.workspace-lint.toml`) as the standing forcing function that would
have caught the regression.

**Landed (Phase 3, increment 3 — corpus broadening + reclassification).** Added
`thiserror` (a multi-member workspace: `thiserror` lib + `thiserror-impl`
proc-macro) — the corpus's first >1-member crate and proc-macro target — and began
auditing `unused-pub` on multi-member crates (cross-crate referrers make it
meaningful; the public API + proc-macro entries are correctly exempt). It
immediately surfaced two concrete resolver gaps (bare single-ident sibling
references; `use path::{self}` group-import binding) — captured as documented
known-FPs in `corpus_fp/thiserror.stderr` for future increments. Also
reclassified anyhow's prior `unused-deps` findings: `syn` is a confirmed **true
positive** (a genuinely unused dev-dep — a lint win, not an FP), `futures` the
lone remaining dependency FP (doc-test-only).

**Landed (Phase 3, increment 4 — the two thiserror gaps closed).** Both
`unused-pub` FP classes thiserror surfaced are fixed in `syn-workspace`, taking
`corpus_fp/thiserror.stderr` from 8 → clean: (1) `extract_code_paths` now keeps a
bare single ident that names a same-module **sibling** (not only a `use` binding),
so a sibling referenced by bare name in a field type / struct literal / supertrait
bound / impl position is recorded; (2) `bindings_from_use` binds the module for
`use path::{self, …}` instead of a name called `self`. Both only add same-crate
references (resolution's sibling branch was already in place), so the cross-crate
SCIP precision gate is provably unmoved (precision 100 %, recall floor 12); the
FPs reclassify `Unused` → `IntraCrate`. The last remaining corpus FP is anyhow's
doc-test-only `futures`.

**Landed (Phase 3, increment 5 — doc-comment code-fence scanning).** The last
corpus FP closed: anyhow's `futures`, a dep used only inside a
`/// use futures::stream::…` doc-test example. `syn-workspace/src/resolve/doc_fences.rs`
scans rust-compiling code fences in line doc comments (`///` / `//!`) for
crate-name references — skipping `text` / `ignore` / `compile_fail` /
other-language fences and honoring rustdoc hidden lines (`# `). Per the chosen
scope, these refs feed the **dependency lint only** (`Workspace::doctest_dep_refs`,
unioned into `unused-deps`'s referenced-crate set); they are deliberately kept
out of the occurrence/reference graph, so `unused-pub`, `architecture`, and the
SCIP projection are untouched by construction (doc-test code is a separate
compilation unit). The corpus is now FP-free — every audited crate is clean
except anyhow's `syn`, a confirmed true positive. Block doc comments (`/** … */`)
are a documented non-goal.

**Landed (Phase 3, increment 6 — corpus broadening: `memchr` + macro-namespace
fix).** Added `memchr` to the corpus to stress deep, cfg-gated, arch-specific
module trees (`src/arch/{x86_64,aarch64,wasm32,all,generic}/…`) — the
structurally-hardest crate to date. It loaded and resolved cleanly and surfaced
**one** real FP: the `log` dep, referenced only as `log::debug!`/`log::trace!`
inside memchr's own `debug!`/`trace!` `macro_rules!` wrappers, with a local
`macro_rules! log` of the same name. A `macro_rules!` definition introduces a
name in the *macro* namespace only, so it must not shadow a path-position
reference (`log::debug` resolves `log` in the type/module namespace). `sibling_name`
(`syn-workspace/src/resolve/module_tree.rs`) no longer treats `macro_rules!`
items as siblings; the change only *adds* external references that were being
shadowed, so it is precision-neutral — the SCIP differential is unmoved
(precision 100 %, in-class recall 12/18). Guarded by the
`unused-deps/true_negatives/dep_referenced_in_macro_not_shadowed_by_local_macro`
fixture. The corpus stays FP-free (anyhow's `syn` remains the lone true positive).

**Landed (lint policy — publish-aware `unused-pub`).** Distinct from the
resolver-precision loop above: a fix to *what `unused-pub` is allowed to flag*.
Previously every library-public item in every member lib was exempt as "external
API surface", so the lint could only catch `pub` items trapped behind a private
module hop — it couldn't flag over-exposed *internal-crate* APIs, which is its
main job at the workspace level. Now the exemption applies **only** when a crate
declares `publish = true` (or a registry list); a `publish = false` or
publish-absent crate is treated as workspace-internal and its unused `pub` items
are flagged. `syn-workspace` gained `Manifest::publish()` / `Workspace::resolved_publish`
(reading the raw manifest, since `cargo metadata` collapses `publish = true` and
an absent field). Config: `assume-all-public` (opt back into the old behavior;
used by the corpus FP-audit) and `publish-hint-threshold` (a crate-level
"set `publish = true`" nudge once an internal crate floods). The three published
crates here set `publish = true`; dogfood stays clean. Two former unused-pub
`known_false_negatives` were promoted to `true_positives`. Known limitation: a
definition's own ident counts as a self-reference, so a never-used item reads as
`IntraCrate` rather than `Unused` (a separate resolver fix).

### Phase 4 — Framework semantics via Phase B plugins
**Goal:** handle what token-scanning structurally can't, demand-driven.
When a framework causes systematic FPs no amount of scanning fixes, add a Phase B
`resolve()` plugin. **First customer: Dioxus** — `#[component] fn Foo` makes
`Foo {}` a legal `rsx!` target, so without a cross-linking pass `unused-pub`
false-positives on every component. Each plugin is gated by a failing
corpus/SCIP case; the hook stays empty until then.
**Done when:** the first framework plugin lands with a regression test, proving
the Phase B extension point carries real weight.

---

## Non-goals / honest limits

- We will **never** match SCIP globally. Method calls, field access, type
  inference, and proc-macro expansion are out of scope by design. Success is
  *in-class* precision + recall as a proxy for the real targets — **low
  false-positive rate first, high true-positive rate second** — not global SCIP
  equality.
- SCIP is the **means**, lint correctness is the **end**. Do not add resolver
  complexity to chase RA behavior no lint consumes.
- Plugins are **independent pure contributors** merged deterministically — never
  order-dependent or mutually-aware. Core resolution (use-bindings, re-export,
  cross-crate attribution) stays core, not pluggable.
