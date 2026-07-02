#!/usr/bin/env bash
# WS4 part 2 — do the pivot's documented syn false-negatives/positives flip?
#
# Runs the rustc extractor over the actual tests/cases known-false fixtures
# (copied to a tempdir; never mutated) and reports, honestly:
#   - unused-pub KFN `pub_method_in_impl_block`: FLIPS (fixed). syn can't
#     enumerate impl-block items; the rustc IR carries the unused pub impl method
#     as an inherent-impl candidate → false negative becomes a true positive.
#   - unused-pub KFP `ffi_no_mangle_export`: NOT fixed yet. The `#[no_mangle]`
#     FFI export has no Rust referrer, so the pivot flags it too — until the
#     extractor emits attributes so the assembler can treat it as an export root
#     (a deferred extraction gap, see SPIKE §12 ledger).
set -euo pipefail

REPO="$(git rev-parse --show-toplevel)"
# The extractor dylib (migration PR 2: extractor/, package wl-extractor); the
# filename embeds toolchain + host triple, so glob rather than hardcode.
LIB="$(ls "$REPO"/extractor/target/debug/libwl_extractor@*.dylib "$REPO"/extractor/target/debug/libwl_extractor@*.so 2>/dev/null | head -1 || true)"
test -n "$LIB" || { echo "build the extractor first: (cd extractor && cargo build)"; exit 1; }
EMBED="$REPO/spike/embed/target/debug/wl-embed"
CASES="$REPO/crates/workspace-lint/tests/cases/unused-pub"

extract() { # <case-dir> <workdir>
  cp -R "$1/workspace/." "$2/"
  "$EMBED" "$2" "$LIB" "$2/ir" >/dev/null 2>&1
}

echo "── KFN: pub_method_in_impl_block (expect: FLIPS to a real finding) ──"
W1="$(mktemp -d)"; extract "$CASES/known_false_negatives/pub_method_in_impl_block" "$W1"
python3 - "$W1/ir" <<'EOF'
import json, sys, glob, os
d = {"items": [], "references": []}
for f in glob.glob(os.path.join(sys.argv[1], "*.json")):
    j = json.load(open(f)); d["items"] += j["items"]; d["references"] += j["references"]
m = next((i for i in d["items"] if i["path"][-1] == "never_used"), None)
assert m, "never_used absent from the rustc IR — extractor regression"
inherent = m["parent_kind"] == "impl" and m["trait_item"] is None and m["visibility"] == "Public"
uses = [e for e in d["references"] if e["to_key"] == m["key"] and not e["import"]]
assert inherent, f"not classified as an inherent-impl pub candidate: {m}"
assert not uses, f"unexpectedly has {len(uses)} use-edges"
print(f"  ✓ {'::'.join(m['path'])}: inherent-impl pub method, 0 uses → unused-pub candidate")
print("    (syn's model omits impl-block items entirely → this is the false negative, now visible)")
EOF
rm -rf "$W1"

echo "── KFP: ffi_no_mangle_export (honest: NOT fixed — needs attribute capture) ──"
W2="$(mktemp -d)"; extract "$CASES/known_false_positives/ffi_no_mangle_export" "$W2"
python3 - "$W2/ir" <<'EOF'
import json, sys, glob, os
d = {"items": [], "references": []}
for f in glob.glob(os.path.join(sys.argv[1], "*.json")):
    j = json.load(open(f)); d["items"] += j["items"]; d["references"] += j["references"]
f = next((i for i in d["items"] if i["path"][-1] == "exported_for_ffi"), None)
assert f, "exported_for_ffi absent"
uses = [e for e in d["references"] if e["to_key"] == f["key"] and not e["import"]]
# The extractor emits no attribute info, so #[no_mangle] can't exempt it: the
# item looks exactly like a dead pub fn. This is the honest gap.
print(f"  • {'::'.join(f['path'])}: pub fn, {len(uses)} Rust uses; #[no_mangle] NOT captured")
print("    → the pivot would still flag it. Fix requires emitting attributes so the")
print("      assembler can treat FFI exports as reachability roots (SPIKE §12 ledger).")
EOF
rm -rf "$W2"

echo ""
echo "KFN flip demonstrated; KFP gap documented honestly."
