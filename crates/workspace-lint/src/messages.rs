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
//! - [`human`] : clippy-style text written to stderr.
//! - [`json`] : `--message-format=json`, rustc-compatible per-line records.
//! - [`github`] : `--message-format=github`, Actions workflow command.
//!
//! Add a new scenario by extending [`scenarios`] with one builder per case,
//! then re-running `cargo test` — insta will prompt you to accept the new
//! snapshot lines.

use std::path::PathBuf;

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::{at_crate, at_file, at_line, at_workspace};

/// Every distinct diagnostic the tool can emit, in a fixed order. The order
/// here is what the snapshot tests assert against, so think of this as the
/// canonical user-facing surface.
pub fn scenarios() -> Vec<(&'static str, Diagnostic)> {
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
            .help("split #[cfg(test)] modules into separate test files")
            .help("extract related structs, enums, or trait impls into their own modules")
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
        // freshness: tracked file stale relative to deps.
        (
            "freshness_stale",
            at_file(
                "workspace-lint::freshness",
                "`crates/api/CLAUDE.md` is older than source files it depends on",
                PathBuf::from("crates/api/CLAUDE.md"),
            )
            .help("files matching `**/*.rs` in the subtree are newer")
            .help("run `workspace-lint done` once the tracked file is up to date")
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
        // unused-deps: single unused dep.
        (
            "unused_deps_one",
            at_crate(
                "workspace-lint::unused-deps",
                "1 possibly unused dependency in crates/alpha/Cargo.toml",
                PathBuf::from("crates/alpha"),
            )
            .help("[dependencies] rand")
            .note("proc-macro crates and build.rs-generated code may cause false positives")
            .note("verify by removing the dep and running `cargo build --all-targets`")
            .note("if the build breaks, add the dep to [unused-deps] ignore in your config")
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
            .note("proc-macro crates and build.rs-generated code may cause false positives")
            .note("verify by removing the dep and running `cargo build --all-targets`")
            .note("if the build breaks, add the dep to [unused-deps] ignore in your config")
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
            .note(
                "#[cfg]-gated items, proc-macro usage, and re-exports may cause false positives",
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
            .note(
                "#[cfg]-gated items, proc-macro usage, and re-exports may cause false positives",
            )
            .build(),
        ),
        // stale-expect: an `expect!` directive that didn't fire.
        (
            "stale_expect",
            at_line(
                "workspace-lint::stale-expect",
                "expect directive for `file-size` did not match any diagnostic",
                PathBuf::from("crates/api/src/lib.rs"),
                1,
            )
            .help("remove this expect — the lint it tracks is no longer firing")
            .note("a stale expect usually means the underlying issue has been fixed")
            .build(),
        ),
        // stale-git-index: file deleted from disk but still tracked.
        (
            "stale_git_index",
            at_workspace(
                "workspace-lint::stale-git-index",
                "deleted file `crates/old/src/legacy.rs` is still tracked by git",
            )
            .help("run `git rm crates/old/src/legacy.rs` to stage the removal")
            .build(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostic::render::{github, human, json};

    // -------------------------------------------------------------------------
    // HUMAN renderer — clippy-style text. Reviewer scrolls this section to
    // validate the diagnostic *prose* the user sees in their terminal.
    // -------------------------------------------------------------------------

    mod human_snapshots {
        use super::*;

        fn render(d: &Diagnostic) -> String {
            let mut buf = Vec::new();
            human::write_one(d, &mut buf).unwrap();
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
            1 + # workspace-lint: allow(centralized-deps)
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
            1 + # workspace-lint: allow(centralized-deps)
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
              = help: split #[cfg(test)] modules into separate test files
              = help: extract related structs, enums, or trait impls into their own modules
              = note: configured by [[file-size.rules]] glob = "**/*.rs"
            help: if intentional, silence with:
              |
            1 + workspace_lint::allow!(file_size);
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
            1 + # workspace-lint: allow(crate-size)
              |
              = note: `#[warn(workspace_lint::crate_size)]` on by default
            "#);
        }

        #[test]
        fn freshness_stale() {
            insta::assert_snapshot!(render(&scenario("freshness_stale")), @r#"
            warning: `crates/api/CLAUDE.md` is older than source files it depends on
             --> crates/api/CLAUDE.md:1:1
              |
              = help: files matching `**/*.rs` in the subtree are newer
              = help: run `workspace-lint done` once the tracked file is up to date
            help: if intentional, silence with:
              |
            1 + # workspace-lint: allow(freshness)
              |
              = note: `#[warn(workspace_lint::freshness)]` on by default
            "#);
        }

        #[test]
        fn cli_crate_version_mismatch() {
            insta::assert_snapshot!(render(&scenario("cli_crate_version_mismatch")), @r#"
            warning: `wasm-bindgen` CLI version 0.2.89 does not match Cargo.lock 0.2.90
              = help: update or reinstall `wasm-bindgen` to match the workspace version
              = note: ran `wasm-bindgen --version`
            help: if intentional, silence with:
              |
            1 + # workspace-lint: allow(cli-crate-version)
              |
              = note: `#[warn(workspace_lint::cli_crate_version)]` on by default
            "#);
        }

        #[test]
        fn unused_deps_one() {
            insta::assert_snapshot!(render(&scenario("unused_deps_one")), @r#"
            warning: 1 possibly unused dependency in crates/alpha/Cargo.toml
             --> crates/alpha/Cargo.toml:1:1
              |
              = help: [dependencies] rand
              = note: proc-macro crates and build.rs-generated code may cause false positives
              = note: verify by removing the dep and running `cargo build --all-targets`
              = note: if the build breaks, add the dep to [unused-deps] ignore in your config
            help: if intentional, silence with:
              |
            1 + # workspace-lint: allow(unused-deps)
              |
              = note: `#[warn(workspace_lint::unused_deps)]` on by default
            "#);
        }

        #[test]
        fn unused_deps_multiple() {
            insta::assert_snapshot!(render(&scenario("unused_deps_multiple")), @r#"
            warning: 2 possibly unused dependencies in crates/beta/Cargo.toml
             --> crates/beta/Cargo.toml:1:1
              |
              = help: [dependencies] foo
              = help: [dev-dependencies] bar
              = note: proc-macro crates and build.rs-generated code may cause false positives
              = note: verify by removing the dep and running `cargo build --all-targets`
              = note: if the build breaks, add the dep to [unused-deps] ignore in your config
            help: if intentional, silence with:
              |
            1 + # workspace-lint: allow(unused-deps)
              |
              = note: `#[warn(workspace_lint::unused_deps)]` on by default
            "#);
        }

        #[test]
        fn unused_pub_removal_candidate() {
            insta::assert_snapshot!(render(&scenario("unused_pub_removal_candidate")), @r#"
            warning: pub fn `helper` in crate `mycrate` appears unused — consider removing
             --> crates/mycrate/src/lib.rs:42:1
              |
              = help: remove the item or its `pub` visibility
              = note: #[cfg]-gated items, proc-macro usage, and re-exports may cause false positives
            help: if intentional, silence with:
              |
            42 + workspace_lint::allow!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            "#);
        }

        #[test]
        fn unused_pub_tighten_visibility() {
            insta::assert_snapshot!(render(&scenario("unused_pub_tighten_visibility")), @r#"
            warning: pub struct `Builder` in crate `mycrate` is only used inside the crate
             --> crates/mycrate/src/builder.rs:7:1
              |
              = help: consider `pub(crate)` to tighten visibility
              = note: #[cfg]-gated items, proc-macro usage, and re-exports may cause false positives
            help: if intentional, silence with:
              |
            7 + workspace_lint::allow!(unused_pub);
              |
              = note: `#[warn(workspace_lint::unused_pub)]` on by default
            "#);
        }

        #[test]
        fn stale_expect() {
            insta::assert_snapshot!(render(&scenario("stale_expect")), @r#"
            warning: expect directive for `file-size` did not match any diagnostic
             --> crates/api/src/lib.rs:1:1
              |
              = help: remove this expect — the lint it tracks is no longer firing
              = note: a stale expect usually means the underlying issue has been fixed
            help: if intentional, silence with:
              |
            1 + workspace_lint::allow!(stale_expect);
              |
              = note: `#[warn(workspace_lint::stale_expect)]` on by default
            "#);
        }

        #[test]
        fn stale_git_index() {
            insta::assert_snapshot!(render(&scenario("stale_git_index")), @r#"
            warning: deleted file `crates/old/src/legacy.rs` is still tracked by git
              = help: run `git rm crates/old/src/legacy.rs` to stage the removal
            help: if intentional, silence with:
              |
            1 + # workspace-lint: allow(stale-git-index)
              |
              = note: `#[warn(workspace_lint::stale_git_index)]` on by default
            "#);
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
            json::write(std::slice::from_ref(d), &mut buf).unwrap();
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
            insta::assert_snapshot!(render(&scenario("centralized_deps_one_dep")), @r##"{"level":"warning","message":"1 dependency in crates/alpha/Cargo.toml should use `workspace = true`","code":{"code":"workspace-lint::centralized-deps","explanation":null},"spans":[{"file_name":"crates/alpha/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/alpha/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: allow(centralized-deps)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"[dependencies] serde: has own version \"1.0.200\" — use { workspace = true } instead","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn file_size_over_limit() {
            // Validates the most important JSON contract: a Rust file
            // diagnostic carries a suggested_replacement with the `allow!`
            // macro text so the IDE quick-fix Just Works.
            insta::assert_snapshot!(render(&scenario("file_size_over_limit")), @r##"{"level":"warning","message":"file exceeds 500 code lines (612)","code":{"code":"workspace-lint::file-size","explanation":null},"spans":[{"file_name":"crates/web-api/src/handler.rs","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/web-api/src/handler.rs","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::allow!(file_size);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"split #[cfg(test)] modules into separate test files","spans":[]},{"level":"help","message":"extract related structs, enums, or trait impls into their own modules","spans":[]},{"level":"note","message":"configured by [[file-size.rules]] glob = \"**/*.rs\"","spans":[]}],"rendered":null}"##);
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
                "workspace_lint::allow!(unused_pub);\n"
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
            github::write_one(d, &mut buf).unwrap();
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
        fn freshness_stale() {
            insta::assert_snapshot!(render(&scenario("freshness_stale")), @"::warning file=crates/api/CLAUDE.md,line=1,col=1,title=workspace-lint%3A%3Afreshness::`crates/api/CLAUDE.md` is older than source files it depends on");
        }

        #[test]
        fn cli_crate_version_mismatch() {
            insta::assert_snapshot!(render(&scenario("cli_crate_version_mismatch")), @"::warning file=Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Acli-crate-version::`wasm-bindgen` CLI version 0.2.89 does not match Cargo.lock 0.2.90");
        }

        #[test]
        fn unused_deps_one() {
            insta::assert_snapshot!(render(&scenario("unused_deps_one")), @"::warning file=crates/alpha/Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Aunused-deps::1 possibly unused dependency in crates/alpha/Cargo.toml");
        }

        #[test]
        fn unused_pub_removal_candidate() {
            insta::assert_snapshot!(render(&scenario("unused_pub_removal_candidate")), @"::warning file=crates/mycrate/src/lib.rs,line=42,col=1,title=workspace-lint%3A%3Aunused-pub::pub fn `helper` in crate `mycrate` appears unused — consider removing");
        }

        #[test]
        fn unused_pub_tighten_visibility() {
            insta::assert_snapshot!(render(&scenario("unused_pub_tighten_visibility")), @"::warning file=crates/mycrate/src/builder.rs,line=7,col=1,title=workspace-lint%3A%3Aunused-pub::pub struct `Builder` in crate `mycrate` is only used inside the crate");
        }

        #[test]
        fn stale_expect() {
            insta::assert_snapshot!(render(&scenario("stale_expect")), @"::warning file=crates/api/src/lib.rs,line=1,col=1,title=workspace-lint%3A%3Astale-expect::expect directive for `file-size` did not match any diagnostic");
        }

        #[test]
        fn stale_git_index() {
            insta::assert_snapshot!(render(&scenario("stale_git_index")), @"::warning file=Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Astale-git-index::deleted file `crates/old/src/legacy.rs` is still tracked by git");
        }

        #[test]
        fn deny_level_uses_error_command() {
            // Severity flip — the only non-message variation worth covering
            // for the GitHub renderer.
            let d = at_workspace("workspace-lint::cli-crate-version", "blocking")
                .level(crate::diagnostic::Level::Deny)
                .build();
            let mut buf = Vec::new();
            github::write_one(&d, &mut buf).unwrap();
            let s = String::from_utf8(buf).unwrap();
            insta::assert_snapshot!(s, @"::error file=Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Acli-crate-version::blocking");
        }
    }
}
