#!/usr/bin/env bash
# WS1 A4 — findings-channel suggestion round-trip (SPIKE §12.7).
#
# Proves a rustc-native `span_suggestion` (the production `unused-pub` fix shape:
# vis_span → `pub(crate)`, MachineApplicable) survives Dylint + cargo's
# `--message-format=json` capture with byte-exact offsets, and that applying
# those bytes the way `crates/workspace-lint/src/fix.rs` does yields code that
# still compiles.
set -euo pipefail

REPO="$(git rev-parse --show-toplevel)"
LIB="$REPO/spike/wl-lint/target/debug/libwl_lint@nightly-2026-04-16-aarch64-apple-darwin.dylib"
EMBED="$REPO/spike/embed/target/debug/wl-embed"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cp -R "$REPO/spike/probes/expansion" "$WORK/probe"   # never dirty the fixture
rm -rf "$WORK/probe/target"

# 1. Capture findings as cargo JSON (embed inherits stdout → redirectable).
"$EMBED" "$WORK/probe" "$LIB" "$WORK/ir" -- --message-format=json \
  > "$WORK/findings.jsonl" 2> "$WORK/embed.err"

# 2–5. Parse, assert, apply, in one python pass.
python3 - "$WORK" <<'EOF'
import json, sys, pathlib
work = pathlib.Path(sys.argv[1])
src_path = work / "probe/src/lib.rs"
src = src_path.read_bytes()

# The exact `pub` token of `undocumented_roundtrip` — the write surface we
# expect a byte-exact suggestion for.
needle = b"pub fn undocumented_roundtrip"
expect_lo = src.find(needle)
assert expect_lo != -1, "probe changed: can't find undocumented_roundtrip"

# Collect every suggestion span across all wl_undocumented_pub messages.
suggs = []
for line in work.joinpath("findings.jsonl").read_text().splitlines():
    line = line.strip()
    if not line or not line.startswith("{"):
        continue
    obj = json.loads(line)
    if obj.get("reason") != "compiler-message":
        continue
    msg = obj["message"]
    if (msg.get("code") or {}).get("code") != "wl_undocumented_pub":
        continue
    spanpool = list(msg.get("spans", []))
    for child in msg.get("children", []):
        spanpool += child.get("spans", [])
    for sp in spanpool:
        if sp.get("suggested_replacement") is not None:
            suggs.append(sp)

# Exactly one suggestion is expected: the two macro-generated pub fns are
# from_expansion, so the extractor's guard withholds a fix surface for them.
assert len(suggs) == 1, f"expected 1 suggestion, got {len(suggs)}: {[(s['byte_start'],s['suggested_replacement']) for s in suggs]}"
s = suggs[0]

assert s["byte_start"] == expect_lo, f"suggestion at byte {s['byte_start']}, expected {expect_lo}"
assert s["suggested_replacement"] == "pub(crate)", s["suggested_replacement"]
assert s["suggestion_applicability"] == "MachineApplicable", s.get("suggestion_applicability")
assert s["byte_end"] - s["byte_start"] == 3 and src[s["byte_start"]:s["byte_end"]] == b"pub", \
    f"vis token not exactly `pub`: {src[s['byte_start']:s['byte_end']]!r}"
print(f"  suggestion verified: bytes {s['byte_start']}..{s['byte_end']} = 'pub' -> 'pub(crate)' [MachineApplicable]")

# Apply exactly as crates/workspace-lint/src/fix.rs does (byte-range replace).
fixed = src[:s["byte_start"]] + b"pub(crate)" + src[s["byte_end"]:]
src_path.write_bytes(fixed)
assert b"pub(crate) fn undocumented_roundtrip" in fixed
print("  applied byte-range replacement")
EOF

# 6. The rewrite must still compile (pub(crate) unused fn is a warning, not error).
( cd "$WORK/probe" && cargo check 2>&1 | tail -2 )
grep -q 'pub(crate) fn undocumented_roundtrip' "$WORK/probe/src/lib.rs"
echo "round-trip OK: rustc suggestion captured, byte-exact, applied, still compiles"
