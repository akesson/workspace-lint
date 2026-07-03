#!/usr/bin/env bash
# Transitional backend diff (migration PRs 8–11): run workspace-lint twice
# over a target workspace — once per semantic backend — and diff the
# normalized diagnostics. Used to classify every divergence while porting a
# lint; dies with the legacy backend in the deletion PR.
#
# Usage: tools/backend-diff/backend-diff.sh [TARGET_DIR]   (default: this repo)
#
# Reading the output: an empty diff = the backends agree on TARGET_DIR.
# KNOWN, deliberate divergences (each pinned by a fixture):
#   - unused-deps on target-cfg-gated deps: the syn backend judges them
#     (cfg-blind parse); the rustc backend never does (a foreign platform's
#     dep is unobservable from this host) — see tests/cases/unused-deps/
#     known_false_negatives/target_cfg_dep_unused.
set -euo pipefail

REPO="$(git rev-parse --show-toplevel)"
TARGET="${1:-$REPO}"
BIN="$REPO/target/debug/workspace-lint"
test -x "$BIN" || (cd "$REPO" && cargo build -q -p workspace-lint)

OUT="$(mktemp -d)"
trap 'rm -rf "$OUT"' EXIT

run() { # <backend> <outfile>
  # Diagnostics land on stderr; exit 1 (findings) must not abort the diff.
  # Strip the engine's platform-dependent progress lines (the same
  # normalization the case harness applies).
  (cd "$TARGET" && WL_SEMANTIC_BACKEND="$1" "$BIN" 2>&1 1>/dev/null) \
    | grep -v '^Checking with toolchain `' > "$2" || true
}

run rustc "$OUT/rustc.txt"
run syn "$OUT/syn.txt"

echo "── backend diff on $TARGET (syn → rustc):"
if diff -u "$OUT/syn.txt" "$OUT/rustc.txt"; then
  echo "── backends agree."
fi
