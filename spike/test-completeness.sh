#!/usr/bin/env bash
# WS5.1 — wl-embed completeness guard (SPIKE §11 caching gotcha).
#
# Reproduces the exact hole: `WL_IR_OUT` isn't in cargo's fingerprint, so a
# fragment removed out-of-band while the dylint cache is warm is NOT recreated by
# a plain re-run (every crate reads "fresh", the lint pass never re-runs). The
# guard must detect the miss, force a re-lint by bumping the dylib mtime, and
# regenerate ONLY workspace members (no registry-dep recompile).
set -euo pipefail

REPO="$(git rev-parse --show-toplevel)"
# The extractor dylib (migration PR 2: extractor/, package wl-extractor); the
# filename embeds toolchain + host triple, so glob rather than hardcode.
LIB="$(ls "$REPO"/extractor/target/debug/libwl_extractor@*.dylib "$REPO"/extractor/target/debug/libwl_extractor@*.so 2>/dev/null | head -1 || true)"
test -n "$LIB" || { echo "build the extractor first: (cd extractor && cargo build)"; exit 1; }
EMBED="$REPO/spike/embed/target/debug/wl-embed"
OUT="$REPO/spike/ir-out-guard"
# Member count = fragments a full default-config run must produce (5 since
# wl-ir joined the workspace in migration PR 1).
MEMBERS=5
LOG="$(mktemp -d)"
trap 'rm -rf "$LOG"' EXIT
rm -rf "$OUT"

echo "── run 1 (deterministic full lint): expect $MEMBERS fragments + 'completeness check OK' ──"
# Bump the dylib so dylint re-lints every member regardless of the ambient cargo
# cache state — this run writes all fragments itself, so the guard sees no miss.
touch "$LIB"
"$EMBED" "$REPO" "$LIB" "$OUT" 2> "$LOG/run1.err" >/dev/null
n1="$(ls "$OUT" | wc -l | tr -d ' ')"
echo "  fragments: $n1"; test "$n1" -eq "$MEMBERS"
grep -q "completeness check OK" "$LOG/run1.err"; echo "  ✓ guard reported OK (dylint wrote all $MEMBERS)"

echo "── delete syn_workspace_marker.json out-of-band (cargo stays fresh) ──"
rm "$OUT/syn_workspace_marker.json"
test ! -f "$OUT/syn_workspace_marker.json"

echo "── run 2 (warm): guard must detect + regenerate the missing fragment ──"
"$EMBED" "$REPO" "$LIB" "$OUT" 2> "$LOG/run2.err" >/dev/null
test -f "$OUT/syn_workspace_marker.json"; echo "  ✓ fragment regenerated"
grep -q "fragment(s) missing" "$LOG/run2.err"; echo "  ✓ miss detected"
grep -q "completeness restored" "$LOG/run2.err"; echo "  ✓ restored via forced re-lint"
# No registry-dep recompile: the mtime bump only invalidates member units.
if grep -Eq '(Compiling|Checking) (serde|proc-macro2|quote|syn|anyhow|toml|toml_edit|clap|cargo_metadata) ' "$LOG/run2.err"; then
  echo "  ✗ a registry dep was recompiled:"; grep -E '(Compiling|Checking) ' "$LOG/run2.err"; exit 1
fi
echo "  ✓ no registry-dep recompile (members only)"

echo "── run 3 (warm, complete): guard idempotent, no forced re-lint ──"
"$EMBED" "$REPO" "$LIB" "$OUT" 2> "$LOG/run3.err" >/dev/null
grep -q "completeness check OK" "$LOG/run3.err"; echo "  ✓ guard reported OK"
! grep -q "fragment(s) missing" "$LOG/run3.err"; echo "  ✓ no miss"

echo "── --tests config: guard's expected set = actual +test fragments ──"
OUTT="$REPO/spike/ir-out-guard-tests"; rm -rf "$OUTT"
"$EMBED" "$REPO" "$LIB" "$OUTT" -- --tests 2> "$LOG/runt.err" >/dev/null
nt="$(ls "$OUTT" | wc -l | tr -d ' ')"
echo "  +test fragments: $nt"
grep -q "completeness check OK" "$LOG/runt.err"; echo "  ✓ guard OK under --tests ($nt fragments)"

echo ""
echo "completeness guard OK"
