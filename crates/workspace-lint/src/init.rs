//! The `init` subcommand: scaffold a default `.workspace-lint.toml`.
//!
//! A side-effecting command (like `expand`) — it emits no diagnostics,
//! so it carries no `LintId`. It writes the [`DEFAULT_CONFIG`] template to the
//! cwd, but *only* when the cwd is a genuine cargo **workspace root**: the root
//! `Cargo.toml` must declare a `[workspace]` table and the cwd must be that root
//! (not a member subdir). This keeps the file where config loading actually
//! looks for it (`config::load` reads `.workspace-lint.toml` from the cwd), and
//! keeps `workspace-lint`'s config genuinely workspace-scoped.
//!
//! Every guard fails loudly through [`wl_lint_api::util::fail`] (exit code 2, the
//! operational-error code) — a misdirected `init` is an operator mistake, not a
//! lint finding.

use std::path::Path;

use fs_err as fs;
use wl_engine::fast::FastModel;

use crate::config;
use wl_lint_api::util;

/// The commented starter written by `init`. Only the `[lints]` escalation and
/// the `[engine]` matrix are active; everything else is commented out, so the
/// emitted file audits clean: enabling a *policy* lint without its rules table
/// would itself draw a `config` finding, and the on-by-default structural lints
/// need no entry. The `init` test `default_config_is_self_clean` pins this
/// invariant.
const DEFAULT_CONFIG: &str = r#"# workspace-lint configuration.
# Docs: https://github.com/akesson/workspace-lint
#
# Structural lints (centralized-deps, orphan-file, feature-drift, unused-deps,
# unused-pub) are ON by default at `warn` with no config needed. This file only
# escalates/loosens levels and enables the policy lints (which need a rules
# table to do anything).

[lints]
# Escalate to a CI-failing error (exit code 1). Safe: every workspace crate
# should declare its dependencies with `workspace = true`.
centralized-deps = "deny"

# unused-deps / unused-pub are ON (the semantic tier): a plain `workspace-lint`
# run compiles the workspace on a pinned nightly toolchain (it prints the exact
# install commands, and on a terminal offers to run them). Pre-commit hooks
# should use `workspace-lint --fast-only`, which skips this tier.
# Uncomment to turn either off:
# unused-deps = "allow"
# unused-pub  = "allow"

# unused-pub deletion: `workspace-lint --fix-auto-delete` REMOVES a truly-unused
# `pub` item (instead of only narrowing it to `pub(crate)`), cascading through
# the whole dead chain in one pass and trimming any `use` left dangling. Only
# git-clean files are touched — the deletion's backup is `git checkout`. A
# manual, CLI-only operation: there is no config equivalent, so CI `--fix`
# runs can never delete code.

# --- Policy lints: uncomment a rules table to enable. (Enabling a policy lint
# --- in [lints] without its rules table is itself a `config` finding.) ---

# Cap shipped code lines per file (test code is excluded from the count).
# [[file-size.rules]]
# glob = "crates/*/src/**/*.rs"
# max-code-lines = 500

# Cap shipped code lines per crate directory.
# [[crate-size.rules]]
# glob = "crates/*"
# max-code-lines = 5000

# Flag structurally identical code (Type-2 clones: variable names and literal
# values may differ) across the workspace. The bare table enables it with the
# built-in thresholds; see the README for the tuning fields.
# [duplicate-code]

# Pin an installed CLI tool's version to a workspace crate's version.
# [[cli-crate-version.rules]]
# command = ["wasm-bindgen", "--version"]
# pattern = "wasm-bindgen (\\S+)"
# crate = "wasm-bindgen"

# Enforce layering: which crates may import which paths.
# [[architecture.rules]]
# name = "domain stays pure"
# from = ["domain-*"]
# deny = ["*::infra::*"]

# --- Semantic engine matrix: the real cargo commands this project runs — the
# declared matrix IS the support matrix (one `cargo check`-fidelity compile per
# entry, verdicts unioned). "cargo test" also judges test-gated usage and
# dev-dependencies; it costs a second compile pass, so drop it to just
# "cargo build" if you want a single, faster run. Cross-compiled projects add
# their target commands, e.g. "cargo build --target wasm32-unknown-unknown -p <pkg>".
[engine]
configs = ["cargo build", "cargo test"]
"#;

/// Scaffold [`DEFAULT_CONFIG`] into the cwd. See the module docs for the guard
/// chain; `force` overwrites an existing standalone file (nothing else).
pub(crate) fn run(force: bool) {
    let cwd = std::env::current_dir()
        .unwrap_or_else(|e| util::fail(format!("failed to resolve the current directory: {e}")));

    // A loadable cargo manifest is the floor: `Err` means "no Cargo.toml here"
    // or a cargo-level error — either way, not a workspace root.
    let model = FastModel::load(Path::new(".")).unwrap_or_else(|_| {
        util::fail("error: `init` must run at a cargo workspace root — no loadable Cargo.toml here")
    });

    // Guard against running inside a member: cargo walks up to find the
    // enclosing workspace, so `load` from a member succeeds but reports the
    // *parent* as root. An equal path yields an empty relative path (the helper
    // reconciles the macOS `/private/var` and Windows-UNC path asymmetries).
    let rel = model.crate_relative_path(&cwd);
    if !rel.as_os_str().is_empty() {
        util::fail(format!(
            "error: `init` must run at the workspace root; you're inside `{}` (root is `{}`)",
            rel.display(),
            model.root().display()
        ));
    }

    // A lone `[package]` is cargo's *implicit* workspace, not a real one —
    // require an explicit `[workspace]` table.
    if !model.root_manifest().defines_workspace() {
        util::fail(
            "error: Cargo.toml has no [workspace] table — `init` requires a cargo workspace root",
        );
    }

    // Never scaffold a second, conflicting config source: loading both a
    // standalone file and Cargo-metadata config is a hard error.
    if config::metadata_config_exists() {
        util::fail(
            "error: config already present in Cargo.toml [workspace.metadata.workspace-lint]",
        );
    }
    if config::standalone_config_exists() && !force {
        util::fail(format!(
            "error: {} already exists (use --force to overwrite)",
            config::STANDALONE_FILE
        ));
    }

    fs::write(config::STANDALONE_FILE, DEFAULT_CONFIG).unwrap_or_else(|e| {
        util::fail(format!("failed to write {}: {e}", config::STANDALONE_FILE))
    });
    // Human text → stderr, matching the tool's convention (keeps stdout clean).
    eprintln!("created {}", config::STANDALONE_FILE);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The emitted template must parse *and* draw no config-audit findings —
    /// otherwise a fresh `init` would hand the user a self-flagging config. Guards
    /// against a future section rename/removal silently rotting the template.
    #[test]
    fn default_config_is_self_clean() {
        let findings = config::audit_str(DEFAULT_CONFIG);
        assert!(
            findings.is_empty(),
            "the init template must be self-clean, but the config audit flagged: {:#?}",
            findings.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }
}
