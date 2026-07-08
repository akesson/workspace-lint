# duplicate-code

Name-invariant (Type-2) duplicate detection: flags groups of structurally
identical code regions — whole functions/methods, nested blocks, and runs of
consecutive statements — **even when local variable names and literal values
differ**.

Because its classifier consults the rustc call graph, this is a **semantic
lint**: it needs a compiling workspace and pays extraction (the same cost
class as `unused-pub`), and `--fast-only` skips it entirely — a
`check duplicate-code --fast-only` is a hard error, never a silently
degraded run. (`--stats` below is the exception: it stays build-free.)

## What it checks

Each region is normalized (local bindings α-renamed to positional
placeholders in first-occurrence order, literals abstracted per kind) and
hashed; identical structures bucket together, so the pass is near-linear and
finds cross-crate copy-paste for free. Statement runs are what keep a clone
in one piece: a span copied mid-body into two otherwise-different functions
is reported once, maximally — not as fragments of whichever nested blocks
happen to match. Renames must be *consistent*: swapping two variables
changes the structure and correctly breaks the match. Called functions,
types, and field/method names are kept verbatim — two blocks that call
different functions are never clones.

Each clone group gets ONE line-anchored diagnostic at its first site (by
file and line), listing the other sites in a note. Advisory only: `--fix`
never rewrites duplicates — resolving one means extracting a shared
function, which is an author decision.

**Naming the fix.** Beyond flagging a group, the lint classifies *what
refactoring it calls for* and reshapes the help accordingly: two copies of
one function the call graph confirms are interchangeable → *keep one and
redirect the callers* (with the outside call-site count); a copy nothing
references → *delete the dead one*; the same method across impls of one
trait → *a default method on the trait*; free functions all taking one
workspace type first → *a method on that type*; copies built from the same
UI macro (`rsx!` by default) → *extract a component*. When two
token-identical functions resolve *different* callees (a local shadowing a
same-named function), the merge is withheld with a caution note instead of
advising a rewrite that would change behavior; a group with no clear named
fix keeps the generic "extract the shared logic" help. Classification only
reshapes the help and notes — it never changes which groups fire or at what
level. Turn it off with `classify = false`.

Two noise filters (both on by default; zero disables) separate real
copy-paste from *normal* duplication, following the filtering practice of
token-based clone detectors (CCFinderX's RNR/TKS metrics):

- `min-distinct-anchors` — a region must reference at least N distinct
  *verbatim* names (called functions, types, fields, methods). A struct
  literal or a `HashMap` fill table carries plenty of tokens but almost no
  distinct names — convergent boilerplate, not copies.
- `min-non-repeating-ratio` — at least this fraction of the region's
  normalized token stream must not repeat an earlier window of the *same*
  region. One row stamped out N times (an insert table, assert stutter) is
  self-repetition, not duplication worth extracting.

After grouping, the lint reads back the concrete literals normalization
abstracted away and prices the extraction:

- Each distinct way the instances' literals co-vary is one **parameter** of
  the function an extraction would produce. A group needing more than
  `max-parameters` (default 3; 0 disables) is a *data table* and is
  suppressed. Survivors carry the price in a note.
- One literal breaking an otherwise *consistent* cross-instance mapping —
  the copy renamed `"alpha"` → `"beta"` but missed one occurrence — is the
  classic copy-paste-bug signature. Such a group is reported with a
  `possible copy-paste drift` note and is **never** suppressed by
  `max-parameters`. The rule is deliberately strict, so expect it to be
  quiet — when it speaks, look closely.

When the duplicated region is a **statement run** (a copied span mid-body,
not a whole fn or block), the extraction is priced as a signature: variables
it reads that were bound earlier become the extracted fn's parameters, and
variables it binds that are read afterwards become its return values — e.g.
`an extracted fn would take 2 parameters (items, config) and return total`.
A run returning more than `max-live-out` values (default 1; 0 disables) would
extract into a fn returning a tuple the caller must destructure — awkward
enough that the finding is **downgraded to a warning** so it stops failing CI
without disappearing. That downgrade is a lint-chosen level, so — like
`architecture`'s per-rule severities — a `[lints]` override neither
re-escalates nor drops it (a per-crate `allow` won't silence a downgraded
run; only a global `allow` does). The liveness pass is syntactic (a flat
lexical approximation, not a real scope stack), so it errs toward
*softening*; its parameters are variables, where the extraction-cost note's
are literals.

## Configuration

The `[duplicate-code]` table's presence enables the lint (it is off by
default — noisy enough to demand deliberate opt-in). All fields are optional:

```toml
[duplicate-code]
min-lines = 8            # smallest region (source lines) considered
min-tokens = 40          # …and its minimum normalized-token weight
min-instances = 2        # copies needed before a group is reported
ignore-literals = true   # `+ 1` vs `+ 5` still matches
ignore-test-code = true  # skip tests/benches/examples and #[cfg(test)]
cross-crate-only = false # true = only groups spanning ≥ 2 crates
min-distinct-anchors = 4      # noise filter: distinct verbatim names
min-non-repeating-ratio = 0.5 # noise filter: fraction of non-repeated windows
max-parameters = 3       # extraction-cost gate: literal parameters allowed
max-live-out = 1         # run-clone downgrade: return values before it warns
classify = true          # name the refactoring each group calls for
component-macros = ["rsx"]  # UI macros whose copies suggest a component
baseline = "…"           # path to the accepted-clone baseline (see below); unset = off
include = []             # workspace-relative globs to scan (empty = all)
exclude = []             # globs to skip (wins over include)
```

Ad-hoc (no config) form — one flag per field; `--exact-literals` /
`--include-test-code` invert the `ignore-*` defaults, `--no-classify` turns
the classifier off, `--component-macros` overrides the UI-macro list,
`--max-live-out` sets the run-clone downgrade threshold, and `--baseline
<path>` reads an accepted-clone baseline for one ad-hoc run:

```sh
workspace-lint check duplicate-code                     # defaults, zero config
workspace-lint check duplicate-code --cross-crate-only  # just cross-crate copies
workspace-lint check duplicate-code --min-lines 12 --exact-literals
workspace-lint check duplicate-code --stats             # threshold-tuning readout
```

`--stats` prints a measure-only readout (per-group divergence, parameter
histogram, drift candidates, a per-run live-out column and histogram, each
group's `fp` fingerprint — the baseline match key — threshold sweeps, and each
group's *syntactic* refactoring class) instead of
diagnostics — the view to tune thresholds
against. It stays build-free even though the lint itself is semantic, so the
call-graph verdicts (merge / delete-dead-copy / withheld) never appear in
its class column — only the classes decidable from syntax.

## Silencing

Silence a deliberate duplication with an `expect` directive at the group's
anchor site (its first reported location):

```rust
workspace_lint::expect!(duplicate_code);
```

Because the diagnostic anchors at a line, an impl-block directive needs a
File-anchored form. Prefer `expect` over the permanent `allow`.

## Baseline ratchet

For brownfield adoption. A legacy tree can have hundreds of clone groups on day
one, so making
`duplicate-code` deny-level would fail CI immediately. Point `baseline` at a
checked-in file and only *new* duplication fails — the SonarQube new-code-gate
model, but keyed on a portable content **fingerprint** rather than a line
number, so an entry survives reformatting, local renames, and moving the code
to another file:

```sh
# 1. add `baseline = "duplicate-code.baseline.toml"` to [duplicate-code]
workspace-lint --baseline-write     # 2. record every current group, then commit it
# 3. CI stays green; a NEW or GROWN clone group now fails
```

The ratchet only tightens. A group listed in the baseline is skipped; a group
that grows past its recorded instance count still fires (with a *grew beyond
its baseline* note); and any entry that no longer matches — the clone was
fixed, or shrank below its recorded count — is reported at the lint's level
(PHPStan's `reportUnmatchedIgnoredErrors` model), so a resolved duplication
forces you to regenerate rather than letting the baseline rot. Each finding
names the regen command.

Because the fingerprint is a name-invariant *content* hash, renaming locals or
reflowing whitespace never invalidates an entry — but editing the duplicated
code itself, flipping `ignore-literals`, changing the detection thresholds, or
upgrading workspace-lint's normalizer does. Regenerate after any such change;
the stale-entry findings make a forgotten regen loud, never silent. A partial
fix can also surface smaller clones the resolved parent previously subsumed —
correct ratchet behavior, reported as new. `--baseline-write` is default-run
only (the baseline must be generated under the same config CI lints with), and
it interacts with `expect!`/`allow!` by redundancy: a directive on a baselined
group goes stale (`stale-expect`) — pick one silencing mechanism per group.

## Known limits

Deliberate scope boundaries:

- An edited statement mid-clone splits the match into the identical runs on
  either side of it — each reported only if it clears the thresholds on its
  own (near-miss "Type-3" detection is out of scope).
- `macro_rules!` bodies are not scanned.
- Drift detection sees literal divergence only — a changed *identifier*
  produces a different structure and silently leaves the group rather than
  being reported as drift.
