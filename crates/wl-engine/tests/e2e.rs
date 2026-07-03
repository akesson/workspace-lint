//! End-to-end orchestration: vendored source → materialize → build the dylib
//! on the pinned nightly → extract via the embedded `dylint::run` → guard.
//!
//! Gated behind `WL_ENGINE_E2E=1` because it needs the pinned nightly
//! (+ rustc-dev + llvm-tools) and `dylint-link` installed — plain
//! `cargo test --workspace` must stay green on a stable-only machine. CI runs
//! it on Linux in `.github/workflows/extractor.yml`; locally:
//!
//! ```sh
//! WL_ENGINE_E2E=1 cargo test -p wl-engine --test e2e -- --nocapture
//! ```
//!
//! One `#[test]` only: `Engine::extract` documents process-global effects
//! (WL_IR_OUT + a scoped chdir), so it must not race a sibling test.

use wl_engine::{CfgSelector, Engine, EngineConfig, ExtractorSource, SemanticModel};

#[test]
fn vendored_extract_this_repo() {
    if std::env::var_os("WL_ENGINE_E2E").is_none() {
        eprintln!("skipped: set WL_ENGINE_E2E=1 (needs the pinned nightly + dylint-link)");
        return;
    }
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();

    // Vendored (production) source, materialized into a throwaway cache: this
    // exercises the full user-site path — including that the materialized
    // manifests are self-contained and the vendored lockfile builds --locked.
    let cache = tempfile::tempdir().unwrap();
    let ir_root = tempfile::tempdir().unwrap();
    let engine = Engine::new(ExtractorSource::Vendored {
        cache_root: cache.path().to_path_buf(),
    });
    let runs = engine
        .extract(&EngineConfig {
            workspace_root: repo_root.clone(),
            configs: vec![CfgSelector::default_cfg()],
            // One small crate: e2e proves the flow, not the fan-out (the
            // whole-workspace path is exercised daily by spike.yml + scripts).
            packages: vec!["wl-ir".into()],
            ir_root: ir_root.path().to_path_buf(),
        })
        .expect("extraction");

    assert_eq!(runs.runs.len(), 1);
    let frag_path = runs.runs[0].ir_dir.join("wl_ir.json");
    let frag: wl_ir::IrFragment =
        serde_json::from_str(&std::fs::read_to_string(&frag_path).unwrap()).unwrap();
    frag.check_schema().unwrap();
    assert_eq!(frag.crate_name, "wl_ir");
    assert!(
        frag.items
            .iter()
            .any(|i| i.path.last().map(String::as_str) == Some("IrFragment")),
        "extracted fragment should contain wl-ir's own IrFragment def"
    );

    // Phase 2 over the real extraction (the orchestrate→semantic seam):
    // assembly succeeds, and wl-ir's schema types read as published API
    // surface (its crate declares publish intent), never as hard-dead.
    let model = SemanticModel::load(&runs, &repo_root).expect("assemble");
    assert_eq!(model.config_ids().collect::<Vec<_>>(), ["default"]);
    let verdict = model.union_verdict();
    assert!(
        verdict.leads.iter().all(|l| !l.dead),
        "wl-ir is a publishable lib — unused pub API must classify as surface, not dead: {:?}",
        verdict.leads
    );
}
