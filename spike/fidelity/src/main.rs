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
use std::ops::Range;
use std::path::{Path, PathBuf};

use syn_workspace::{ItemKind, TargetKind, Visibility as SynVis, Workspace};
use wl_ir::{IrFragment, Span as IrSpan, Visibility as IrVis};

/// Normalized definition set. Key = (canonical path, shared-vocab kind);
/// value = is-public (the coarse visibility axis we score).
type DefSet = BTreeMap<(String, &'static str), bool>;

/// rustc-side visibility detail for the vis-span differential: the emitted
/// visibility + the `pub`-token byte range (`None` when there's no editable
/// token). Keyed like [`DefSet`].
type RustcVis = BTreeMap<(String, &'static str), (IrVis, Option<IrSpan>)>;

/// syn-side visibility detail: syn's visibility + its `vis_byte_range` (only
/// `Some` for plain `pub`) + the absolute source file it lives in.
type SynVisMap = BTreeMap<(String, &'static str), (SynVis, Option<Range<u32>>, PathBuf)>;

fn main() {
    let mut args = std::env::args().skip(1);
    let repo = args.next().expect("arg1: repo root");
    let ir_json = args.next().expect("arg2: rustc IR json");
    let crate_code = args.next().unwrap_or_else(|| "syn_workspace".to_string());

    // ── rustc side (ground truth) ───────────────────────────────────────────
    let frag: IrFragment =
        serde_json::from_str(&std::fs::read_to_string(&ir_json).expect("read IR json"))
            .expect("parse IR json");
    frag.check_schema().expect("IR schema version");

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
    let mut rustc_vis: RustcVis = BTreeMap::new();
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
        rustc_vis.insert(
            (path.clone(), kind),
            (it.visibility.clone(), it.vis_span.clone()),
        );
        rustc_defs.insert((path, kind), is_pub);
    }

    // ── syn side (being retired) ────────────────────────────────────────────
    let ws = Workspace::load(&repo).expect("Workspace::load failed");
    let krate = ws
        .member_by_code_name(&crate_code)
        .unwrap_or_else(|| panic!("`{crate_code}` is not a workspace member"));

    let mut syn_defs: DefSet = BTreeMap::new();
    let mut syn_vis: SynVisMap = BTreeMap::new();
    for target in krate.targets.iter().filter(|t| t.kind == TargetKind::Lib) {
        for (_m, item) in target.root.walk_items() {
            if !item.kind.is_definition() {
                continue;
            }
            let Some(kind) = norm_syn_kind(item.kind) else {
                continue;
            };
            let is_pub = matches!(item.visibility, SynVis::Public);
            let key = (item.canonical.display(), kind);
            if let Some(src) = &item.source {
                syn_vis.insert(
                    key.clone(),
                    (item.visibility.clone(), item.vis_byte_range.clone(), src.file.clone()),
                );
            }
            syn_defs.insert(key, is_pub);
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

    compare_vis_spans(&repo, &rustc_vis, &syn_vis);
}

/// The WS1 `--fix` span-fidelity differential (SPIKE §12.7): for every def both
/// engines see, check that rustc's emitted `vis_span` (the tighten write
/// surface) byte-exactly matches syn's proven `vis_byte_range`, and that the
/// bytes under it are literally `pub`. Restricted-visibility spans
/// (`pub(crate)`/`pub(in …)`) are rustc-only — syn captures only plain `pub` —
/// so they're reported as *added* fidelity, not a mismatch.
fn compare_vis_spans(repo: &str, rustc: &RustcVis, syn: &SynVisMap) {
    let mut src = SrcCache::new(repo);
    let (mut both_present, mut byte_exact, mut text_pub) = (0usize, 0usize, 0usize);
    let (mut pub_crate, mut pub_in) = (0usize, 0usize);
    let mut mismatches: Vec<String> = Vec::new();

    for (key, (r_vis, r_span)) in rustc {
        let Some((s_vis, s_range, s_file)) = syn.get(key) else {
            continue; // only compare defs both engines see
        };

        // Plain `pub` on both sides: syn has a byte range here, rustc must too,
        // and they must agree to the byte.
        let syn_plain_pub = matches!(s_vis, SynVis::Public) && s_range.is_some();
        if syn_plain_pub {
            both_present += 1;
            let s_range = s_range.as_ref().unwrap();
            match r_span {
                None => mismatches.push(format!(
                    "{} [{}]: syn has vis range {}..{}, rustc vis_span = None",
                    key.0, key.1, s_range.start, s_range.end
                )),
                Some(rs) => {
                    // syn stores an absolute path; rustc a repo-relative one.
                    let files_ok = same_file(repo, s_file, &rs.file);
                    let bytes_ok = rs.lo == s_range.start && rs.hi == s_range.end;
                    if files_ok && bytes_ok {
                        byte_exact += 1;
                        let s_text = src.slice(&rs.file, rs.lo, rs.hi);
                        let syn_text = src.slice_abs(s_file, s_range.start, s_range.end);
                        if s_text.as_deref() == Some("pub") && syn_text.as_deref() == Some("pub") {
                            text_pub += 1;
                        } else {
                            mismatches.push(format!(
                                "{} [{}]: byte-exact but text rustc={:?} syn={:?} (expected \"pub\")",
                                key.0, key.1, s_text, syn_text
                            ));
                        }
                    } else {
                        mismatches.push(format!(
                            "{} [{}]: syn={}..{} @ {}  rustc={}..{} @ {}  (files_ok={files_ok})",
                            key.0,
                            key.1,
                            s_range.start,
                            s_range.end,
                            s_file.display(),
                            rs.lo,
                            rs.hi,
                            rs.file
                        ));
                    }
                }
            }
        } else if matches!(r_vis, IrVis::Restricted(_)) {
            // rustc-only restricted vis span — the fidelity syn can't capture.
            if let Some(rs) = r_span {
                match src.slice(&rs.file, rs.lo, rs.hi) {
                    Some(t) if t.starts_with("pub(crate)") => pub_crate += 1,
                    Some(t) if t.starts_with("pub(") => pub_in += 1,
                    other => mismatches.push(format!(
                        "{} [{}]: restricted vis_span text unexpected: {:?}",
                        key.0, key.1, other
                    )),
                }
            }
        }
    }

    println!("── vis-span differential: rustc vis_span vs syn vis_byte_range ──\n");
    println!("module-level defs both engines see (plain pub): {both_present}");
    println!(
        "  byte-exact (file, lo, hi):                    {byte_exact} / {both_present}   ← must be 100%"
    );
    println!(
        "  text == \"pub\" at range (both sides):          {text_pub} / {both_present}"
    );
    println!(
        "rustc-only restricted vis spans (syn can't):    {}   (pub(crate): {pub_crate}, pub(in …): {pub_in})",
        pub_crate + pub_in
    );
    println!("mismatches:                                     {}", mismatches.len());
    for m in mismatches.iter().take(20) {
        println!("    ✗ {m}");
    }
    if mismatches.is_empty() && byte_exact == both_present && text_pub == both_present {
        println!("\n✓ every shared plain-pub def has a byte-exact, `pub`-covering vis_span");
    }
}

/// Compare syn's absolute path against rustc's repo-relative one.
fn same_file(repo: &str, syn_abs: &Path, rustc_rel: &str) -> bool {
    let rustc_norm = rustc_rel.replace('\\', "/");
    if let Ok(stripped) = syn_abs.strip_prefix(repo) {
        if stripped.to_string_lossy().replace('\\', "/") == rustc_norm {
            return true;
        }
    }
    // Fall back to basename equality (robust to repo-prefix quirks).
    syn_abs.file_name().map(|n| n.to_string_lossy().into_owned())
        == Path::new(&rustc_norm).file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Reads source files once and slices byte ranges out of them.
struct SrcCache<'a> {
    repo: &'a str,
    files: BTreeMap<String, Option<Vec<u8>>>,
}

impl<'a> SrcCache<'a> {
    fn new(repo: &'a str) -> Self {
        Self { repo, files: BTreeMap::new() }
    }

    /// Slice a repo-relative file.
    fn slice(&mut self, rel: &str, lo: u32, hi: u32) -> Option<String> {
        let abs = if Path::new(rel).is_absolute() {
            PathBuf::from(rel)
        } else {
            Path::new(self.repo).join(rel)
        };
        self.slice_path(&abs, lo, hi)
    }

    /// Slice an absolute path.
    fn slice_abs(&mut self, abs: &Path, lo: u32, hi: u32) -> Option<String> {
        self.slice_path(abs, lo, hi)
    }

    fn slice_path(&mut self, abs: &Path, lo: u32, hi: u32) -> Option<String> {
        let key = abs.to_string_lossy().into_owned();
        let bytes = self
            .files
            .entry(key)
            .or_insert_with(|| std::fs::read(abs).ok());
        let bytes = bytes.as_ref()?;
        let (lo, hi) = (lo as usize, hi as usize);
        bytes.get(lo..hi).map(|b| String::from_utf8_lossy(b).into_owned())
    }
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
