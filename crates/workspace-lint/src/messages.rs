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

use crate::diagnostic::builder::{at_crate, at_file, at_line, at_workspace};
use crate::diagnostic::{Applicability, Diagnostic, Span, Suggestion};

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
            .note("build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives")
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
            .note("build.rs-generated code, *-sys link-only deps, and feature-plumbing-only deps may still cause false positives")
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
                "code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives",
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
                "code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives",
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
            .note(
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
            .note("a stale expect usually means the underlying issue has been fixed")
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
            .note("internal types are not part of the published API surface")
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
            .note("internal types are not part of the published API surface")
            .build(),
        ),
        // module-tree: a `mod foo;` declaration with no backing file.
        (
            "module_tree_broken_mod_decl",
            at_line(
                "workspace-lint::module-tree",
                "`mod missing` declared but no `missing.rs` or `missing/mod.rs` found",
                PathBuf::from("crates/demo/src/lib.rs"),
                3,
            )
            .help(
                "create `missing.rs` adjacent to this file, or `missing/mod.rs`, or add a `#[path = \"…\"]` attribute",
            )
            .note("`mod foo;` with no inline body must resolve to a source file")
            .build(),
        ),
        // module-tree: an orphan source file not reachable from any `mod`.
        (
            "module_tree_orphan_file",
            at_file(
                "workspace-lint::module-tree",
                "orphan source file `src/orphan.rs` is not reachable from any `mod` declaration",
                PathBuf::from("crates/demo/src/orphan.rs"),
            )
            .help(
                "add `mod orphan;` (or a `#[path = \"src/orphan.rs\"] mod ...;`) in the appropriate parent module, or delete the file",
            )
            .note("crate `demo`'s module tree was built from `src/lib.rs` or `src/main.rs`")
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
        fn freshness_stale() {
            insta::assert_snapshot!(render(&scenario("freshness_stale")), @r"
            warning: `crates/api/CLAUDE.md` is older than source files it depends on
             --> crates/api/CLAUDE.md:1:1
              |
              = help: files matching `**/*.rs` in the subtree are newer
              = help: run `workspace-lint done` once the tracked file is up to date
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(freshness)
              |
              = note: `#[warn(workspace_lint::freshness)]` on by default
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
        fn unused_pub_removal_candidate() {
            insta::assert_snapshot!(render(&scenario("unused_pub_removal_candidate")), @r"
            warning: pub fn `helper` in crate `mycrate` appears unused — consider removing
             --> crates/mycrate/src/lib.rs:42:1
              |
              = help: remove the item or its `pub` visibility
              = note: code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives
            help: if intentional, silence with:
              |
            42 + workspace_lint::expect!(unused_pub);
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
        fn stale_git_index() {
            insta::assert_snapshot!(render(&scenario("stale_git_index")), @r"
            warning: deleted file `crates/old/src/legacy.rs` is still tracked by git
              = help: run `git rm crates/old/src/legacy.rs` to stage the removal
            help: if intentional, silence with:
              |
            1 + # workspace-lint: expect(stale-git-index)
              |
              = note: `#[warn(workspace_lint::stale_git_index)]` on by default
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
        fn module_tree_broken_mod_decl() {
            insta::assert_snapshot!(render(&scenario("module_tree_broken_mod_decl")), @r#"
            warning: `mod missing` declared but no `missing.rs` or `missing/mod.rs` found
             --> crates/demo/src/lib.rs:3:1
              |
              = help: create `missing.rs` adjacent to this file, or `missing/mod.rs`, or add a `#[path = "…"]` attribute
              = note: `mod foo;` with no inline body must resolve to a source file
            help: if intentional, silence with:
              |
            3 + workspace_lint::expect!(module_tree);
              |
              = note: `#[warn(workspace_lint::module_tree)]` on by default
            "#);
        }

        #[test]
        fn module_tree_orphan_file() {
            insta::assert_snapshot!(render(&scenario("module_tree_orphan_file")), @r#"
            warning: orphan source file `src/orphan.rs` is not reachable from any `mod` declaration
             --> crates/demo/src/orphan.rs:1:1
              |
              = help: add `mod orphan;` (or a `#[path = "src/orphan.rs"] mod ...;`) in the appropriate parent module, or delete the file
              = note: crate `demo`'s module tree was built from `src/lib.rs` or `src/main.rs`
            help: if intentional, silence with:
              |
            1 + workspace_lint::expect!(module_tree);
              |
              = note: `#[warn(workspace_lint::module_tree)]` on by default
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
        fn module_tree_broken_mod_decl() {
            insta::assert_snapshot!(render(&scenario("module_tree_broken_mod_decl")), @r#"{"level":"warning","message":"`mod missing` declared but no `missing.rs` or `missing/mod.rs` found","code":{"code":"workspace-lint::module-tree","explanation":null},"spans":[{"file_name":"crates/demo/src/lib.rs","byte_start":0,"byte_end":0,"line_start":3,"line_end":3,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/demo/src/lib.rs","byte_start":0,"byte_end":0,"line_start":3,"line_end":3,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(module_tree);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"create `missing.rs` adjacent to this file, or `missing/mod.rs`, or add a `#[path = \"…\"]` attribute","spans":[]},{"level":"note","message":"`mod foo;` with no inline body must resolve to a source file","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn module_tree_orphan_file() {
            insta::assert_snapshot!(render(&scenario("module_tree_orphan_file")), @r#"{"level":"warning","message":"orphan source file `src/orphan.rs` is not reachable from any `mod` declaration","code":{"code":"workspace-lint::module-tree","explanation":null},"spans":[{"file_name":"crates/demo/src/orphan.rs","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/demo/src/orphan.rs","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(module_tree);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"add `mod orphan;` (or a `#[path = \"src/orphan.rs\"] mod ...;`) in the appropriate parent module, or delete the file","spans":[]},{"level":"note","message":"crate `demo`'s module tree was built from `src/lib.rs` or `src/main.rs`","spans":[]}],"rendered":null}"#);
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
        fn freshness_stale() {
            insta::assert_snapshot!(render(&scenario("freshness_stale")), @r##"{"level":"warning","message":"`crates/api/CLAUDE.md` is older than source files it depends on","code":{"code":"workspace-lint::freshness","explanation":null},"spans":[{"file_name":"crates/api/CLAUDE.md","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/api/CLAUDE.md","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(freshness)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"files matching `**/*.rs` in the subtree are newer","spans":[]},{"level":"help","message":"run `workspace-lint done` once the tracked file is up to date","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn cli_crate_version_rule_error() {
            insta::assert_snapshot!(render(&scenario("cli_crate_version_rule_error")), @r##"{"level":"warning","message":"pattern `v(\\d+)` did not match the output of `wasm-bindgen --version`","code":{"code":"workspace-lint::cli-crate-version","explanation":null},"spans":[],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(cli-crate-version)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"the regex must capture the version in group 1","spans":[]},{"level":"note","message":"ran `wasm-bindgen --version`","spans":[]}],"rendered":null}"##);
        }

        #[test]
        fn stale_git_index() {
            insta::assert_snapshot!(render(&scenario("stale_git_index")), @r##"{"level":"warning","message":"deleted file `crates/old/src/legacy.rs` is still tracked by git","code":{"code":"workspace-lint::stale-git-index","explanation":null},"spans":[],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(stale-git-index)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"run `git rm crates/old/src/legacy.rs` to stage the removal","spans":[]}],"rendered":null}"##);
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
        fn unused_pub_tighten_visibility() {
            insta::assert_snapshot!(render(&scenario("unused_pub_tighten_visibility")), @r#"{"level":"warning","message":"pub struct `Builder` in crate `mycrate` is only used inside the crate","code":{"code":"workspace-lint::unused-pub","explanation":null},"spans":[{"file_name":"crates/mycrate/src/builder.rs","byte_start":0,"byte_end":0,"line_start":7,"line_end":7,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/mycrate/src/builder.rs","byte_start":0,"byte_end":0,"line_start":7,"line_end":7,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"workspace_lint::expect!(unused_pub);\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"consider `pub(crate)` to tighten visibility","spans":[]},{"level":"note","message":"code compiled under configs outside `[engine] configs` and out-of-workspace consumers may cause false positives","spans":[]}],"rendered":null}"#);
        }

        #[test]
        fn unused_pub_publish_hint() {
            insta::assert_snapshot!(render(&scenario("unused_pub_publish_hint")), @r##"{"level":"warning","message":"crate `mycrate` has 3 public items unused within the workspace","code":{"code":"workspace-lint::unused-pub","explanation":null},"spans":[{"file_name":"crates/mycrate/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":null,"suggestion_applicability":null}],"children":[{"level":"help","message":"if intentional, silence with:","spans":[{"file_name":"crates/mycrate/Cargo.toml","byte_start":0,"byte_end":0,"line_start":1,"line_end":1,"column_start":1,"column_end":1,"is_primary":true,"label":null,"suggested_replacement":"# workspace-lint: expect(unused-pub)\n","suggestion_applicability":"MachineApplicable"}]},{"level":"help","message":"if `mycrate` is published outside this workspace, set `publish = true` in its Cargo.toml to treat its public API as external (these findings become exempt)","spans":[]},{"level":"note","message":"workspace-lint treats a crate as workspace-internal unless it declares `publish = true` (or a registry); see the unused-pub docs","spans":[]}],"rendered":null}"##);
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
        fn cli_crate_version_rule_error() {
            insta::assert_snapshot!(render(&scenario("cli_crate_version_rule_error")), @r"::warning file=Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Acli-crate-version::pattern `v(\d+)` did not match the output of `wasm-bindgen --version`");
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
        fn unused_pub_publish_hint() {
            insta::assert_snapshot!(render(&scenario("unused_pub_publish_hint")), @"::warning file=crates/mycrate/Cargo.toml,line=1,col=1,title=workspace-lint%3A%3Aunused-pub::crate `mycrate` has 3 public items unused within the workspace");
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
        fn architecture_denied_import() {
            insta::assert_snapshot!(render(&scenario("architecture_denied_import")), @"::warning file=crates/apps-foo/src/lib.rs,line=7,col=1,title=workspace-lint%3A%3Aarchitecture::import of `data_models::internal::User` from `apps-foo` violates architecture rule `no-internal-imports`");
        }

        #[test]
        fn architecture_denied_code_reference() {
            insta::assert_snapshot!(render(&scenario("architecture_denied_code_reference")), @"::warning file=crates/apps-foo/src/lib.rs,line=12,col=1,title=workspace-lint%3A%3Aarchitecture::reference to `data_models::internal::User` from `apps-foo` violates architecture rule `no-internal-imports`");
        }

        #[test]
        fn module_tree_broken_mod_decl() {
            insta::assert_snapshot!(render(&scenario("module_tree_broken_mod_decl")), @"::warning file=crates/demo/src/lib.rs,line=3,col=1,title=workspace-lint%3A%3Amodule-tree::`mod missing` declared but no `missing.rs` or `missing/mod.rs` found");
        }

        #[test]
        fn module_tree_orphan_file() {
            insta::assert_snapshot!(render(&scenario("module_tree_orphan_file")), @"::warning file=crates/demo/src/orphan.rs,line=1,col=1,title=workspace-lint%3A%3Amodule-tree::orphan source file `src/orphan.rs` is not reachable from any `mod` declaration");
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
                .level(crate::diagnostic::Level::Deny)
                .build();
            let mut buf = Vec::new();
            github::write_one(&d, &mut buf).unwrap();
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
            missing: vec!["demo+test.json".into()],
        };
        insta::assert_snapshot!(e.to_string(), @r#"IR incomplete under config `tests`: fragments still missing after a forced re-lint: ["demo+test.json"]"#);
    }
}
