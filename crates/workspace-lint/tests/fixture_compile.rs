//! Offline-compile gate for the semantic-lint fixture corpus.
//!
//! The semantic lints (`common::SEMANTIC_LINTS`: architecture, unused-deps,
//! unused-pub) are migrating onto a rustc-driver engine that COMPILES the
//! analyzed workspace, so every one of their `tests/cases/` fixture
//! workspaces must `cargo check` cleanly — and offline, because CI's cargo
//! cache only holds the root workspace's dependency tree. Fixture deps must
//! therefore be fixture-local `stubs/` path crates or crates from that tree.
//!
//! Gated behind `WL_FIXTURE_COMPILE=1` (mirroring wl-engine's `WL_ENGINE_E2E`
//! gate): the sweep cargo-checks ~100 workspaces, too slow for the default
//! `cargo test` loop. CI runs it on Linux in `.github/workflows/ci.yml`
//! after nextest (so the cargo cache is warm); locally:
//!
//! ```sh
//! WL_FIXTURE_COMPILE=1 cargo test -p workspace-lint --test fixture_compile -- --nocapture
//! ```

mod common;

use std::process::Command;
use std::time::Instant;

#[test]
fn semantic_fixture_workspaces_compile_offline() {
    if std::env::var_os("WL_FIXTURE_COMPILE").is_none() {
        eprintln!(
            "skipped: set WL_FIXTURE_COMPILE=1 (offline-compiles every semantic-lint \
             fixture workspace)"
        );
        return;
    }

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let scratch = tempfile::tempdir().expect("scratch tempdir");
    // One target dir PER workspace. Sharing one across them all is unsound:
    // cargo leaves a local path package's absolute path out of its metadata
    // hash (so a target dir stays relocatable), and the fixtures reuse package
    // names freely — 14 distinct `provider` crates, 29 distinct `alpha`. Two of
    // them therefore hash to the same `-C metadata` and write the same
    // `libprovider-<hash>.rmeta`; last writer wins, and a later fixture's
    // `consumer` links an earlier fixture's `provider` (`E0432`, pointing at a
    // file that plainly defines the missing item). Whether it bites depends on
    // build order and fingerprint freshness, so it presents as flakiness — and
    // it reproduced only on macOS, where `fs::copy` preserves mtime.
    // Isolation costs ~9s over the whole sweep; only a handful of fixtures pull
    // a registry dep, so there was little to amortize.
    let target_root = scratch.path().join("targets");

    let started = Instant::now();
    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();

    common::walk_cases(|lint, _kind, case_dir| {
        if !common::lint_needs_build(lint) {
            return;
        }
        checked += 1;
        // Check a copy, never the source tree: cargo writes Cargo.lock and
        // resolves paths, and the committed fixtures must stay build-free.
        let copy = scratch.path().join(format!("ws-{checked}"));
        if let Err(e) = common::copy_tree(&case_dir.join("workspace"), &copy) {
            failures.push(format!("{}\n    copy: {e}", case_dir.display()));
            return;
        }
        let result = Command::new(&cargo)
            .args(["check", "--offline", "--quiet"])
            .current_dir(&copy)
            .env(
                "CARGO_TARGET_DIR",
                target_root.join(format!("ws-{checked}")),
            )
            .env("CARGO_NET_OFFLINE", "true")
            .output();
        // Collect every failure (no fail-fast): one report showing the whole
        // corpus state beats re-running the sweep per broken fixture.
        match result {
            Ok(out) if out.status.success() => {}
            Ok(out) => failures.push(format!(
                "{}\n{}",
                case_dir.display(),
                String::from_utf8_lossy(&out.stderr).trim_end()
            )),
            Err(e) => failures.push(format!("{}\n    spawn cargo: {e}", case_dir.display())),
        }
    });

    // Mirror cases.rs: the semantic corpus is populated (~100 committed
    // workspaces), so zero discovered means the walk broke — fail loudly
    // rather than green-pass an empty sweep.
    assert!(
        checked > 0,
        "no semantic-lint fixture workspaces discovered — the discovery walk is broken"
    );
    eprintln!(
        "fixture_compile: {checked} semantic fixture workspace(s) checked offline in {:.1}s",
        started.elapsed().as_secs_f32()
    );
    assert!(
        failures.is_empty(),
        "{} of {checked} semantic fixture workspace(s) failed `cargo check --offline`:\n\n{}",
        failures.len(),
        failures.join("\n---\n")
    );
}
