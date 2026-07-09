//! Orphan source files — `src/**.rs` that no declared config compiles.
//!
//! rustc never opens a file no `mod` chain reaches, so `dead_code` cannot see
//! it and clippy emits nothing: a file left behind by a rename is invisible to
//! the whole toolchain. That gap is this lint's reason to exist.
//!
//! **Reachability is rustc's answer, not ours.** The judged substrate is
//! `SemanticModel::loaded_files` — the union, across the `[engine]` config
//! matrix, of every source file rustc actually opened. A syntactic walker
//! cannot answer this question: the module graph is only determined after macro
//! expansion, so `#[cfg_attr(unix, path = …)]`, an `include!` in expression
//! position, a `macro_rules!`-generated `mod`, and a build-script-computed
//! include path each defeat it. This lint used to guess, and told people to
//! delete files that `memmap2`, `socket2`, and `tempfile` all compile.
//!
//! One residual class survives: a file reachable only under a `cfg` no declared
//! config compiles. rustc never opens it either, so it is indistinguishable
//! from dead code *by compilation alone*. The fast tier's `declared_reach`
//! separates the two — if the crate's own source names the file, the finding is
//! a **coverage gap** in the `[engine]` matrix, never a delete suggestion.

mod classify;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use wl_engine::SemanticModel;
use wl_engine::fast::{CrateInfo, FastModel};

use classify::{Verdict, canonical, classify, code_name};
use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::at_file;
use wl_diagnostic::render::display_path;
use wl_lint_api::{LintContext, LintId, LintImpl, Requirements};

pub struct OrphanFile;

impl OrphanFile {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OrphanFile {
    fn default() -> Self {
        Self::new()
    }
}

impl LintImpl for OrphanFile {
    const ID: LintId = LintId::OrphanFile;
    const DOC: &'static str = include_str!("DOC.md");
    // Semantic: "did rustc open this file?" is a fact only the compiler holds.
    // The fast tier still supplies the candidate listing and `declared_reach`.
    const REQUIRES: Requirements = Requirements {
        needs_fast: true,
        needs_semantic: true,
    };

    fn run(&self, cx: &LintContext<'_>) -> Vec<Diagnostic> {
        let Some((fast, semantic)) = cx.semantic_models(Self::ID) else {
            return Vec::new();
        };
        check(fast, semantic)
    }
}

pub(crate) fn check(fast: &FastModel, semantic: &SemanticModel) -> Vec<Diagnostic> {
    let rustc_reached: HashSet<PathBuf> = semantic
        .loaded_files()
        .iter()
        .map(|f| canonical(Path::new(f)))
        .collect();
    // A crate that emitted no fragment was never compiled by any config — a
    // scoped `[engine] configs = ["cargo build -p foo"]` does exactly that.
    // Judging one would report *every* file it owns as an orphan.
    let compiled = semantic.fragment_crates();
    let configs: Vec<&str> = semantic.config_ids().collect();

    let mut diagnostics = Vec::new();
    for krate in fast.members() {
        if !compiled.contains(code_name(&krate.name).as_str()) {
            continue;
        }
        for file in krate.src_files() {
            let canon = canonical(file);
            match classify(&canon, &rustc_reached, krate.declared_reach()) {
                Verdict::Live => {}
                Verdict::Orphan => diagnostics.push(orphan(fast, krate, file)),
                Verdict::CoverageGap => diagnostics.push(coverage_gap(fast, krate, file, &configs)),
            }
        }
    }
    diagnostics
}

/// The crate-relative form (`src/stale.rs`) is the right shape for the message:
/// the crate root is the natural origin for a reader thinking about `mod`
/// declarations. The *anchor*, though, must be workspace-relative so a
/// directive in a sibling file can match it.
fn crate_relative(krate: &CrateInfo, file: &Path) -> String {
    file.strip_prefix(&krate.manifest_dir)
        .map(display_path)
        .unwrap_or_else(|_| display_path(file))
}

fn orphan(fast: &FastModel, krate: &CrateInfo, file: &Path) -> Diagnostic {
    let rel = crate_relative(krate, file);
    let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    at_file(
        LintId::OrphanFile.id(),
        format!("orphan source file `{rel}` is never compiled"),
        fast.crate_relative_path(file),
    )
    .help(format!(
        "delete the file, or reach it: add `mod {stem};` (or `#[path = \"{rel}\"] mod ...;`) \
         in the appropriate parent module"
    ))
    .note(format!(
        "no `[engine]` config compiled it, and nothing in crate `{}`'s source names it",
        krate.name
    ))
    .build()
}

/// Pinned to `warn` via `level_explicit`: this reports a shortfall of the
/// `[engine]` matrix, not a defect in the code, so `orphan-file = "deny"` must
/// not turn it into a build failure. Same reasoning as unused-deps' own
/// not-compiled note.
fn coverage_gap(fast: &FastModel, krate: &CrateInfo, file: &Path, configs: &[&str]) -> Diagnostic {
    let rel = crate_relative(krate, file);
    at_file(
        LintId::OrphanFile.id(),
        format!("no declared `[engine]` config compiles `{rel}`"),
        fast.crate_relative_path(file),
    )
    .level_explicit(wl_diagnostic::Level::Warn)
    .help(
        "add a config that compiles it — a `--target` for a platform-gated module, \
         or `\"cargo test\"` for a `#[cfg(test)]` one",
    )
    // Deliberately not "so it is not dead": being named proves only that we
    // must not accuse it. A file gated on an unsatisfiable `cfg` is named, and
    // dead, and indistinguishable from a platform module we simply never built.
    .note(format!(
        "crate `{}`'s source names this file, so it is not reported as an orphan — but \
         the declared config{} ({}) never opened it, so nothing in it is checked",
        krate.name,
        if configs.len() == 1 { "" } else { "s" },
        configs.join(", "),
    ))
    .build()
}
