//! Transitional fidelity oracle — SPIKE-rustc-fidelity-tree.md §10 / §11.1.
//!
//! Diffs the rustc-driver IR (ground truth) against the syn-workspace resolver's
//! model of the *same* crate and reports a **scored** fidelity delta.
//!
//! The comparison is apples-to-apples on the granularity syn actually supports:
//! top-level **named definitions** (the set `ItemKind::is_definition()` defines —
//! fn/struct/enum/union/trait/type/const/static/macro). Three classes are
//! excluded from the score and reported separately, because syn *structurally
//! cannot* represent them (no proc-macro execution, no descent into impl/trait
//! or fn bodies). Each is now classified from the rustc-emitted parent `DefKind`
//! (`ItemFact::parent_kind`), not a path heuristic:
//!   - associated items (parent `impl`/`trait` — methods, assoc consts/types),
//!   - fn-local defs (parent `fn`/`const`/`static`/`closure` — body-nested), and
//!   - modules (representation differs; sidestepped to keep the score clean).
//! Those exclusions are the headline "gap" the pivot closes.
//!
//! Usage: wl-fidelity <REPO_ROOT> <RUSTC_IR_JSON> [CRATE_CODE_NAME=syn_workspace]

use std::collections::BTreeMap;

use syn_workspace::{ItemKind, TargetKind, Visibility as SynVis, Workspace};
use wl_ir::{IrFragment, Visibility as IrVis};

/// Normalized definition set. Key = (canonical path, shared-vocab kind);
/// value = is-public (the coarse visibility axis we score).
type DefSet = BTreeMap<(String, &'static str), bool>;

fn main() {
    let mut args = std::env::args().skip(1);
    let repo = args.next().expect("arg1: repo root");
    let ir_json = args.next().expect("arg2: rustc IR json");
    let crate_code = args.next().unwrap_or_else(|| "syn_workspace".to_string());

    // ── rustc side (ground truth) ───────────────────────────────────────────
    let frag: IrFragment =
        serde_json::from_str(&std::fs::read_to_string(&ir_json).expect("read IR json"))
            .expect("parse IR json");

    // Pre-scan: every path that has a `fn` def. Used to strip `--test` harness
    // descriptor consts — the `#[test]` desugar emits a `TestDescAndFn` *const* at
    // the same path as the test fn, so a `const` shadowing a `fn` is harness
    // scaffolding, not a source definition. (Verified: 188/188 on this crate.)
    let fn_paths: std::collections::HashSet<String> = frag
        .items
        .iter()
        .filter(|i| i.kind == "fn")
        .map(|i| i.path.join("::"))
        .collect();

    let mut rustc_defs: DefSet = BTreeMap::new();
    let (mut rustc_assoc, mut rustc_local, mut rustc_mods, mut rustc_synth, rustc_total) =
        (0usize, 0usize, 0usize, 0usize, frag.items.len());
    for it in &frag.items {
        let Some(kind) = norm_rustc_kind(&it.kind) else {
            if it.kind == "mod" {
                rustc_mods += 1;
            }
            continue;
        };
        let path = it.path.join("::");
        // Strip harness synthetics so they aren't counted as spurious syn misses:
        // the generated `main` (no source span) and the per-test descriptor consts
        // (a `const` shadowing a `fn`). Harmless in non-test IR (neither occurs).
        if it.span.is_none() || (kind == "const" && fn_paths.contains(&path)) {
            rustc_synth += 1;
            continue;
        }
        // Principled container classification from the rustc-emitted parent
        // `DefKind` (replaces the old snake_case-path heuristic): only defs whose
        // parent is a module are in syn's representable set. `impl`/`trait`
        // parents are associated items; any other parent (fn/const/static/closure
        // body) is a fn-local def — both are structural gaps syn can't reach.
        match it.parent_kind.as_deref() {
            Some("impl") | Some("trait") => {
                rustc_assoc += 1;
                continue;
            }
            Some("mod") | None => {} // module-level (None = crate root only)
            _ => {
                rustc_local += 1;
                continue;
            }
        }
        let is_pub = matches!(it.visibility, IrVis::Public);
        rustc_defs.insert((path, kind), is_pub);
    }

    // ── syn side (being retired) ────────────────────────────────────────────
    let ws = Workspace::load(&repo).expect("Workspace::load failed");
    let krate = ws
        .member_by_code_name(&crate_code)
        .unwrap_or_else(|| panic!("`{crate_code}` is not a workspace member"));

    let mut syn_defs: DefSet = BTreeMap::new();
    for target in krate.targets.iter().filter(|t| t.kind == TargetKind::Lib) {
        for (_m, item) in target.root.walk_items() {
            if !item.kind.is_definition() {
                continue;
            }
            let Some(kind) = norm_syn_kind(item.kind) else {
                continue;
            };
            let is_pub = matches!(item.visibility, SynVis::Public);
            syn_defs.insert((item.canonical.display(), kind), is_pub);
        }
    }

    // ── diff ────────────────────────────────────────────────────────────────
    let (mut matched, mut vis_agree) = (0usize, 0usize);
    let mut rustc_only: Vec<(String, &str)> = Vec::new();
    for (key, r_pub) in &rustc_defs {
        match syn_defs.get(key) {
            Some(s_pub) => {
                matched += 1;
                if s_pub == r_pub {
                    vis_agree += 1;
                }
            }
            None => rustc_only.push((key.0.clone(), key.1)),
        }
    }
    let syn_only: Vec<(String, &str)> = syn_defs
        .keys()
        .filter(|k| !rustc_defs.contains_key(*k))
        .map(|k| (k.0.clone(), k.1))
        .collect();

    // The rustc IR is compiled in ONE config (default features, no `--cfg test`),
    // so it omits `#[cfg(test)]` code; syn is cfg-blind and includes it. Split the
    // syn-only set on that axis so "precision" isn't dominated by test code rustc
    // correctly excluded. `::tests::`/`::test::` path segment ≈ a cfg(test) module.
    let (test_gated, mut real_over): (Vec<_>, Vec<_>) = syn_only
        .iter()
        .cloned()
        .partition(|(p, _)| looks_test_gated(p));

    let (r, s) = (rustc_defs.len(), syn_defs.len());
    let recall = pct(matched, r);
    let precision = pct(matched, s);
    // Precision with the cfg(test) inflation removed from syn's denominator —
    // the config-matched view (still an approximation; other cfgs may remain).
    let adj_precision = pct(matched, s - test_gated.len());
    let f1 = harmonic(recall, precision);
    let adj_f1 = harmonic(recall, adj_precision);
    let vis_pct = pct(vis_agree, matched.max(1));

    // ── report ──────────────────────────────────────────────────────────────
    println!("── Fidelity: rustc IR vs syn resolver — crate `{crate_code}` ──\n");
    println!("rustc IR total items:             {rustc_total}");
    println!("  ├─ comparable named defs:       {r}");
    println!(
        "  ├─ associated items (excluded): {rustc_assoc}   ← parent impl/trait; syn can't represent"
    );
    println!(
        "  ├─ fn-local defs (excluded):    {rustc_local}   ← parent fn/body; syn has no fn-body descent"
    );
    println!(
        "  ├─ harness synthetics (excl.):  {rustc_synth}   ← --test `main` + TestDescAndFn consts"
    );
    println!("  └─ modules (excluded):          {rustc_mods}");
    println!("syn comparable named defs:        {s}\n");

    println!("matched (path+kind):              {matched}");
    println!("rustc-only (syn MISSED):          {}", rustc_only.len());
    println!("syn-only total:                   {}", syn_only.len());
    println!(
        "  ├─ cfg(test)-gated (rustc omits): {}   ← config mismatch, not a syn error",
        test_gated.len()
    );
    println!("  └─ genuine over-report:           {}\n", real_over.len());

    println!("recall    (matched / rustc):      {recall:.1}%   ← syn's coverage of ground truth");
    println!(
        "precision (raw, matched / syn):   {precision:.1}%   ← deflated by cfg(test) inflation"
    );
    println!(
        "precision (config-matched):       {adj_precision:.1}%   ← cfg(test) removed from denominator"
    );
    println!("F1 (raw / config-matched):        {f1:.1}% / {adj_f1:.1}%");
    println!("visibility agreement (pub axis):  {vis_pct:.1}%  of matched\n");

    print_sample(
        "rustc-only (module-level defs syn genuinely MISSED — expect ~0 config-matched)",
        &mut rustc_only,
    );
    print_sample(
        "syn-only NOT under a `tests` module (triage: bare #[cfg(test)] fn or real FP)",
        &mut real_over,
    );
}

/// A path in a `tests`/`test` module — a proxy for `#[cfg(test)]` code the
/// single-config rustc build correctly omits. NOTE: only catches items inside a
/// `mod tests`; a bare `#[cfg(test)] fn` elsewhere slips through (verified: the 2
/// residuals here are exactly that). The rigorous fix is a config-matched rustc
/// build (`--cfg test`) rather than a path heuristic — see SPIKE §7/§10.
fn looks_test_gated(path: &str) -> bool {
    path.split("::").any(|seg| seg == "tests" || seg == "test")
}

fn harmonic(a: f64, b: f64) -> f64 {
    if a + b > 0.0 {
        2.0 * a * b / (a + b)
    } else {
        0.0
    }
}

fn print_sample(title: &str, items: &mut [(String, &str)]) {
    items.sort();
    let n = items.len();
    println!("▸ {title} — {n} total, first {}:", n.min(12));
    for (p, k) in items.iter().take(12) {
        println!("    {k:<7} {p}");
    }
    println!();
}

fn pct(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        100.0 * n as f64 / d as f64
    }
}

/// rustc kind string → shared vocab; `None` for non-comparable kinds (`mod`, …).
fn norm_rustc_kind(k: &str) -> Option<&'static str> {
    Some(match k {
        "struct" => "struct",
        "enum" => "enum",
        "union" => "union",
        "trait" => "trait",
        "trait_alias" => "trait_alias",
        "type" => "type",
        "fn" => "fn",
        "const" => "const",
        "static" => "static",
        "macro" => "macro",
        _ => return None,
    })
}

/// syn `ItemKind` → shared vocab (only `is_definition` kinds reach here).
fn norm_syn_kind(k: ItemKind) -> Option<&'static str> {
    Some(match k {
        ItemKind::Fn => "fn",
        ItemKind::Struct => "struct",
        ItemKind::Enum => "enum",
        ItemKind::Union => "union",
        ItemKind::Trait => "trait",
        ItemKind::TypeAlias => "type",
        ItemKind::Const => "const",
        ItemKind::Static => "static",
        ItemKind::Macro => "macro",
        _ => return None,
    })
}
