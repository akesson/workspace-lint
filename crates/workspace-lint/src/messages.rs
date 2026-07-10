//! Canonical scenarios for every diagnostic workspace-lint emits, plus inline
//! snapshot tests in human, JSON, and GitHub Actions formats.
//!
//! **Read this file top-to-bottom to validate every user-facing message the
//! tool can produce.** The body of each `#[test]` contains the rendered
//! output as a literal string; `cargo insta review` keeps them honest as the
//! code evolves.
//!
//! Three sub-modules — one per renderer — each named after the lint and
//! scenario it exercises:
//!
//! - `human` : clippy-style text written to stderr.
//! - `json` : `--message-format=json`, rustc-compatible per-line records.
//! - `github` : `--message-format=github`, Actions workflow command.
//!
//! Add a new scenario by extending [`scenarios`] with one builder per case,
//! then re-running `cargo test` — insta will prompt you to accept the new
//! snapshot lines.

use std::path::PathBuf;

use wl_diagnostic::builder::{at_crate, at_file, at_line, at_workspace};
use wl_diagnostic::{Applicability, Diagnostic, Span, Suggestion};

/// Every distinct diagnostic the tool can emit, in a fixed order. The order
/// here is what the snapshot tests assert against, so think of this as the
/// canonical user-facing surface.
///
/// NOTE: these scenarios are hand-built `Diagnostic`s — they exercise the
/// three *renderers* (human/json/github), not the lints' own logic. The real
/// message strings a lint emits are pinned only where a `tests/cases/` fixture
/// runs it end-to-end; keep the strings here in sync with the lint by hand.
///
/// Test-only: consumed by the snapshot tests below and `messages_quality.rs`.
#[allow(dead_code)]
pub(crate) fn scenarios() -> Vec<(&'static str, Diagnostic)> {
    vec![
        // centralized-deps: one offending member crate.
        (
            "centralized_deps_one_dep",
            at_crate(
                "workspace-lint::centralized-deps",
                "1 dependency in crates/alpha/Cargo.toml should use `workspace = true`",
                PathBuf::from("crates/alpha"),
            )
            .help(
                r#"[dependencies] serde: has own version "1.0.200" — use { workspace = true } instead"#,
            )
            .build(),
        ),
        // centralized-deps: the two-file fix — the dep is missing from
        // [workspace.dependencies] and every member agrees on the version, so
        // one suggestion seeds the workspace table (a pure insertion) and the
        // member rewrite machine-applies in the same run.
        (
            "centralized_deps_workspace_seed",
            at_crate(
                "workspace-lint::centralized-deps",
                "1 dependency in crates/alpha/Cargo.toml should use `workspace = true`",
                PathBuf::from("crates/alpha"),
            )
            .help(
                r#"[dependencies] rand: version "0.8" not in [workspace.dependencies] — add it there and use { workspace = true }"#,
            )
            .suggestion(Suggestion {
                span: Span {
                    file: PathBuf::from("Cargo.toml"),
                    line_start: 5,
                    line_end: 5,
                    col_start: 1,
                    col_end: 1,
                    byte_start: 60,
                    byte_end: 60,
                },
                message: "add `rand` to [workspace.dependencies]".into(),
                replacement: "rand = \"0.8\"\n".into(),
                applicability: Applicability::MachineApplicable,
                original: None,
            })
            .suggestion(Suggestion {
                span: Span {
                    file: PathBuf::from("crates/alpha/Cargo.toml"),
                    line_start: 7,
                    line_end: 7,
                    col_start: 1,
                    col_end: 1,
                    byte_start: 90,
                    byte_end: 102,
                },
                message: "use { workspace = true } for `rand`".into(),
                replacement: "rand = { workspace = true }".into(),
                applicability: Applicability::MachineApplicable,
                original: Some("rand = \"0.8\"".into()),
            })
            .build(),
        ),
        // centralized-deps: the absent-table shape of the two-file fix — no
        // [workspace.dependencies] exists, so ONE suggestion creates it with
        // every agreed entry (per-dep insertions would each carry their own
        // duplicate header — cargo rejects the manifest). Entries whose
        // members declare `default-features = false` carry the flag into the
        // created table: cargo resolves features from the workspace side.
        (
            "centralized_deps_table_creation",
            at_crate(
                "workspace-lint::centralized-deps",
                "2 dependencies in crates/alpha/Cargo.toml should use `workspace = true`",
                PathBuf::from("crates/alpha"),
            )
            .help(
                r#"[dependencies] log: version "0.4" not in [workspace.dependencies] — add it there and use { workspace = true }"#,
            )
            .help(
                r#"[dependencies] serde: version "1" not in [workspace.dependencies] — add it there and use { workspace = true }"#,
            )
            .suggestion(Suggestion {
                span: Span {
                    file: PathBuf::from("Cargo.toml"),
                    line_start: 3,
                    line_end: 3,
                    col_start: 1,
                    col_end: 1,
                    byte_start: 55,
                    byte_end: 55,
                },
                message: "create [workspace.dependencies] with `log`, `serde`".into(),
                replacement:
                    "\n[workspace.dependencies]\nlog = { version = \"0.4\", default-features = false }\nserde = \"1\"\n"
                        .into(),
                applicability: Applicability::MachineApplicable,
                original: None,
            })
            .build(),
        ),
        // centralized-deps: multiple offending deps in one crate.
        (
            "centralized_deps_multiple_deps",
            at_crate(
                "workspace-lint::centralized-deps",
                "2 dependencies in crates/beta/Cargo.toml should use `workspace = true`",
                PathBuf::from("crates/beta"),
            )
            .help(r#"[dependencies] serde: version "1.0" not in [workspace.dependencies]"#)
            .help(r#"[dev-dependencies] rand: version "0.8" not in [workspace.dependencies]"#)
            .build(),
        ),
        // file-size: single file over limit.
        (
            "file_size_over_limit",
            at_file(
                "workspace-lint::file-size",
                "file exceeds 500 code lines (612)",
                PathBuf::from("crates/web-api/src/handler.rs"),
            )
            .help("split this file into focused submodules (e.g. a `foo/` directory with a `mod.rs`)")
            .help("extract related structs, enums, or trait impls into their own modules")
            .help("only shipped source counts — `#[cfg(test)]` and `#[test]` code is already excluded")
            .note(r#"configured by [[file-size.rules]] glob = "**/*.rs""#)
            .build(),
        ),
        // crate-size: one crate over limit.
        (
            "crate_size_over_limit",
            at_crate(
                "workspace-lint::crate-size",
                "crate exceeds 5000 code lines (7321)",
                PathBuf::from("crates/legacy"),
            )
            .help("split the crate into smaller, more focused crates")
            .note(r#"configured by [[crate-size.rules]] glob = "crates/*""#)
            .build(),
        ),
        // duplicate-code: one diagnostic per clone group, anchored at its
        // first instance, the other sites cross-referenced in the note and
        // the extraction priced by the literal-divergence pass.
        (
            "duplicate_code_group",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicated code: 3 structurally identical instances (~14 lines)",
                PathBuf::from("crates/alpha/src/report.rs"),
                42,
            )
            .note("also found at: crates/beta/src/render.rs:88, crates/gamma/src/emit.rs:17")
            .note_once("matching ignores local variable names and literal values")
            .note("extracting would take ~2 parameters for the differing literals")
            .help("extract the shared logic into one function the copies can call")
            .build(),
        ),
        // duplicate-code drift: one instance breaks an otherwise consistent
        // literal mapping — the probable-bug case that bypasses the
        // max-parameters gate; the defecting site is named.
        (
            "duplicate_code_drift",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicated code: 2 structurally identical instances (~12 lines)",
                PathBuf::from("crates/alpha/src/report.rs"),
                42,
            )
            .note("also found at: crates/beta/src/render.rs:88")
            .note_once("matching ignores local variable names and literal values")
            .note(
                "possible copy-paste drift: crates/beta/src/render.rs:96 has \"alpha\" \
                 where the mapping elsewhere expects \"beta\"",
            )
            .help("extract the shared logic into one function the copies can call")
            .build(),
        ),
        // duplicate-code classifier: identical fns the call graph confirms are
        // interchangeable — merge and redirect the callers (the call-sites note
        // is the outside referrer count the merge would move).
        (
            "duplicate_code_merge_identical_fns",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicated code: 2 structurally identical instances (~8 lines)",
                PathBuf::from("crates/alpha/src/report.rs"),
                42,
            )
            .note("also found at: crates/beta/src/render.rs:88")
            .note_once("matching ignores local variable names and literal values")
            .help(
                "these are copies of the same function — keep one and redirect the other call sites",
            )
            .note("instances are identical (differing at most in local names)")
            .note("2 call sites reference the copies (first at crates/alpha/src/main.rs:15)")
            .build(),
        ),
        // duplicate-code classifier: identical fns where some copy has no
        // referrer at all — delete the dead one rather than merge.
        (
            "duplicate_code_delete_dead_copy",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicated code: 2 structurally identical instances (~8 lines)",
                PathBuf::from("crates/alpha/src/report.rs"),
                42,
            )
            .note("also found at: crates/beta/src/render.rs:88")
            .note_once("matching ignores local variable names and literal values")
            .help("the copy at crates/beta/src/render.rs:88 is never referenced — delete it")
            .note("instances are identical (differing at most in local names)")
            .build(),
        ),
        // duplicate-code classifier: the same method across impls of one trait —
        // hoist it to a default method on the trait.
        (
            "duplicate_code_default_trait_method",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicated code: 2 structurally identical instances (~6 lines)",
                PathBuf::from("crates/alpha/src/report.rs"),
                42,
            )
            .note("also found at: crates/beta/src/render.rs:88")
            .note_once("matching ignores local variable names and literal values")
            .help("every copy implements `Formatter::render` — make it a default method on the trait")
            .note("instances are identical (differing at most in local names)")
            .build(),
        ),
        // duplicate-code classifier: free/inherent fns all taking one workspace
        // type first — extract a method on that type.
        (
            "duplicate_code_method_on_receiver_type",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicated code: 2 structurally identical instances (~5 lines)",
                PathBuf::from("crates/alpha/src/report.rs"),
                42,
            )
            .note("also found at: crates/beta/src/render.rs:88")
            .note_once("matching ignores local variable names and literal values")
            .help(
                "extract the shared logic into a method on `Config` — every copy takes it as the first parameter",
            )
            .note("extracting would take ~1 parameter for the differing literals")
            .build(),
        ),
        // duplicate-code classifier: copies built from the same UI macro —
        // extract one component the copies can render.
        (
            "duplicate_code_ui_component",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicated code: 2 structurally identical instances (~4 lines)",
                PathBuf::from("crates/alpha/src/report.rs"),
                42,
            )
            .note("also found at: crates/beta/src/render.rs:88")
            .note_once("matching ignores local variable names and literal values")
            .help("extract the shared `rsx!` markup into one component the copies can render")
            .note("extracting would take ~1 parameter for the differing literals")
            .build(),
        ),
        // duplicate-code classifier: identical tokens but the instances resolve
        // different callees (a local shadows a same-named fn) — the merge is
        // withheld and the generic help stands, with a caution note.
        (
            "duplicate_code_merge_withheld",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicated code: 2 structurally identical instances (~6 lines)",
                PathBuf::from("crates/alpha/src/report.rs"),
                42,
            )
            .note("also found at: crates/beta/src/render.rs:88")
            .note_once("matching ignores local variable names and literal values")
            .help("extract the shared logic into one function the copies can call")
            .note("instances are identical (differing at most in local names)")
            .note("instances resolve different callees — the copies may not be interchangeable")
            .build(),
        ),
        // duplicate-code statement run: a mid-body run priced as an extraction
        // signature — the live-in variables become parameters, the one live-out
        // variable the return value.
        (
            "duplicate_code_run_signature",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicated code: 2 structurally identical instances (~6 lines)",
                PathBuf::from("crates/alpha/src/report.rs"),
                42,
            )
            .note("also found at: crates/beta/src/render.rs:88")
            .note_once("matching ignores local variable names and literal values")
            .help("extract the shared logic into one function the copies can call")
            .note("instances are identical (differing at most in local names)")
            .note("an extracted fn would take 2 parameters (items, config) and return total")
            .build(),
        ),
        // duplicate-code statement run needing several return values: extracting
        // it would return a tuple the caller must destructure, so the finding is
        // downgraded to a lint-chosen `warn` (level_explicit) and the note says
        // so.
        (
            "duplicate_code_run_live_out_downgrade",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicated code: 2 structurally identical instances (~7 lines)",
                PathBuf::from("crates/alpha/src/report.rs"),
                42,
            )
            .note("also found at: crates/beta/src/render.rs:88")
            .note_once("matching ignores local variable names and literal values")
            .help("extract the shared logic into one function the copies can call")
            .note("instances are identical (differing at most in local names)")
            .note(
                "an extracted fn would take 1 parameter (items) but needs 3 return values \
                 (count, total, errors) — extraction is awkward; consider restructuring",
            )
            .level_explicit(wl_diagnostic::Level::Warn)
            .build(),
        ),
        // duplicate-code baseline: a baselined group that outgrew its accepted
        // instance count still fires, with a note flagging the new copy.
        (
            "duplicate_code_baseline_grew",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicated code: 3 structurally identical instances (~8 lines)",
                PathBuf::from("crates/alpha/src/report.rs"),
                42,
            )
            .note("also found at: crates/beta/src/render.rs:88, crates/gamma/src/emit.rs:17")
            .note_once("matching ignores local variable names and literal values")
            .note("grew beyond its baseline: 2 instances accepted, now 3")
            .note("instances are identical (differing at most in local names)")
            .help("extract the shared logic into one function the copies can call")
            .build(),
        ),
        // duplicate-code baseline: an entry no current group matches — the
        // clone was fixed — reported so the ratchet can only tighten. Anchored
        // at the entry's line in the baseline file.
        (
            "duplicate_code_baseline_stale",
            at_line(
                "workspace-lint::duplicate-code",
                "stale duplicate-code baseline entry: no clone group matches fingerprint \
                 9930bf3835a56614 (was crates/alpha/src/report.rs, 2 instances)",
                PathBuf::from("duplicate-code.baseline.toml"),
                8,
            )
            .note_once("the duplication was resolved, or the code changed enough to re-fingerprint")
            .help("regenerate with `workspace-lint --baseline-write` (or delete this entry)")
            .build(),
        ),
        // duplicate-code baseline: an entry recording more instances than
        // remain — a partial fix — reported so the count can be ratcheted down.
        (
            "duplicate_code_baseline_overcount",
            at_line(
                "workspace-lint::duplicate-code",
                "duplicate-code baseline entry 9930bf3835a56614 records 3 instances but only 2 remain",
                PathBuf::from("duplicate-code.baseline.toml"),
                8,
            )
            .help("ratchet down: regenerate with `workspace-lint --baseline-write`")
            .build(),
        ),
        // duplicate-code baseline: the configured file is absent — reported as
        // the only finding (the run isn't judged against a missing record).
        (
            "duplicate_code_baseline_missing",
            at_file(
                "workspace-lint::duplicate-code",
                "duplicate-code baseline file `duplicate-code.baseline.toml` not found",
                PathBuf::from("duplicate-code.baseline.toml"),
            )
            .help(
                "generate it with `workspace-lint --baseline-write`, or remove `baseline` \
                 from [duplicate-code]",
            )
            .build(),
        ),
        // cli-crate-version: version mismatch.
        (
            "cli_crate_version_mismatch",
            at_workspace(
                "workspace-lint::cli-crate-version",
                "`wasm-bindgen` CLI version 0.2.89 does not match Cargo.lock 0.2.90",
            )
            .help("update or reinstall `wasm-bindgen` to match the workspace version")
            .note("ran `wasm-bindgen --version`")
            .build(),
        ),
        // cli-crate-version: a misconfigured / un-runnable rule reported as a
        // diagnostic instead of aborting the whole run.
        (
            "cli_crate_version_rule_error",
            at_workspace(
                "workspace-lint::cli-crate-version",
                "pattern `v(\\d+)` did not match the output of `wasm-bindgen --version`",
            )
            .help("the regex must capture the version in group 1")
            .note("ran `wasm-bindgen --version`")
            .build(),
        ),
        // unused-deps: single unused dep.
        (
            "unused_deps_one",
            at_crate(
                "workspace-lint::unused-deps",
                "1 possibly unused dependency in crates/alpha/Cargo.toml",
                PathBuf::from("crates/alpha"),
            )
            .help("[dependencies] rand")
            .note_once("build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives")
            .note_once("verify by removing the dep and running `cargo build --all-targets`")
            .note_once("if the build breaks, add the dep to [unused-deps] ignore in your config")
            .build(),
        ),
        // unused-deps: multiple unused deps.
        (
            "unused_deps_multiple",
            at_crate(
                "workspace-lint::unused-deps",
                "2 possibly unused dependencies in crates/beta/Cargo.toml",
                PathBuf::from("crates/beta"),
            )
            .help("[dependencies] foo")
            .help("[dev-dependencies] bar")
            .note_once("build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives")
            .note_once("verify by removing the dep and running `cargo build --all-targets`")
            .note_once("if the build breaks, add the dep to [unused-deps] ignore in your config")
            .build(),
        ),
        // unused-deps: the manifest has uncommitted changes, so the dep-line
        // deletion is withheld by the per-file git gate (`--fix` counts it as
        // withheld and this note says why).
        (
            "unused_deps_dirty_manifest",
            at_crate(
                "workspace-lint::unused-deps",
                "1 possibly unused dependency in crates/alpha/Cargo.toml",
                PathBuf::from("crates/alpha"),
            )
            .help("[dependencies] rand")
            .note_once("file `crates/alpha/Cargo.toml` is untracked or has uncommitted changes; `--fix-auto-delete` will not delete it (commit first or use `git stash`)")
            .note_once("build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives")
            .note_once("verify by removing the dep and running `cargo build --all-targets`")
            .note_once("if the build breaks, add the dep to [unused-deps] ignore in your config")
            .build(),
        ),
        // unused-deps: dep removal is a deletion, so plain `--fix` withholds
        // it (--fix-auto-delete only); the note names the flag and the hedge.
        (
            "unused_deps_removal_withheld",
            at_crate(
                "workspace-lint::unused-deps",
                "1 possibly unused dependency in crates/alpha/Cargo.toml",
                PathBuf::from("crates/alpha"),
            )
            .help("[dependencies] rand")
            .note_once("not auto-applied: removing a dependency is `--fix-auto-delete` only — the verdict is \"possibly unused\"; verify before deleting")
            .note_once("build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives")
            .note_once("verify by removing the dep and running `cargo build --all-targets`")
            .note_once("if the build breaks, add the dep to [unused-deps] ignore in your config")
            .build(),
        ),
        // unused-deps: a member no [engine] config compiled. Its deps produced
        // zero fragments, so they are unjudgeable — surfaced as a coverage note
        // pinned to `warn` (level_explicit, so `unused-deps = "deny"` can't turn
        // a coverage gap into a build failure), never flagged as unused.
        (
            "unused_deps_not_compiled",
            at_crate(
                "workspace-lint::unused-deps",
                "2 dependencies of `gamma` could not be checked in crates/gamma/Cargo.toml",
                PathBuf::from("crates/gamma"),
            )
            .help("[dependencies] foo")
            .help("[dependencies] bar")
            .help("compile it under an [engine] config (e.g. \"cargo build --target <triple> -p gamma\"), or add these deps to [unused-deps] ignore")
            .note_once("this crate produced no compiler output under the current [engine] config matrix")
            .level_explicit(wl_diagnostic::Level::Warn)
            .build(),
        ),
        // unused-pub: removal candidate (appears unused entirely).
        (
            "unused_pub_removal_candidate",
            at_line(
                "workspace-lint::unused-pub",
                "pub fn `helper` in crate `mycrate` appears unused — consider removing",
                PathBuf::from("crates/mycrate/src/lib.rs"),
                42,
            )
            .help("remove the item or its `pub` visibility")
            .note_once(
                "code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives",
            )
            .note(
                "not auto-applied: deleting an unused item is `--fix-auto-delete` only — verify it is truly unused, then delete it or narrow by hand",
            )
            .build(),
        ),
        // unused-pub: unused in every DECLARED config, but mentioned inside a
        // cfg region no config compiles — the specific blind spot replaces the
        // generic disclaimer, and `--fix-auto-delete` vetoes the deletion with
        // the same information.
        (
            "unused_pub_cfg_shadowed",
            at_line(
                "workspace-lint::unused-pub",
                "pub fn `tz_offset_minutes` in crate `utils` appears unused — consider removing",
                PathBuf::from("crates/utils/src/lib.rs"),
                9,
            )
            .help("remove the item or its `pub` visibility")
            .note(
                "possibly used: mentioned under `cfg(target_arch = \"wasm32\")` (crates/app/src/main.rs), which no declared `[engine]` config compiles — add a matching cargo command to `[engine] configs` to judge that code",
            )
            .note(
                "not auto-applied: deleting an unused item is `--fix-auto-delete` only — verify it is truly unused, then delete it or narrow by hand",
            )
            .build(),
        ),
        // unused-pub: reached only from test code (any crate) — the
        // dead-family verdict between "unused" and "intra-crate": nothing
        // production reaches it, so neither a tighten (trips `dead_code` on
        // the plain build) nor a bare deletion (orphans the referencing
        // tests) can be machine-applied.
        (
            "unused_pub_test_only",
            at_line(
                "workspace-lint::unused-pub",
                "pub fn `helper` in crate `mycrate` is only used by test code",
                PathBuf::from("crates/mycrate/src/lib.rs"),
                42,
            )
            .help("gate it `#[cfg(test)]`, move it into test code, or remove it")
            .note(
                "code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives",
            )
            .note(
                "no fix is auto-applied: `pub(crate)` would trip `dead_code` on the non-test build, and deleting the item would break the tests that reference it",
            )
            .build(),
        ),
        // unused-pub: a TestOnly target whose deletion `--fix-auto-delete`
        // vetoed — the referencing test also exercises surviving code, so it
        // is not exclusive scaffolding and the target stays (the deletion is
        // downgraded; this note is the veto's rendering).
        (
            "unused_pub_test_only_blocked",
            at_line(
                "workspace-lint::unused-pub",
                "pub fn `embalmed` in crate `alpha` is only used by test code",
                PathBuf::from("crates/alpha/src/lib.rs"),
                6,
            )
            .help("gate it `#[cfg(test)]`, move it into test code, or remove it")
            .note(
                "code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives",
            )
            .note(
                "only test code references it, but test item `beta::tests::covers_both` (crates/beta/src/main.rs:8) also exercises surviving `alpha::kept` — deleting would orphan that test; update or remove the test first, or delete both by hand",
            )
            .build(),
        ),
        // unused-pub: an exclusively-scaffolding test item deleted alongside
        // its TestOnly target by `--fix-auto-delete` (the counterpart of the
        // private-collateral finding, on the test side of the boundary).
        (
            "unused_pub_test_scaffold",
            at_line(
                "workspace-lint::unused-pub",
                "test fn `exercises_embalmed` in crate `beta` only exercises items deleted by this `--fix`",
                PathBuf::from("crates/beta/src/main.rs"),
                12,
            )
            .help("deleting it too — it would reference deleted items and break the test build")
            .note(
                "exclusive test scaffolding: every workspace item it references is also deleted by this `--fix`",
            )
            .build(),
        ),
        // unused-pub: same-crate-only — suggest pub(crate).
        (
            "unused_pub_tighten_visibility",
            at_line(
                "workspace-lint::unused-pub",
                "pub struct `Builder` in crate `mycrate` is only used inside the crate",
                PathBuf::from("crates/mycrate/src/builder.rs"),
                7,
            )
            .help("consider `pub(crate)` to tighten visibility")
            .note_once(
                "code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives",
            )
            .build(),
        ),
        // unused-pub: an item the one-pass `--fix` cascade freed — something
        // *does* reference it in source, but that referrer is deleted in the
        // same pass, so it becomes unused. The extra note distinguishes this
        // from a directly-dead item.
        (
            "unused_pub_cascade_transitive",
            at_line(
                "workspace-lint::unused-pub",
                "pub fn `helper` in crate `mycrate` appears unused — consider removing",
                PathBuf::from("crates/mycrate/src/inner.rs"),
                14,
            )
            .help("remove the item or its `pub` visibility")
            .note_once(
                "code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives",
            )
            .note(
                "transitively unused: the only item(s) that referenced it are also deleted by this `--fix`",
            )
            .build(),
        ),
        // unused-pub: a deletion the unmask guard vetoed, field flavor — the
        // item holds the last READ of a surviving type's field, so deleting it
        // would trip rustc `dead_code` on the fixed tree.
        (
            "unused_pub_delete_unmask_field",
            at_line(
                "workspace-lint::unused-pub",
                "pub fn `open_state` in crate `widgets` appears unused — consider removing",
                PathBuf::from("crates/widgets/src/lib.rs"),
                13,
            )
            .help("remove the item or its `pub` visibility")
            .note(
                "deleting this would leave field `open` of surviving `widgets::Panel` never-read, tripping `dead_code` on the fixed tree — remove the field first or delete by hand",
            )
            .build(),
        ),
        // unused-pub: a deletion the unmask guard vetoed, clippy flavor —
        // removing `is_empty` out from under a surviving `len`.
        (
            "unused_pub_delete_unmask_len",
            at_line(
                "workspace-lint::unused-pub",
                "pub fn `is_empty` in crate `store` appears unused — consider removing",
                PathBuf::from("crates/store/src/buf.rs"),
                17,
            )
            .help("remove the item or its `pub` visibility")
            .note(
                "deleting `is_empty` would trip clippy `len_without_is_empty` on `store::Buf`'s surviving `len` — remove or keep the pair together",
            )
            .build(),
        ),
        // unused-pub: the dangling-`use` surgery the cascade emits — a deleted
        // item's import leaf is excised so the tree still compiles (no E0432).
        (
            "unused_pub_import_surgery",
            at_line(
                "workspace-lint::unused-pub",
                "unused import of a removed item",
                PathBuf::from("crates/mycrate/src/lib.rs"),
                3,
            )
            .help("removing the dangling `use` left by the deleted item")
            .suggestion(Suggestion {
                span: Span {
                    file: PathBuf::from("crates/mycrate/src/lib.rs"),
                    line_start: 3,
                    line_end: 3,
                    col_start: 1,
                    col_end: 1,
                    byte_start: 12,
                    byte_end: 20,
                },
                message: "remove the unused import".to_string(),
                replacement: String::new(),
                applicability: Applicability::MachineApplicable,
                original: Some("helper, ".to_string()),
            })
            .build(),
        ),
        // unused-pub: a tighten the clippy-unmask guard downgraded — narrowing
        // would strip clippy's `avoid-breaking-exported-api` exemption and a
        // style lint would fire on the fixed tree, so the fix is shown but
        // never machine-applied.
        (
            "unused_pub_tighten_unmask",
            at_line(
                "workspace-lint::unused-pub",
                "pub struct `Rect` in crate `mycrate` is only used inside the crate",
                PathBuf::from("crates/mycrate/src/geometry.rs"),
                11,
            )
            .help("consider `pub(crate)` to tighten visibility")
            .note_once(
                "code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives",
            )
            .note(
                "`pub(crate)` would unmask clippy `wrong_self_convention` on `is_wide` (clippy exempts exported items via `avoid-breaking-exported-api`) — resolve that first or narrow by hand",
            )
            .build(),
        ),
        // unused-pub: private collateral — a private helper whose last user
        // is deleted in the same `--fix` pass (rustc `dead_code` would flag
        // it on the fixed tree), deleted alongside its users.
        (
            "unused_pub_private_collateral",
            at_line(
                "workspace-lint::unused-pub",
                "private fn `helper` in crate `mycrate` loses its last user in this `--fix`",
                PathBuf::from("crates/mycrate/src/lib.rs"),
                21,
            )
            .help("deleting it too — rustc `dead_code` would flag it on the fixed tree")
            .note(
                "transitively dead: the only item(s) that referenced it are also deleted by this `--fix`",
            )
            .build(),
        ),
        // unused-pub: crate-level hint nudging `publish = true` for an internal
        // crate that accumulated several findings.
        (
            "unused_pub_publish_hint",
            at_crate(
                "workspace-lint::unused-pub",
                "crate `mycrate` has 3 public items unused within the workspace",
                PathBuf::from("crates/mycrate"),
            )
            .help(
                "if `mycrate` is published outside this workspace, set `publish = true` in its Cargo.toml to treat its public API as external (these findings become exempt)",
            )
            .note_once(
                "workspace-lint treats a crate as workspace-internal unless it declares `publish = true` (or a registry); see the unused-pub docs",
            )
            .build(),
        ),
        // stale-expect: an `expect!` directive that didn't fire. Carries a
        // MachineApplicable whole-line deletion (`--fix` removes the directive).
        (
            "stale_expect",
            at_line(
                "workspace-lint::stale-expect",
                "expect directive for `file-size` did not match any diagnostic",
                PathBuf::from("crates/api/src/lib.rs"),
                1,
            )
            .help("remove this expect — the lint it tracks is no longer firing")
            .note_once("a stale expect usually means the underlying issue has been fixed")
            .suggestion(Suggestion {
                span: Span {
                    file: PathBuf::from("crates/api/src/lib.rs"),
                    line_start: 1,
                    line_end: 1,
                    col_start: 1,
                    col_end: 1,
                    byte_start: 0,
                    byte_end: 36,
                },
                message: "remove this stale expect directive".to_string(),
                replacement: String::new(),
                applicability: Applicability::MachineApplicable,
                original: Some("workspace_lint::expect!(file_size);".to_string()),
            })
            .build(),
        ),
        // architecture: a denied `use` violates a configured rule. Anchored
        // at the offending `use` line via `UseBinding::source` (added in
        // syn-workspace 0.4.0); the previous "imported in module …" note
        // is redundant once the diagnostic points at the line itself.
        (
            "architecture_denied_import",
            at_line(
                "workspace-lint::architecture",
                "import of `data_models::internal::User` from `apps-foo` violates architecture rule `no-internal-imports`",
                PathBuf::from("crates/apps-foo/src/lib.rs"),
                7,
            )
            .help("import from `data-models::api` instead")
            .note_once("internal types are not part of the published API surface")
            .build(),
        ),
        // architecture: the same rule violated by a *fully-qualified* reference
        // (no `use`) — `data_models::internal::User::new()`. Distinct verb
        // ("reference to" vs "import of"); anchored at the call line.
        (
            "architecture_denied_code_reference",
            at_line(
                "workspace-lint::architecture",
                "reference to `data_models::internal::User` from `apps-foo` violates architecture rule `no-internal-imports`",
                PathBuf::from("crates/apps-foo/src/lib.rs"),
                12,
            )
            .help("import from `data-models::api` instead")
            .note_once("internal types are not part of the published API surface")
            .build(),
        ),
        // orphan-file: no config compiled it and nothing names it — safe to delete.
        (
            "orphan_file_orphan",
            at_file(
                "workspace-lint::orphan-file",
                "orphan source file `src/orphan.rs` is never compiled",
                PathBuf::from("crates/demo/src/orphan.rs"),
            )
            .help(
                "delete the file, or reach it: add `mod orphan;` (or `#[path = \"src/orphan.rs\"] mod ...;`) in the appropriate parent module",
            )
            .note("no `[engine]` config compiled it, and nothing in crate `demo`'s source names it")
            .build(),
        ),
        // orphan-file: the source names it, but the config matrix never opens it.
        (
            "orphan_file_cfg_coverage_gap",
            at_file(
                "workspace-lint::orphan-file",
                "no declared `[engine]` config compiles `src/imp_windows.rs`",
                PathBuf::from("crates/demo/src/imp_windows.rs"),
            )
            .help(
                "add a config that compiles it — a `--target` for a platform-gated module, or `\"cargo test\"` for a `#[cfg(test)]` one",
            )
            .note("crate `demo`'s source names this file, so it is not reported as an orphan — but the declared config (default) never opened it, so nothing in it is checked")
            .level_explicit(wl_diagnostic::Level::Warn)
            .build(),
        ),
        // feature-drift: a declared feature never appears in `#[cfg]`.
        (
            "feature_drift_declared_never_gated",
            at_crate(
                "workspace-lint::feature-drift",
                "feature `experimental` is declared in `[features]` but never gated in source",
                PathBuf::from("crates/demo"),
            )
            .help(
                "either gate code with `#[cfg(feature = \"experimental\")]` or remove `experimental` from `[features]`",
            )
            .note("declared in `demo/Cargo.toml`")
            .build(),
        ),
        // feature-drift: a `#[cfg(feature = "…")]` references a feature
        // missing from `[features]`.
        (
            "feature_drift_gated_undeclared",
            at_crate(
                "workspace-lint::feature-drift",
                "feature `nightly` is gated in source but not declared in `[features]`",
                PathBuf::from("crates/demo"),
            )
            .help(
                "add `nightly = []` to the `[features]` table of `demo/Cargo.toml`, or remove the `cfg(feature = \"nightly\")` references",
            )
            .build(),
        ),
        // config: an unknown key in the config file.
        (
            "config_unknown_key",
            at_file(
                "workspace-lint::config",
                "unknown configuration section `file-siz`",
                PathBuf::from(".workspace-lint.toml"),
            )
            .help("did you mean `file-size`?")
            .build(),
        ),
        // config: the retired `[macros]` expansion-uses surface — the rustc
        // engine sees macro expansions natively.
        (
            "config_macros_deprecated",
            at_file(
                "workspace-lint::config",
                "`[macros]` is obsolete: the engine sees macro expansions natively",
                PathBuf::from(".workspace-lint.toml"),
            )
            .help(
                "delete the `[macros]` section; `expansion_uses!` annotations and \
                 `# workspace-lint: expansion-uses(...)` comments are no longer read",
            )
            .build(),
        ),
        // unknown-lint: a referenced lint name that doesn't exist.
        (
            "unknown_lint_in_lints_table",
            at_file(
                "workspace-lint::unknown-lint",
                "unknown lint `unused-dep` in `[lints]`",
                PathBuf::from(".workspace-lint.toml"),
            )
            .help("did you mean `unused-deps`?")
            .build(),
        ),
    ]
}

// Structural-quality assertions across every `scenarios()` Diagnostic live in a
// sibling file to keep this one focused on the message surface and its snapshots.
#[cfg(test)]
#[path = "messages_quality.rs"]
mod quality_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use wl_diagnostic::render::{Format, render_one};

    // -------------------------------------------------------------------------
    // HUMAN renderer — clippy-style text. Reviewer scrolls this section to
    // validate the diagnostic *prose* the user sees in their terminal.
    // -------------------------------------------------------------------------

    mod human_snapshots {
        use super::*;

        fn render(d: &Diagnostic) -> String {
            let mut buf = Vec::new();
            render_one(Format::Human, d, &mut buf).unwrap();
            String::from_utf8(buf).unwrap()
        }

        fn scenario(name: &str) -> Diagnostic {
            scenarios()
                .into_iter()
                .find(|(n, _)| *n == name)
                .map(|(_, d)| d)
                .unwrap_or_else(|| panic!("missing scenario: {name}"))
        }

        #[test]
        fn centralized_deps_one_dep() {
            insta::assert_snapshot!(render(&scenario("centralized_deps_one_dep")), @r#"
            warning: 1 dependency in crates/alpha/Cargo.toml should use `workspace = true`
             --> crates/alpha/Cargo.toml:1:1
              |
              = help: [dependencies] serde: has own version "1.0.200" — use { workspace = true } instead
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(centralized-deps)
              |
              = note: `#[warn(workspace_lint::centralized_deps)]` on by default
            "#);
        }

        #[test]
        fn centralized_deps_workspace_seed() {
            insta::assert_snapshot!(render(&scenario("centralized_deps_workspace_seed")), @r#"
            warning: 1 dependency in crates/alpha/Cargo.toml should use `workspace = true`
             --> crates/alpha/Cargo.toml:1:1
              |
              = help: [dependencies] rand: version "0.8" not in [workspace.dependencies] — add it there and use { workspace = true }
            help: add `rand` to [workspace.dependencies]
              |
            5 + rand = "0.8"
              |
            help: use { workspace = true } for `rand`
              |
            7 - rand = "0.8"
            7 + rand = { workspace = true }
              |
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(centralized-deps)
              |
              = note: `#[warn(workspace_lint::centralized_deps)]` on by default
            "#);
        }

        #[test]
        fn centralized_deps_table_creation() {
            insta::assert_snapshot!(render(&scenario("centralized_deps_table_creation")), @r#"
            warning: 2 dependencies in crates/alpha/Cargo.toml should use `workspace = true`
             --> crates/alpha/Cargo.toml:1:1
              |
              = help: [dependencies] log: version "0.4" not in [workspace.dependencies] — add it there and use { workspace = true }
              = help: [dependencies] serde: version "1" not in [workspace.dependencies] — add it there and use { workspace = true }
            help: create [workspace.dependencies] with `log`, `serde`
              |
            3 + 
            3 + [workspace.dependencies]
            3 + log = { version = "0.4", default-features = false }
            3 + serde = "1"
              |
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(centralized-deps)
              |
              = note: `#[warn(workspace_lint::centralized_deps)]` on by default
            "#);
        }

        #[test]
        fn centralized_deps_multiple_deps() {
            insta::assert_snapshot!(render(&scenario("centralized_deps_multiple_deps")), @r#"
            warning: 2 dependencies in crates/beta/Cargo.toml should use `workspace = true`
             --> crates/beta/Cargo.toml:1:1
              |
              = help: [dependencies] serde: version "1.0" not in [workspace.dependencies]
              = help: [dev-dependencies] rand: version "0.8" not in [workspace.dependencies]
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(centralized-deps)
              |
              = note: `#[warn(workspace_lint::centralized_deps)]` on by default
            "#);
        }

        #[test]
        fn file_size_over_limit() {
            insta::assert_snapshot!(render(&scenario("file_size_over_limit")), @r#"
            warning: file exceeds 500 code lines (612)
             --> crates/web-api/src/handler.rs:1:1
              |
              = help: split this file into focused submodules (e.g. a `foo/` directory with a `mod.rs`)
              = help: extract related structs, enums, or trait impls into their own modules
              = help: only shipped source counts — `#[cfg(test)]` and `#[test]` code is already excluded
              = note: configured by [[file-size.rules]] glob = "**/*.rs"
            help: if intentional, silence with:
              |
            1 + workspace_lint::expect!(file_size);
              |
              = note: `#[warn(workspace_lint::file_size)]` on by default
            "#);
        }

        #[test]
        fn crate_size_over_limit() {
            insta::assert_snapshot!(render(&scenario("crate_size_over_limit")), @r#"
            warning: crate exceeds 5000 code lines (7321)
             --> crates/legacy/Cargo.toml:1:1
              |
              = help: split the crate into smaller, more focused crates
              = note: configured by [[crate-size.rules]] glob = "crates/*"
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(crate-size)
              |
              = note: `#[warn(workspace_lint::crate_size)]` on by default
            "#);
        }

        #[test]
        fn duplicate_code_group() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_group")), @r"
            warning: duplicated code: 3 structurally identical instances (~14 lines)
             --> crates/alpha/src/report.rs:42:1
              |
              = help: extract the shared logic into one function the copies can call
              = note: also found at: crates/beta/src/render.rs:88, crates/gamma/src/emit.rs:17
              = note: matching ignores local variable names and literal values
              = note: extracting would take ~2 parameters for the differing literals
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(duplicate_code);
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_drift() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_drift")), @r#"
            warning: duplicated code: 2 structurally identical instances (~12 lines)
             --> crates/alpha/src/report.rs:42:1
              |
              = help: extract the shared logic into one function the copies can call
              = note: also found at: crates/beta/src/render.rs:88
              = note: matching ignores local variable names and literal values
              = note: possible copy-paste drift: crates/beta/src/render.rs:96 has "alpha" where the mapping elsewhere expects "beta"
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(duplicate_code);
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            "#);
        }

        #[test]
        fn duplicate_code_merge_identical_fns() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_merge_identical_fns")), @r"
            warning: duplicated code: 2 structurally identical instances (~8 lines)
             --> crates/alpha/src/report.rs:42:1
              |
              = help: these are copies of the same function — keep one and redirect the other call sites
              = note: also found at: crates/beta/src/render.rs:88
              = note: matching ignores local variable names and literal values
              = note: instances are identical (differing at most in local names)
              = note: 2 call sites reference the copies (first at crates/alpha/src/main.rs:15)
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(duplicate_code);
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_delete_dead_copy() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_delete_dead_copy")), @r"
            warning: duplicated code: 2 structurally identical instances (~8 lines)
             --> crates/alpha/src/report.rs:42:1
              |
              = help: the copy at crates/beta/src/render.rs:88 is never referenced — delete it
              = note: also found at: crates/beta/src/render.rs:88
              = note: matching ignores local variable names and literal values
              = note: instances are identical (differing at most in local names)
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(duplicate_code);
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_default_trait_method() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_default_trait_method")), @r"
            warning: duplicated code: 2 structurally identical instances (~6 lines)
             --> crates/alpha/src/report.rs:42:1
              |
              = help: every copy implements `Formatter::render` — make it a default method on the trait
              = note: also found at: crates/beta/src/render.rs:88
              = note: matching ignores local variable names and literal values
              = note: instances are identical (differing at most in local names)
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(duplicate_code);
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_method_on_receiver_type() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_method_on_receiver_type")), @r"
            warning: duplicated code: 2 structurally identical instances (~5 lines)
             --> crates/alpha/src/report.rs:42:1
              |
              = help: extract the shared logic into a method on `Config` — every copy takes it as the first parameter
              = note: also found at: crates/beta/src/render.rs:88
              = note: matching ignores local variable names and literal values
              = note: extracting would take ~1 parameter for the differing literals
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(duplicate_code);
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_ui_component() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_ui_component")), @r"
            warning: duplicated code: 2 structurally identical instances (~4 lines)
             --> crates/alpha/src/report.rs:42:1
              |
              = help: extract the shared `rsx!` markup into one component the copies can render
              = note: also found at: crates/beta/src/render.rs:88
              = note: matching ignores local variable names and literal values
              = note: extracting would take ~1 parameter for the differing literals
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(duplicate_code);
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_merge_withheld() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_merge_withheld")), @r"
            warning: duplicated code: 2 structurally identical instances (~6 lines)
             --> crates/alpha/src/report.rs:42:1
              |
              = help: extract the shared logic into one function the copies can call
              = note: also found at: crates/beta/src/render.rs:88
              = note: matching ignores local variable names and literal values
              = note: instances are identical (differing at most in local names)
              = note: instances resolve different callees — the copies may not be interchangeable
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(duplicate_code);
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_run_signature() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_run_signature")), @r"
            warning: duplicated code: 2 structurally identical instances (~6 lines)
             --> crates/alpha/src/report.rs:42:1
              |
              = help: extract the shared logic into one function the copies can call
              = note: also found at: crates/beta/src/render.rs:88
              = note: matching ignores local variable names and literal values
              = note: instances are identical (differing at most in local names)
              = note: an extracted fn would take 2 parameters (items, config) and return total
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(duplicate_code);
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_run_live_out_downgrade() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_run_live_out_downgrade")), @r"
            warning: duplicated code: 2 structurally identical instances (~7 lines)
             --> crates/alpha/src/report.rs:42:1
              |
              = help: extract the shared logic into one function the copies can call
              = note: also found at: crates/beta/src/render.rs:88
              = note: matching ignores local variable names and literal values
              = note: instances are identical (differing at most in local names)
              = note: an extracted fn would take 1 parameter (items) but needs 3 return values (count, total, errors) — extraction is awkward; consider restructuring
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(duplicate_code);
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_baseline_grew() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_grew")), @r"
            warning: duplicated code: 3 structurally identical instances (~8 lines)
             --> crates/alpha/src/report.rs:42:1
              |
              = help: extract the shared logic into one function the copies can call
              = note: also found at: crates/beta/src/render.rs:88, crates/gamma/src/emit.rs:17
              = note: matching ignores local variable names and literal values
              = note: grew beyond its baseline: 2 instances accepted, now 3
              = note: instances are identical (differing at most in local names)
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(duplicate_code);
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_baseline_stale() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_stale")), @r"
            warning: stale duplicate-code baseline entry: no clone group matches fingerprint 9930bf3835a56614 (was crates/alpha/src/report.rs, 2 instances)
             --> duplicate-code.baseline.toml:8:1
              |
              = help: regenerate with `workspace-lint --baseline-write` (or delete this entry)
              = note: the duplication was resolved, or the code changed enough to re-fingerprint
            help: if intentional, silence with:
              |
            8 + # workspace-lint: expect(duplicate-code)
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_baseline_overcount() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_overcount")), @r"
            warning: duplicate-code baseline entry 9930bf3835a56614 records 3 instances but only 2 remain
             --> duplicate-code.baseline.toml:8:1
              |
              = help: ratchet down: regenerate with `workspace-lint --baseline-write`
            help: if intentional, silence with:
              |
            8 + # workspace-lint: expect(duplicate-code)
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn duplicate_code_baseline_missing() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_missing")), @r"
            warning: duplicate-code baseline file `duplicate-code.baseline.toml` not found
             --> duplicate-code.baseline.toml:1:1
              |
              = help: generate it with `workspace-lint --baseline-write`, or remove `baseline` from [duplicate-code]
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(duplicate-code)
              |
              = note: `#[warn(workspace_lint::duplicate_code)]` on by default
            ");
        }

        #[test]
        fn cli_crate_version_mismatch() {
            insta::assert_snapshot!(render(&scenario("cli_crate_version_mismatch")), @r"
            warning: `wasm-bindgen` CLI version 0.2.89 does not match Cargo.lock 0.2.90
              = help: update or reinstall `wasm-bindgen` to match the workspace version
              = note: ran `wasm-bindgen --version`
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(cli-crate-version)
              |
              = note: `#[warn(workspace_lint::cli_crate_version)]` on by default
            ");
        }

        #[test]
        fn cli_crate_version_rule_error() {
            insta::assert_snapshot!(render(&scenario("cli_crate_version_rule_error")), @r"
            warning: pattern `v(\d+)` did not match the output of `wasm-bindgen --version`
              = help: the regex must capture the version in group 1
              = note: ran `wasm-bindgen --version`
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(cli-crate-version)
              |
              = note: `#[warn(workspace_lint::cli_crate_version)]` on by default
            ");
        }

        #[test]
        fn unused_deps_one() {
            insta::assert_snapshot!(render(&scenario("unused_deps_one")), @r"
            warning: 1 possibly unused dependency in crates/alpha/Cargo.toml
             --> crates/alpha/Cargo.toml:1:1
              |
              = help: [dependencies] rand
              = note: build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives
              = note: verify by removing the dep and running `cargo build --all-targets`
              = note: if the build breaks, add the dep to [unused-deps] ignore in your config
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(unused-deps)
              |
              = note: `#[warn(workspace_lint::unused_deps)]` on by default
            ");
        }

        #[test]
        fn unused_deps_multiple() {
            insta::assert_snapshot!(render(&scenario("unused_deps_multiple")), @r"
            warning: 2 possibly unused dependencies in crates/beta/Cargo.toml
             --> crates/beta/Cargo.toml:1:1
              |
              = help: [dependencies] foo
              = help: [dev-dependencies] bar
              = note: build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives
              = note: verify by removing the dep and running `cargo build --all-targets`
              = note: if the build breaks, add the dep to [unused-deps] ignore in your config
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(unused-deps)
              |
              = note: `#[warn(workspace_lint::unused_deps)]` on by default
            ");
        }

        #[test]
        fn unused_deps_removal_withheld() {
            insta::assert_snapshot!(render(&scenario("unused_deps_removal_withheld")), @r#"
            warning: 1 possibly unused dependency in crates/alpha/Cargo.toml
             --> crates/alpha/Cargo.toml:1:1
              |
              = help: [dependencies] rand
              = note: not auto-applied: removing a dependency is `--fix-auto-delete` only — the verdict is "possibly unused"; verify before deleting
              = note: build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives
              = note: verify by removing the dep and running `cargo build --all-targets`
              = note: if the build breaks, add the dep to [unused-deps] ignore in your config
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(unused-deps)
              |
              = note: `#[warn(workspace_lint::unused_deps)]` on by default
            "#);
        }

        #[test]
        fn unused_deps_dirty_manifest() {
            insta::assert_snapshot!(render(&scenario("unused_deps_dirty_manifest")), @r"
            warning: 1 possibly unused dependency in crates/alpha/Cargo.toml
             --> crates/alpha/Cargo.toml:1:1
              |
              = help: [dependencies] rand
              = note: file `crates/alpha/Cargo.toml` is untracked or has uncommitted changes; `--fix-auto-delete` will not delete it (commit first or use `git stash`)
              = note: build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives
              = note: verify by removing the dep and running `cargo build --all-targets`
              = note: if the build breaks, add the dep to [unused-deps] ignore in your config
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(unused-deps)
              |
              = note: `#[warn(workspace_lint::unused_deps)]` on by default
            ");
        }

        #[test]
        fn unused_deps_not_compiled() {
            insta::assert_snapshot!(render(&scenario("unused_deps_not_compiled")), @r#"
            warning: 2 dependencies of `gamma` could not be checked in crates/gamma/Cargo.toml
             --> crates/gamma/Cargo.toml:1:1
              |
              = help: [dependencies] foo
              = help: [dependencies] bar
              = help: compile it under an [engine] config (e.g. "cargo build --target <triple> -p gamma"), or add these deps to [unused-deps] ignore
              = note: this crate produced no compiler output under the current [engine] config matrix
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(unused-deps)
              |
              = note: `#[warn(workspace_lint::unused_deps)]` on by default
            "#);
        }

        #[test]
        fn unused_pub_removal_candidate() {
            insta::assert_snapshot!(render(&scenario("unused_pub_removal_candidate")), @r"
            warning: pub fn `helper` in crate `mycrate` appears unused — consider removing
             --> crates/mycrate/src/lib.rs:42:1
              |
              = help: remove the item or its `pub` visibility
              = note: code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives
              = note: not auto-applied: deleting an unused item is `--fix-auto-delete` only — verify it is truly unused, then delete it or narrow by hand
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn unused_pub_cfg_shadowed() {
            insta::assert_snapshot!(render(&scenario("unused_pub_cfg_shadowed")), @r#"
            warning: pub fn `tz_offset_minutes` in crate `utils` appears unused — consider removing
             --> crates/utils/src/lib.rs:9:1
              |
              = help: remove the item or its `pub` visibility
              = note: possibly used: mentioned under `cfg(target_arch = "wasm32")` (crates/app/src/main.rs), which no declared `[engine]` config compiles — add a matching cargo command to `[engine] configs` to judge that code
              = note: not auto-applied: deleting an unused item is `--fix-auto-delete` only — verify it is truly unused, then delete it or narrow by hand
            help: if intentional, silence with:
              |
            9 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            "#);
        }

        #[test]
        fn unused_pub_test_only() {
            insta::assert_snapshot!(render(&scenario("unused_pub_test_only")), @r"
            warning: pub fn `helper` in crate `mycrate` is only used by test code
             --> crates/mycrate/src/lib.rs:42:1
              |
              = help: gate it `#[cfg(test)]`, move it into test code, or remove it
              = note: code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives
              = note: no fix is auto-applied: `pub(crate)` would trip `dead_code` on the non-test build, and deleting the item would break the tests that reference it
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn unused_pub_test_only_blocked() {
            insta::assert_snapshot!(render(&scenario("unused_pub_test_only_blocked")), @r"
            warning: pub fn `embalmed` in crate `alpha` is only used by test code
             --> crates/alpha/src/lib.rs:6:1
              |
              = help: gate it `#[cfg(test)]`, move it into test code, or remove it
              = note: code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives
              = note: only test code references it, but test item `beta::tests::covers_both` (crates/beta/src/main.rs:8) also exercises surviving `alpha::kept` — deleting would orphan that test; update or remove the test first, or delete both by hand
            help: if intentional, silence with:
              |
            6 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn unused_pub_test_scaffold() {
            insta::assert_snapshot!(render(&scenario("unused_pub_test_scaffold")), @r"
            warning: test fn `exercises_embalmed` in crate `beta` only exercises items deleted by this `--fix`
             --> crates/beta/src/main.rs:12:1
              |
              = help: deleting it too — it would reference deleted items and break the test build
              = note: exclusive test scaffolding: every workspace item it references is also deleted by this `--fix`
            help: if intentional, silence with:
              |
            12 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn unused_pub_cascade_transitive() {
            insta::assert_snapshot!(render(&scenario("unused_pub_cascade_transitive")), @r"
            warning: pub fn `helper` in crate `mycrate` appears unused — consider removing
             --> crates/mycrate/src/inner.rs:14:1
              |
              = help: remove the item or its `pub` visibility
              = note: code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives
              = note: transitively unused: the only item(s) that referenced it are also deleted by this `--fix`
            help: if intentional, silence with:
              |
            14 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn unused_pub_delete_unmask_field() {
            insta::assert_snapshot!(render(&scenario("unused_pub_delete_unmask_field")), @r"
            warning: pub fn `open_state` in crate `widgets` appears unused — consider removing
             --> crates/widgets/src/lib.rs:13:1
              |
              = help: remove the item or its `pub` visibility
              = note: deleting this would leave field `open` of surviving `widgets::Panel` never-read, tripping `dead_code` on the fixed tree — remove the field first or delete by hand
            help: if intentional, silence with:
              |
            13 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn unused_pub_delete_unmask_len() {
            insta::assert_snapshot!(render(&scenario("unused_pub_delete_unmask_len")), @r"
            warning: pub fn `is_empty` in crate `store` appears unused — consider removing
             --> crates/store/src/buf.rs:17:1
              |
              = help: remove the item or its `pub` visibility
              = note: deleting `is_empty` would trip clippy `len_without_is_empty` on `store::Buf`'s surviving `len` — remove or keep the pair together
            help: if intentional, silence with:
              |
            17 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn unused_pub_import_surgery() {
            insta::assert_snapshot!(render(&scenario("unused_pub_import_surgery")), @r"
            warning: unused import of a removed item
             --> crates/mycrate/src/lib.rs:3:1
              |
              = help: removing the dangling `use` left by the deleted item
            help: remove the unused import
              |
            3 - helper, 
              |
            help: if intentional, silence with:
              |
            3 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn unused_pub_tighten_visibility() {
            insta::assert_snapshot!(render(&scenario("unused_pub_tighten_visibility")), @r"
            warning: pub struct `Builder` in crate `mycrate` is only used inside the crate
             --> crates/mycrate/src/builder.rs:7:1
              |
              = help: consider `pub(crate)` to tighten visibility
              = note: code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives
            help: if intentional, silence with:
              |
            7 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn unused_pub_publish_hint() {
            insta::assert_snapshot!(render(&scenario("unused_pub_publish_hint")), @r"
            warning: crate `mycrate` has 3 public items unused within the workspace
             --> crates/mycrate/Cargo.toml:1:1
              |
              = help: if `mycrate` is published outside this workspace, set `publish = true` in its Cargo.toml to treat its public API as external (these findings become exempt)
              = note: workspace-lint treats a crate as workspace-internal unless it declares `publish = true` (or a registry); see the unused-pub docs
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(unused-pub)
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn unused_pub_tighten_unmask() {
            insta::assert_snapshot!(render(&scenario("unused_pub_tighten_unmask")), @r"
            warning: pub struct `Rect` in crate `mycrate` is only used inside the crate
             --> crates/mycrate/src/geometry.rs:11:1
              |
              = help: consider `pub(crate)` to tighten visibility
              = note: code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives
              = note: `pub(crate)` would unmask clippy `wrong_self_convention` on `is_wide` (clippy exempts exported items via `avoid-breaking-exported-api`) — resolve that first or narrow by hand
            help: if intentional, silence with:
              |
            11 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn unused_pub_private_collateral() {
            insta::assert_snapshot!(render(&scenario("unused_pub_private_collateral")), @r"
            warning: private fn `helper` in crate `mycrate` loses its last user in this `--fix`
             --> crates/mycrate/src/lib.rs:21:1
              |
              = help: deleting it too — rustc `dead_code` would flag it on the fixed tree
              = note: transitively dead: the only item(s) that referenced it are also deleted by this `--fix`
            help: if intentional, silence with:
              |
            21 + workspace_lint::expect!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            ");
        }

        #[test]
        fn stale_expect() {
            insta::assert_snapshot!(render(&scenario("stale_expect")), @r"
            warning: expect directive for `file-size` did not match any diagnostic
             --> crates/api/src/lib.rs:1:1
              |
              = help: remove this expect — the lint it tracks is no longer firing
              = note: a stale expect usually means the underlying issue has been fixed
            help: remove this stale expect directive
              |
            1 - workspace_lint::expect!(file_size);
              |
            help: if intentional, silence with:
              |
            1 + workspace_lint::expect!(stale_expect);
              |
              = note: `#[warn(workspace_lint::stale_expect)]` on by default
            ");
        }

        #[test]
        fn architecture_denied_import() {
            insta::assert_snapshot!(render(&scenario("architecture_denied_import")), @r"
            warning: import of `data_models::internal::User` from `apps-foo` violates architecture rule `no-internal-imports`
             --> crates/apps-foo/src/lib.rs:7:1
              |
              = help: import from `data-models::api` instead
              = note: internal types are not part of the published API surface
            help: if intentional, silence with:
              |
            7 + workspace_lint::expect!(architecture);
              |
              = note: `#[warn(workspace_lint::architecture)]` on by default
            ");
        }

        #[test]
        fn architecture_denied_code_reference() {
            insta::assert_snapshot!(render(&scenario("architecture_denied_code_reference")), @r"
            warning: reference to `data_models::internal::User` from `apps-foo` violates architecture rule `no-internal-imports`
             --> crates/apps-foo/src/lib.rs:12:1
              |
              = help: import from `data-models::api` instead
              = note: internal types are not part of the published API surface
            help: if intentional, silence with:
              |
            12 + workspace_lint::expect!(architecture);
              |
              = note: `#[warn(workspace_lint::architecture)]` on by default
            ");
        }

        #[test]
        fn orphan_file_orphan() {
            insta::assert_snapshot!(render(&scenario("orphan_file_orphan")), @r#"
            warning: orphan source file `src/orphan.rs` is never compiled
             --> crates/demo/src/orphan.rs:1:1
              |
              = help: delete the file, or reach it: add `mod orphan;` (or `#[path = "src/orphan.rs"] mod ...;`) in the appropriate parent module
              = note: no `[engine]` config compiled it, and nothing in crate `demo`'s source names it
            help: if intentional, silence with:
              |
            1 + workspace_lint::expect!(orphan_file);
              |
              = note: `#[warn(workspace_lint::orphan_file)]` on by default
            "#);
        }

        #[test]
        fn orphan_file_cfg_coverage_gap() {
            insta::assert_snapshot!(render(&scenario("orphan_file_cfg_coverage_gap")), @r#"
            warning: no declared `[engine]` config compiles `src/imp_windows.rs`
             --> crates/demo/src/imp_windows.rs:1:1
              |
              = help: add a config that compiles it — a `--target` for a platform-gated module, or `"cargo test"` for a `#[cfg(test)]` one
              = note: crate `demo`'s source names this file, so it is not reported as an orphan — but the declared config (default) never opened it, so nothing in it is checked
            help: if intentional, silence with:
              |
            1 + workspace_lint::expect!(orphan_file);
              |
              = note: `#[warn(workspace_lint::orphan_file)]` on by default
            "#);
        }

        #[test]
        fn feature_drift_declared_never_gated() {
            insta::assert_snapshot!(render(&scenario("feature_drift_declared_never_gated")), @r#"
            warning: feature `experimental` is declared in `[features]` but never gated in source
             --> crates/demo/Cargo.toml:1:1
              |
              = help: either gate code with `#[cfg(feature = "experimental")]` or remove `experimental` from `[features]`
              = note: declared in `demo/Cargo.toml`
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(feature-drift)
              |
              = note: `#[warn(workspace_lint::feature_drift)]` on by default
            "#);
        }

        #[test]
        fn feature_drift_gated_undeclared() {
            insta::assert_snapshot!(render(&scenario("feature_drift_gated_undeclared")), @r#"
            warning: feature `nightly` is gated in source but not declared in `[features]`
             --> crates/demo/Cargo.toml:1:1
              |
              = help: add `nightly = []` to the `[features]` table of `demo/Cargo.toml`, or remove the `cfg(feature = "nightly")` references
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(feature-drift)
              |
              = note: `#[warn(workspace_lint::feature_drift)]` on by default
            "#);
        }

        #[test]
        fn config_unknown_key() {
            insta::assert_snapshot!(render(&scenario("config_unknown_key")), @r"
            warning: unknown configuration section `file-siz`
             --> .workspace-lint.toml:1:1
              |
              = help: did you mean `file-size`?
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(config)
              |
              = note: `#[warn(workspace_lint::config)]` on by default
            ");
        }

        #[test]
        fn unknown_lint_in_lints_table() {
            insta::assert_snapshot!(render(&scenario("unknown_lint_in_lints_table")), @r"
            warning: unknown lint `unused-dep` in `[lints]`
             --> .workspace-lint.toml:1:1
              |
              = help: did you mean `unused-deps`?
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(unknown-lint)
              |
              = note: `#[warn(workspace_lint::unknown_lint)]` on by default
            ");
        }
    }

    // -------------------------------------------------------------------------
    // JSON renderer — rustc-compatible per-line records. Reviewer scrolls
    // this section to validate the JSON *shape* (field names, applicability,
    // suggestion text) that rust-analyzer and `--fix` consume.
    // -------------------------------------------------------------------------

    mod json_snapshots {
        use super::*;

        /// Render one diagnostic to its raw JSON line. The single-line shape
        /// is exactly what the binary emits on stdout under
        /// `--message-format=json`, so reviewers see exactly what
        /// rust-analyzer / CI tools consume.
        fn render(d: &Diagnostic) -> String {
            let mut buf = Vec::new();
            render_one(Format::Json, d, &mut buf).unwrap();
            String::from_utf8(buf).unwrap().trim_end().to_string()
        }

        fn scenario(name: &str) -> Diagnostic {
            scenarios()
                .into_iter()
                .find(|(n, _)| *n == name)
                .map(|(_, d)| d)
                .unwrap()
        }

        #[test]
        fn centralized_deps_one_dep() {
            insta::assert_snapshot!(render(&scenario("centralized_deps_one_dep")), @r##"{"level":"warning","message":"1 dependency in crates/alpha/Cargo.toml should use `workspace = true`","code":{"code":"workspace-lint::centralized-deps","explanation":null},"spans":[{"file_name":"crates/alpha/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(centralized-deps)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"[dependencies] serde: has own version \"1.0.200\" — use { workspace = true } instead","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn file_size_over_limit() {
            // Validates the most important JSON contract: a Rust file
            // diagnostic carries a suggested_replacement with the `allow!`
            // macro text so the IDE quick-fix Just Works.
            insta::assert_snapshot!(render(&scenario("file_size_over_limit")), @r#"{"level":"warning","message":"file exceeds 500 code lines (612)","code":{"code":"workspace-lint::file-size","explanation":null},"spans":[{"file_name":"crates/web-api/src/handler.rs","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/web-api/src/handler.rs","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(file_size);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"split this file into focused submodules (e.g. a `foo/` directory with a `mod.rs`)","spans":[]},{"level":"help","message":"extract related structs, enums, or trait impls into their own modules","spans":[]},{"level":"help","message":"only shipped source counts — `#[cfg(test)]` and `#[test]` code is already excluded","spans":[]},{"level":"note","message":"configured by [[file-size.rules]] glob = \"**/*.rs\"","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn unused_pub_removal_carries_specific_line() {
            // Line-anchored diagnostics put the specific line into the silence
            // suggestion's span — the IDE inserts the marker there, not at
            // line 1.
            let v: serde_json::Value =
                serde_json::from_str(&render(&scenario("unused_pub_removal_candidate"))).unwrap();
            let silence_span = &v["children"][0]["spans"][0];
            assert_eq!(silence_span["line_start"], 42);
            assert_eq!(
                silence_span["suggested_replacement"],
                "workspace_lint::expect!(unused_pub);\n"
            );
        }

        #[test]
        fn cli_crate_version_workspace_anchor_has_no_primary_span() {
            let v: serde_json::Value =
                serde_json::from_str(&render(&scenario("cli_crate_version_mismatch"))).unwrap();
            assert!(v["spans"].as_array().unwrap().is_empty());
        }

        #[test]
        fn stale_expect_lint_code_matches() {
            let v: serde_json::Value =
                serde_json::from_str(&render(&scenario("stale_expect"))).unwrap();
            assert_eq!(v["code"]["code"], "workspace-lint::stale-expect");
        }

        #[test]
        fn architecture_denied_import() {
            insta::assert_snapshot!(render(&scenario("architecture_denied_import")), @r#"{"level":"warning","message":"import of `data_models::internal::User` from `apps-foo` violates architecture rule `no-internal-imports`","code":{"code":"workspace-lint::architecture","explanation":null},"spans":[{"file_name":"crates/apps-foo/src/lib.rs","byte_start":0,"byte_end":0,"line_start":7,"line_end":7,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/apps-foo/src/lib.rs","byte_start":0,"byte_end":0,"line_start":7,"line_end":7,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(architecture);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"import from `data-models::api` instead","spans":[]},{"level":"note","message":"internal types are not part of the published API surface","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn architecture_denied_code_reference() {
            insta::assert_snapshot!(render(&scenario("architecture_denied_code_reference")), @r#"{"level":"warning","message":"reference to `data_models::internal::User` from `apps-foo` violates architecture rule `no-internal-imports`","code":{"code":"workspace-lint::architecture","explanation":null},"spans":[{"file_name":"crates/apps-foo/src/lib.rs","byte_start":0,"byte_end":0,"line_start":12,"line_end":12,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/apps-foo/src/lib.rs","byte_start":0,"byte_end":0,"line_start":12,"line_end":12,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(architecture);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"import from `data-models::api` instead","spans":[]},{"level":"note","message":"internal types are not part of the published API surface","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn orphan_file_orphan() {
            insta::assert_snapshot!(render(&scenario("orphan_file_orphan")), @r#"{"level":"warning","message":"orphan source file `src/orphan.rs` is never compiled","code":{"code":"workspace-lint::orphan-file","explanation":null},"spans":[{"file_name":"crates/demo/src/orphan.rs","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/demo/src/orphan.rs","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(orphan_file);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"delete the file, or reach it: add `mod orphan;` (or `#[path = \"src/orphan.rs\"] mod ...;`) in the appropriate parent module","spans":[]},{"level":"note","message":"no `[engine]` config compiled it, and nothing in crate `demo`'s source names it","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn orphan_file_cfg_coverage_gap() {
            insta::assert_snapshot!(render(&scenario("orphan_file_cfg_coverage_gap")), @r#"{"level":"warning","message":"no declared `[engine]` config compiles `src/imp_windows.rs`","code":{"code":"workspace-lint::orphan-file","explanation":null},"spans":[{"file_name":"crates/demo/src/imp_windows.rs","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/demo/src/imp_windows.rs","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(orphan_file);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"add a config that compiles it — a `--target` for a platform-gated module, or `\"cargo test\"` for a `#[cfg(test)]` one","spans":[]},{"level":"note","message":"crate `demo`'s source names this file, so it is not reported as an orphan — but the declared config (default) never opened it, so nothing in it is checked","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn feature_drift_declared_never_gated() {
            insta::assert_snapshot!(render(&scenario("feature_drift_declared_never_gated")), @r##"{"level":"warning","message":"feature `experimental` is declared in `[features]` but never gated in source","code":{"code":"workspace-lint::feature-drift","explanation":null},"spans":[{"file_name":"crates/demo/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/demo/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(feature-drift)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"either gate code with `#[cfg(feature = \"experimental\")]` or remove `experimental` from `[features]`","spans":[]},{"level":"note","message":"declared in `demo/Cargo.toml`","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn feature_drift_gated_undeclared() {
            insta::assert_snapshot!(render(&scenario("feature_drift_gated_undeclared")), @r##"{"level":"warning","message":"feature `nightly` is gated in source but not declared in `[features]`","code":{"code":"workspace-lint::feature-drift","explanation":null},"spans":[{"file_name":"crates/demo/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/demo/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(feature-drift)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"add `nightly = []` to the `[features]` table of `demo/Cargo.toml`, or remove the `cfg(feature = \"nightly\")` references","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn config_unknown_key() {
            insta::assert_snapshot!(render(&scenario("config_unknown_key")), @r##"{"level":"warning","message":"unknown configuration section `file-siz`","code":{"code":"workspace-lint::config","explanation":null},"spans":[{"file_name":".workspace-lint.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":".workspace-lint.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(config)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"did you mean `file-size`?","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn unknown_lint_in_lints_table() {
            insta::assert_snapshot!(render(&scenario("unknown_lint_in_lints_table")), @r##"{"level":"warning","message":"unknown lint `unused-dep` in `[lints]`","code":{"code":"workspace-lint::unknown-lint","explanation":null},"spans":[{"file_name":".workspace-lint.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":".workspace-lint.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(unknown-lint)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"did you mean `unused-deps`?","spans":[]}],"rendered":null}"##);
        }

        // The remaining scenarios complete JSON coverage so every distinct
        // diagnostic is pinned in all three formats (unused-deps especially —
        // `--fix` consumes its JSON `suggested_replacement`).

        #[test]
        fn centralized_deps_multiple_deps() {
            insta::assert_snapshot!(render(&scenario("centralized_deps_multiple_deps")), @r##"{"level":"warning","message":"2 dependencies in crates/beta/Cargo.toml should use `workspace = true`","code":{"code":"workspace-lint::centralized-deps","explanation":null},"spans":[{"file_name":"crates/beta/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/beta/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(centralized-deps)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"[dependencies] serde: version \"1.0\" not in [workspace.dependencies]","spans":[]},{"level":"help","message":"[dev-dependencies] rand: version \"0.8\" not in [workspace.dependencies]","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn crate_size_over_limit() {
            insta::assert_snapshot!(render(&scenario("crate_size_over_limit")), @r##"{"level":"warning","message":"crate exceeds 5000 code lines (7321)","code":{"code":"workspace-lint::crate-size","explanation":null},"spans":[{"file_name":"crates/legacy/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/legacy/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(crate-size)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"split the crate into smaller, more focused crates","spans":[]},{"level":"note","message":"configured by [[crate-size.rules]] glob = \"crates/*\"","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn duplicate_code_group() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_group")), @r#"{"level":"warning","message":"duplicated code: 3 structurally identical instances (~14 lines)","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(duplicate_code);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"extract the shared logic into one function the copies can call","spans":[]},{"level":"note","message":"also found at: crates/beta/src/render.rs:88, crates/gamma/src/emit.rs:17","spans":[]},{"level":"note","message":"matching ignores local variable names and literal values","spans":[]},{"level":"note","message":"extracting would take ~2 parameters for the differing literals","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn duplicate_code_merge_identical_fns() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_merge_identical_fns")), @r#"{"level":"warning","message":"duplicated code: 2 structurally identical instances (~8 lines)","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(duplicate_code);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"these are copies of the same function — keep one and redirect the other call sites","spans":[]},{"level":"note","message":"also found at: crates/beta/src/render.rs:88","spans":[]},{"level":"note","message":"matching ignores local variable names and literal values","spans":[]},{"level":"note","message":"instances are identical (differing at most in local names)","spans":[]},{"level":"note","message":"2 call sites reference the copies (first at crates/alpha/src/main.rs:15)","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn duplicate_code_delete_dead_copy() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_delete_dead_copy")), @r#"{"level":"warning","message":"duplicated code: 2 structurally identical instances (~8 lines)","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(duplicate_code);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"the copy at crates/beta/src/render.rs:88 is never referenced — delete it","spans":[]},{"level":"note","message":"also found at: crates/beta/src/render.rs:88","spans":[]},{"level":"note","message":"matching ignores local variable names and literal values","spans":[]},{"level":"note","message":"instances are identical (differing at most in local names)","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn duplicate_code_default_trait_method() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_default_trait_method")), @r#"{"level":"warning","message":"duplicated code: 2 structurally identical instances (~6 lines)","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(duplicate_code);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"every copy implements `Formatter::render` — make it a default method on the trait","spans":[]},{"level":"note","message":"also found at: crates/beta/src/render.rs:88","spans":[]},{"level":"note","message":"matching ignores local variable names and literal values","spans":[]},{"level":"note","message":"instances are identical (differing at most in local names)","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn duplicate_code_method_on_receiver_type() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_method_on_receiver_type")), @r#"{"level":"warning","message":"duplicated code: 2 structurally identical instances (~5 lines)","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(duplicate_code);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"extract the shared logic into a method on `Config` — every copy takes it as the first parameter","spans":[]},{"level":"note","message":"also found at: crates/beta/src/render.rs:88","spans":[]},{"level":"note","message":"matching ignores local variable names and literal values","spans":[]},{"level":"note","message":"extracting would take ~1 parameter for the differing literals","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn duplicate_code_ui_component() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_ui_component")), @r#"{"level":"warning","message":"duplicated code: 2 structurally identical instances (~4 lines)","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(duplicate_code);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"extract the shared `rsx!` markup into one component the copies can render","spans":[]},{"level":"note","message":"also found at: crates/beta/src/render.rs:88","spans":[]},{"level":"note","message":"matching ignores local variable names and literal values","spans":[]},{"level":"note","message":"extracting would take ~1 parameter for the differing literals","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn duplicate_code_merge_withheld() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_merge_withheld")), @r#"{"level":"warning","message":"duplicated code: 2 structurally identical instances (~6 lines)","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(duplicate_code);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"extract the shared logic into one function the copies can call","spans":[]},{"level":"note","message":"also found at: crates/beta/src/render.rs:88","spans":[]},{"level":"note","message":"matching ignores local variable names and literal values","spans":[]},{"level":"note","message":"instances are identical (differing at most in local names)","spans":[]},{"level":"note","message":"instances resolve different callees — the copies may not be interchangeable","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn duplicate_code_run_signature() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_run_signature")), @r#"{"level":"warning","message":"duplicated code: 2 structurally identical instances (~6 lines)","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(duplicate_code);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"extract the shared logic into one function the copies can call","spans":[]},{"level":"note","message":"also found at: crates/beta/src/render.rs:88","spans":[]},{"level":"note","message":"matching ignores local variable names and literal values","spans":[]},{"level":"note","message":"instances are identical (differing at most in local names)","spans":[]},{"level":"note","message":"an extracted fn would take 2 parameters (items, config) and return total","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn duplicate_code_run_live_out_downgrade() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_run_live_out_downgrade")), @r#"{"level":"warning","message":"duplicated code: 2 structurally identical instances (~7 lines)","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(duplicate_code);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"extract the shared logic into one function the copies can call","spans":[]},{"level":"note","message":"also found at: crates/beta/src/render.rs:88","spans":[]},{"level":"note","message":"matching ignores local variable names and literal values","spans":[]},{"level":"note","message":"instances are identical (differing at most in local names)","spans":[]},{"level":"note","message":"an extracted fn would take 1 parameter (items) but needs 3 return values (count, total, errors) — extraction is awkward; consider restructuring","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn duplicate_code_baseline_grew() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_grew")), @r#"{"level":"warning","message":"duplicated code: 3 structurally identical instances (~8 lines)","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/src/report.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(duplicate_code);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"extract the shared logic into one function the copies can call","spans":[]},{"level":"note","message":"also found at: crates/beta/src/render.rs:88, crates/gamma/src/emit.rs:17","spans":[]},{"level":"note","message":"matching ignores local variable names and literal values","spans":[]},{"level":"note","message":"grew beyond its baseline: 2 instances accepted, now 3","spans":[]},{"level":"note","message":"instances are identical (differing at most in local names)","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn duplicate_code_baseline_stale() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_stale")), @r##"{"level":"warning","message":"stale duplicate-code baseline entry: no clone group matches fingerprint 9930bf3835a56614 (was crates/alpha/src/report.rs, 2 instances)","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"duplicate-code.baseline.toml","byte_start":0,"byte_end":0,"line_start":8,"line_end":8,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"duplicate-code.baseline.toml","byte_start":0,"byte_end":0,"line_start":8,"line_end":8,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(duplicate-code)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"regenerate with `workspace-lint --baseline-write` (or delete this entry)","spans":[]},{"level":"note","message":"the duplication was resolved, or the code changed enough to re-fingerprint","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn duplicate_code_baseline_overcount() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_overcount")), @r##"{"level":"warning","message":"duplicate-code baseline entry 9930bf3835a56614 records 3 instances but only 2 remain","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"duplicate-code.baseline.toml","byte_start":0,"byte_end":0,"line_start":8,"line_end":8,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"duplicate-code.baseline.toml","byte_start":0,"byte_end":0,"line_start":8,"line_end":8,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(duplicate-code)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"ratchet down: regenerate with `workspace-lint --baseline-write`","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn duplicate_code_baseline_missing() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_missing")), @r##"{"level":"warning","message":"duplicate-code baseline file `duplicate-code.baseline.toml` not found","code":{"code":"workspace-lint::duplicate-code","explanation":null},"spans":[{"file_name":"duplicate-code.baseline.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"duplicate-code.baseline.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(duplicate-code)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"generate it with `workspace-lint --baseline-write`, or remove `baseline` from [duplicate-code]","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn cli_crate_version_rule_error() {
            insta::assert_snapshot!(render(&scenario("cli_crate_version_rule_error")), @r##"{"level":"warning","message":"pattern `v(\\d+)` did not match the output of `wasm-bindgen --version`","code":{"code":"workspace-lint::cli-crate-version","explanation":null},"spans":[],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(cli-crate-version)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"the regex must capture the version in group 1","spans":[]},{"level":"note","message":"ran `wasm-bindgen --version`","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn unused_deps_one() {
            insta::assert_snapshot!(render(&scenario("unused_deps_one")), @r##"{"level":"warning","message":"1 possibly unused dependency in crates/alpha/Cargo.toml","code":{"code":"workspace-lint::unused-deps","explanation":null},"spans":[{"file_name":"crates/alpha/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(unused-deps)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"[dependencies] rand","spans":[]},{"level":"note","message":"build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives","spans":[]},{"level":"note","message":"verify by removing the dep and running `cargo build --all-targets`","spans":[]},{"level":"note","message":"if the build breaks, add the dep to [unused-deps] ignore in your config","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn unused_deps_multiple() {
            insta::assert_snapshot!(render(&scenario("unused_deps_multiple")), @r##"{"level":"warning","message":"2 possibly unused dependencies in crates/beta/Cargo.toml","code":{"code":"workspace-lint::unused-deps","explanation":null},"spans":[{"file_name":"crates/beta/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/beta/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(unused-deps)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"[dependencies] foo","spans":[]},{"level":"help","message":"[dev-dependencies] bar","spans":[]},{"level":"note","message":"build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives","spans":[]},{"level":"note","message":"verify by removing the dep and running `cargo build --all-targets`","spans":[]},{"level":"note","message":"if the build breaks, add the dep to [unused-deps] ignore in your config","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn unused_deps_dirty_manifest() {
            insta::assert_snapshot!(render(&scenario("unused_deps_dirty_manifest")), @r##"{"level":"warning","message":"1 possibly unused dependency in crates/alpha/Cargo.toml","code":{"code":"workspace-lint::unused-deps","explanation":null},"spans":[{"file_name":"crates/alpha/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(unused-deps)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"[dependencies] rand","spans":[]},{"level":"note","message":"file `crates/alpha/Cargo.toml` is untracked or has uncommitted changes; `--fix-auto-delete` will not delete it (commit first or use `git stash`)","spans":[]},{"level":"note","message":"build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives","spans":[]},{"level":"note","message":"verify by removing the dep and running `cargo build --all-targets`","spans":[]},{"level":"note","message":"if the build breaks, add the dep to [unused-deps] ignore in your config","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn unused_pub_test_only() {
            insta::assert_snapshot!(render(&scenario("unused_pub_test_only")), @r#"{"level":"warning","message":"pub fn `helper` in crate `mycrate` is only used by test code","code":{"code":"workspace-lint::unused-pub","explanation":null},"spans":[{"file_name":"crates/mycrate/src/lib.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/mycrate/src/lib.rs","byte_start":0,"byte_end":0,"line_start":42,"line_end":42,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(unused_pub);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"gate it `#[cfg(test)]`, move it into test code, or remove it","spans":[]},{"level":"note","message":"code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives","spans":[]},{"level":"note","message":"no fix is auto-applied: `pub(crate)` would trip `dead_code` on the non-test build, and deleting the item would break the tests that reference it","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn unused_pub_test_only_blocked() {
            insta::assert_snapshot!(render(&scenario("unused_pub_test_only_blocked")), @r#"{"level":"warning","message":"pub fn `embalmed` in crate `alpha` is only used by test code","code":{"code":"workspace-lint::unused-pub","explanation":null},"spans":[{"file_name":"crates/alpha/src/lib.rs","byte_start":0,"byte_end":0,"line_start":6,"line_end":6,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/src/lib.rs","byte_start":0,"byte_end":0,"line_start":6,"line_end":6,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(unused_pub);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"gate it `#[cfg(test)]`, move it into test code, or remove it","spans":[]},{"level":"note","message":"code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives","spans":[]},{"level":"note","message":"only test code references it, but test item `beta::tests::covers_both` (crates/beta/src/main.rs:8) also exercises surviving `alpha::kept` — deleting would orphan that test; update or remove the test first, or delete both by hand","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn unused_pub_test_scaffold() {
            insta::assert_snapshot!(render(&scenario("unused_pub_test_scaffold")), @r#"{"level":"warning","message":"test fn `exercises_embalmed` in crate `beta` only exercises items deleted by this `--fix`","code":{"code":"workspace-lint::unused-pub","explanation":null},"spans":[{"file_name":"crates/beta/src/main.rs","byte_start":0,"byte_end":0,"line_start":12,"line_end":12,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/beta/src/main.rs","byte_start":0,"byte_end":0,"line_start":12,"line_end":12,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(unused_pub);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"deleting it too — it would reference deleted items and break the test build","spans":[]},{"level":"note","message":"exclusive test scaffolding: every workspace item it references is also deleted by this `--fix`","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn unused_pub_tighten_visibility() {
            insta::assert_snapshot!(render(&scenario("unused_pub_tighten_visibility")), @r#"{"level":"warning","message":"pub struct `Builder` in crate `mycrate` is only used inside the crate","code":{"code":"workspace-lint::unused-pub","explanation":null},"spans":[{"file_name":"crates/mycrate/src/builder.rs","byte_start":0,"byte_end":0,"line_start":7,"line_end":7,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/mycrate/src/builder.rs","byte_start":0,"byte_end":0,"line_start":7,"line_end":7,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(unused_pub);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"consider `pub(crate)` to tighten visibility","spans":[]},{"level":"note","message":"code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn unused_pub_publish_hint() {
            insta::assert_snapshot!(render(&scenario("unused_pub_publish_hint")), @r##"{"level":"warning","message":"crate `mycrate` has 3 public items unused within the workspace","code":{"code":"workspace-lint::unused-pub","explanation":null},"spans":[{"file_name":"crates/mycrate/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/mycrate/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(unused-pub)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"if `mycrate` is published outside this workspace, set `publish = true` in its Cargo.toml to treat its public API as external (these findings become exempt)","spans":[]},{"level":"note","message":"workspace-lint treats a crate as workspace-internal unless it declares `publish = true` (or a registry); see the unused-pub docs","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn unused_pub_tighten_unmask() {
            insta::assert_snapshot!(render(&scenario("unused_pub_tighten_unmask")), @r#"{"level":"warning","message":"pub struct `Rect` in crate `mycrate` is only used inside the crate","code":{"code":"workspace-lint::unused-pub","explanation":null},"spans":[{"file_name":"crates/mycrate/src/geometry.rs","byte_start":0,"byte_end":0,"line_start":11,"line_end":11,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/mycrate/src/geometry.rs","byte_start":0,"byte_end":0,"line_start":11,"line_end":11,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(unused_pub);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"consider `pub(crate)` to tighten visibility","spans":[]},{"level":"note","message":"code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives","spans":[]},{"level":"note","message":"`pub(crate)` would unmask clippy `wrong_self_convention` on `is_wide` (clippy exempts exported items via `avoid-breaking-exported-api`) — resolve that first or narrow by hand","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn unused_pub_private_collateral() {
            insta::assert_snapshot!(render(&scenario("unused_pub_private_collateral")), @r#"{"level":"warning","message":"private fn `helper` in crate `mycrate` loses its last user in this `--fix`","code":{"code":"workspace-lint::unused-pub","explanation":null},"spans":[{"file_name":"crates/mycrate/src/lib.rs","byte_start":0,"byte_end":0,"line_start":21,"line_end":21,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/mycrate/src/lib.rs","byte_start":0,"byte_end":0,"line_start":21,"line_end":21,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(unused_pub);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"deleting it too — rustc `dead_code` would flag it on the fixed tree","spans":[]},{"level":"note","message":"transitively dead: the only item(s) that referenced it are also deleted by this `--fix`","spans":[]}],"rendered":null}"#);
        }
    }

    // -------------------------------------------------------------------------
    // GITHUB renderer — one workflow-command line per diagnostic. Reviewer
    // scrolls this section to validate the format GitHub Actions consumes for
    // PR annotations.
    // -------------------------------------------------------------------------

    mod github_snapshots {
        use super::*;

        fn render(d: &Diagnostic) -> String {
            let mut buf = Vec::new();
            render_one(Format::Github, d, &mut buf).unwrap();
            String::from_utf8(buf).unwrap()
        }

        fn scenario(name: &str) -> Diagnostic {
            scenarios()
                .into_iter()
                .find(|(n, _)| *n == name)
                .map(|(_, d)| d)
                .unwrap()
        }

        #[test]
        fn centralized_deps_one_dep() {
            insta::assert_snapshot!(render(&scenario("centralized_deps_one_dep")), @"::warning file=crates/alpha/Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Acentralized-deps::1 dependency in crates/alpha/Cargo.toml should use `workspace = true`");
        }

        #[test]
        fn file_size_over_limit() {
            insta::assert_snapshot!(render(&scenario("file_size_over_limit")), @"::warning file=crates/web-api/src/handler.rs,line=1,col=1,title=workspace-lint%3A%3Afile-size::file exceeds 500 code lines (612)");
        }

        #[test]
        fn crate_size_over_limit() {
            insta::assert_snapshot!(render(&scenario("crate_size_over_limit")), @"::warning file=crates/legacy/Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Acrate-size::crate exceeds 5000 code lines (7321)");
        }

        #[test]
        fn duplicate_code_group() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_group")), @"::warning file=crates/alpha/src/report.rs,line=42,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicated code: 3 structurally identical instances (~14 lines)");
        }

        #[test]
        fn duplicate_code_merge_identical_fns() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_merge_identical_fns")), @"::warning file=crates/alpha/src/report.rs,line=42,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicated code: 2 structurally identical instances (~8 lines)");
        }

        #[test]
        fn duplicate_code_delete_dead_copy() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_delete_dead_copy")), @"::warning file=crates/alpha/src/report.rs,line=42,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicated code: 2 structurally identical instances (~8 lines)");
        }

        #[test]
        fn duplicate_code_default_trait_method() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_default_trait_method")), @"::warning file=crates/alpha/src/report.rs,line=42,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicated code: 2 structurally identical instances (~6 lines)");
        }

        #[test]
        fn duplicate_code_method_on_receiver_type() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_method_on_receiver_type")), @"::warning file=crates/alpha/src/report.rs,line=42,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicated code: 2 structurally identical instances (~5 lines)");
        }

        #[test]
        fn duplicate_code_ui_component() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_ui_component")), @"::warning file=crates/alpha/src/report.rs,line=42,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicated code: 2 structurally identical instances (~4 lines)");
        }

        #[test]
        fn duplicate_code_merge_withheld() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_merge_withheld")), @"::warning file=crates/alpha/src/report.rs,line=42,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicated code: 2 structurally identical instances (~6 lines)");
        }

        #[test]
        fn duplicate_code_run_signature() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_run_signature")), @"::warning file=crates/alpha/src/report.rs,line=42,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicated code: 2 structurally identical instances (~6 lines)");
        }

        #[test]
        fn duplicate_code_run_live_out_downgrade() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_run_live_out_downgrade")), @"::warning file=crates/alpha/src/report.rs,line=42,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicated code: 2 structurally identical instances (~7 lines)");
        }

        #[test]
        fn duplicate_code_baseline_grew() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_grew")), @"::warning file=crates/alpha/src/report.rs,line=42,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicated code: 3 structurally identical instances (~8 lines)");
        }

        #[test]
        fn duplicate_code_baseline_stale() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_stale")), @"::warning file=duplicate-code.baseline.toml,line=8,col=1,title=workspace-lint%3A%3Aduplicate-code::stale duplicate-code baseline entry: no clone group matches fingerprint 9930bf3835a56614 (was crates/alpha/src/report.rs, 2 instances)");
        }

        #[test]
        fn duplicate_code_baseline_overcount() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_overcount")), @"::warning file=duplicate-code.baseline.toml,line=8,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicate-code baseline entry 9930bf3835a56614 records 3 instances but only 2 remain");
        }

        #[test]
        fn duplicate_code_baseline_missing() {
            insta::assert_snapshot!(render(&scenario("duplicate_code_baseline_missing")), @"::warning file=duplicate-code.baseline.toml,line=1,col=1,title=workspace-lint%3A%3Aduplicate-code::duplicate-code baseline file `duplicate-code.baseline.toml` not found");
        }

        #[test]
        fn cli_crate_version_mismatch() {
            insta::assert_snapshot!(render(&scenario("cli_crate_version_mismatch")), @"::warning file=Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Acli-crate-version::`wasm-bindgen` CLI version 0.2.89 does not match Cargo.lock 0.2.90");
        }

        #[test]
        fn cli_crate_version_rule_error() {
            insta::assert_snapshot!(render(&scenario("cli_crate_version_rule_error")), @r"::warning file=Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Acli-crate-version::pattern `v(\d+)` did not match the output of `wasm-bindgen --version`");
        }

        #[test]
        fn unused_deps_one() {
            insta::assert_snapshot!(render(&scenario("unused_deps_one")), @"::warning file=crates/alpha/Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Aunused-deps::1 possibly unused dependency in crates/alpha/Cargo.toml");
        }

        #[test]
        fn unused_deps_dirty_manifest() {
            insta::assert_snapshot!(render(&scenario("unused_deps_dirty_manifest")), @"::warning file=crates/alpha/Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Aunused-deps::1 possibly unused dependency in crates/alpha/Cargo.toml");
        }

        #[test]
        fn unused_pub_removal_candidate() {
            insta::assert_snapshot!(render(&scenario("unused_pub_removal_candidate")), @"::warning file=crates/mycrate/src/lib.rs,line=42,col=1,title=workspace-lint%3A%3Aunused-pub::pub fn `helper` in crate `mycrate` appears unused — consider removing");
        }

        #[test]
        fn unused_pub_cascade_transitive() {
            insta::assert_snapshot!(render(&scenario("unused_pub_cascade_transitive")), @"::warning file=crates/mycrate/src/inner.rs,line=14,col=1,title=workspace-lint%3A%3Aunused-pub::pub fn `helper` in crate `mycrate` appears unused — consider removing");
        }

        #[test]
        fn unused_pub_import_surgery() {
            insta::assert_snapshot!(render(&scenario("unused_pub_import_surgery")), @"::warning file=crates/mycrate/src/lib.rs,line=3,col=1,title=workspace-lint%3A%3Aunused-pub::unused import of a removed item");
        }

        #[test]
        fn unused_pub_test_only() {
            insta::assert_snapshot!(render(&scenario("unused_pub_test_only")), @"::warning file=crates/mycrate/src/lib.rs,line=42,col=1,title=workspace-lint%3A%3Aunused-pub::pub fn `helper` in crate `mycrate` is only used by test code");
        }

        #[test]
        fn unused_pub_test_only_blocked() {
            insta::assert_snapshot!(render(&scenario("unused_pub_test_only_blocked")), @"::warning file=crates/alpha/src/lib.rs,line=6,col=1,title=workspace-lint%3A%3Aunused-pub::pub fn `embalmed` in crate `alpha` is only used by test code");
        }

        #[test]
        fn unused_pub_test_scaffold() {
            insta::assert_snapshot!(render(&scenario("unused_pub_test_scaffold")), @"::warning file=crates/beta/src/main.rs,line=12,col=1,title=workspace-lint%3A%3Aunused-pub::test fn `exercises_embalmed` in crate `beta` only exercises items deleted by this `--fix`");
        }

        #[test]
        fn unused_pub_tighten_visibility() {
            insta::assert_snapshot!(render(&scenario("unused_pub_tighten_visibility")), @"::warning file=crates/mycrate/src/builder.rs,line=7,col=1,title=workspace-lint%3A%3Aunused-pub::pub struct `Builder` in crate `mycrate` is only used inside the crate");
        }

        #[test]
        fn unused_pub_publish_hint() {
            insta::assert_snapshot!(render(&scenario("unused_pub_publish_hint")), @"::warning file=crates/mycrate/Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Aunused-pub::crate `mycrate` has 3 public items unused within the workspace");
        }

        #[test]
        fn unused_pub_tighten_unmask() {
            insta::assert_snapshot!(render(&scenario("unused_pub_tighten_unmask")), @"::warning file=crates/mycrate/src/geometry.rs,line=11,col=1,title=workspace-lint%3A%3Aunused-pub::pub struct `Rect` in crate `mycrate` is only used inside the crate");
        }

        #[test]
        fn unused_pub_private_collateral() {
            insta::assert_snapshot!(render(&scenario("unused_pub_private_collateral")), @"::warning file=crates/mycrate/src/lib.rs,line=21,col=1,title=workspace-lint%3A%3Aunused-pub::private fn `helper` in crate `mycrate` loses its last user in this `--fix`");
        }

        #[test]
        fn stale_expect() {
            insta::assert_snapshot!(render(&scenario("stale_expect")), @"::warning file=crates/api/src/lib.rs,line=1,col=1,title=workspace-lint%3A%3Astale-expect::expect directive for `file-size` did not match any diagnostic");
        }

        #[test]
        fn architecture_denied_import() {
            insta::assert_snapshot!(render(&scenario("architecture_denied_import")), @"::warning file=crates/apps-foo/src/lib.rs,line=7,col=1,title=workspace-lint%3A%3Aarchitecture::import of `data_models::internal::User` from `apps-foo` violates architecture rule `no-internal-imports`");
        }

        #[test]
        fn architecture_denied_code_reference() {
            insta::assert_snapshot!(render(&scenario("architecture_denied_code_reference")), @"::warning file=crates/apps-foo/src/lib.rs,line=12,col=1,title=workspace-lint%3A%3Aarchitecture::reference to `data_models::internal::User` from `apps-foo` violates architecture rule `no-internal-imports`");
        }

        #[test]
        fn orphan_file_orphan() {
            insta::assert_snapshot!(render(&scenario("orphan_file_orphan")), @"::warning file=crates/demo/src/orphan.rs,line=1,col=1,title=workspace-lint%3A%3Aorphan-file::orphan source file `src/orphan.rs` is never compiled");
        }

        #[test]
        fn orphan_file_cfg_coverage_gap() {
            insta::assert_snapshot!(render(&scenario("orphan_file_cfg_coverage_gap")), @"::warning file=crates/demo/src/imp_windows.rs,line=1,col=1,title=workspace-lint%3A%3Aorphan-file::no declared `[engine]` config compiles `src/imp_windows.rs`");
        }

        #[test]
        fn feature_drift_declared_never_gated() {
            insta::assert_snapshot!(render(&scenario("feature_drift_declared_never_gated")), @"::warning file=crates/demo/Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Afeature-drift::feature `experimental` is declared in `[features]` but never gated in source");
        }

        #[test]
        fn feature_drift_gated_undeclared() {
            insta::assert_snapshot!(render(&scenario("feature_drift_gated_undeclared")), @"::warning file=crates/demo/Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Afeature-drift::feature `nightly` is gated in source but not declared in `[features]`");
        }

        #[test]
        fn config_unknown_key() {
            insta::assert_snapshot!(render(&scenario("config_unknown_key")), @"::warning file=.workspace-lint.toml,line=1,col=1,title=workspace-lint%3A%3Aconfig::unknown configuration section `file-siz`");
        }

        #[test]
        fn unknown_lint_in_lints_table() {
            insta::assert_snapshot!(render(&scenario("unknown_lint_in_lints_table")), @"::warning file=.workspace-lint.toml,line=1,col=1,title=workspace-lint%3A%3Aunknown-lint::unknown lint `unused-dep` in `[lints]`");
        }

        #[test]
        fn deny_level_uses_error_command() {
            // Severity flip — the only non-message variation worth covering
            // for the GitHub renderer.
            let d = at_workspace("workspace-lint::cli-crate-version", "blocking")
                .level_explicit(wl_diagnostic::Level::Deny)
                .build();
            let mut buf = Vec::new();
            render_one(Format::Github, &d, &mut buf).unwrap();
            let s = String::from_utf8(buf).unwrap();
            insta::assert_snapshot!(s, @"::error file=Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Acli-crate-version::blocking");
        }
    }
}

/// The engine's preflight / failure texts — the other user-facing message
/// surface. These print verbatim to stderr via `util::fail` (never through
/// the diagnostic renderers), so they're audited here the same way the
/// diagnostics are: read the snapshot, see exactly what the user sees.
/// The remediation commands are load-bearing — a user without the pinned
/// toolchain must be able to paste their way out.
#[cfg(test)]
mod engine_errors {
    use wl_engine::EngineError;

    #[test]
    fn toolchain_missing() {
        let e = EngineError::ToolchainMissing {
            pin: "nightly-2026-04-16".into(),
        };
        insta::assert_snapshot!(e.to_string(), @r"
        the full tier extracts IR inside a rustc build and needs the pinned toolchain

        rustup toolchain install nightly-2026-04-16 --profile minimal \
        --component rustc-dev --component llvm-tools-preview

        hint: `--fast-only` runs the build-free lints without any toolchain
        ");
    }

    #[test]
    fn component_missing() {
        let e = EngineError::ComponentMissing {
            pin: "nightly-2026-04-16".into(),
            component: "rustc-dev".into(),
        };
        insta::assert_snapshot!(e.to_string(), @r"
        the pinned toolchain nightly-2026-04-16 is installed but misses the `rustc-dev` component

        rustup component add rustc-dev --toolchain nightly-2026-04-16
        ");
    }

    #[test]
    fn dylint_link_missing() {
        insta::assert_snapshot!(EngineError::DylintLinkMissing.to_string(), @r"
        `dylint-link` is not on PATH — the extractor dylib links through it

        cargo install dylint-link --locked
        ");
    }

    /// The analyzed workspace failed to compile under a config. The cargo
    /// diagnostics are replayed verbatim above this line (they ARE the
    /// diagnosis); this is the trailer that names the failing config.
    #[test]
    fn workspace_compile_failure_names_the_config() {
        let e = EngineError::Incomplete {
            config: "tests".into(),
            missing: vec!["demo+test.wlir".into()],
        };
        insta::assert_snapshot!(e.to_string(), @r#"
        IR incomplete under config `tests`: fragments still missing after a forced re-lint: ["demo+test.wlir"]

        hint: if this persists, delete `target/dylint` in the analyzed workspace to reset the engine's build cache
        "#);
    }
}
