//! The lint-run tier: model loading and lint execution.
//!
//! [`run_all`] (the default run) and [`run_single_check`] (`check <rule>`)
//! both produce a [`RunOutput`]: the diagnostic stream plus the shared
//! models the caller's suppression/leveling/fix tail consumes. The tier
//! owns the fast-first ordering contract — the build-free lints run and
//! their findings are held BEFORE the fallible rustc-backed extraction,
//! so an engine failure ([`EngineFailure`]) can never swallow findings
//! that never needed the engine.

use std::collections::HashSet;

use wl_diagnostic::Diagnostic;
use wl_engine::fast::FastModel;
use wl_lint_api::{LintContext, LintId, util};

use crate::cli::CheckRule;
use crate::{config, provision, registry};

/// One lint pass's outputs: the diagnostic stream plus the shared models the
/// caller's `--fix-auto-delete` cascade reuses, and the ran-set that scopes
/// `stale-expect`.
pub(crate) struct RunOutput {
    pub(crate) diagnostics: Vec<Diagnostic>,
    pub(crate) fast: Option<FastModel>,
    pub(crate) semantic: Option<wl_engine::SemanticModel>,
    pub(crate) cfg_shadow: Option<wl_engine::coverage::CfgShadow>,
    pub(crate) ran: HashSet<LintId>,
    /// The engine failure, when extraction/assembly failed: the fast-tier
    /// findings above still went through the normal pipeline and render
    /// BEFORE this error aborts the run (exit 2) — an engine failure must
    /// not swallow findings that never needed the engine. `ran` excludes
    /// the semantic lints in that case, so their `expect`s can't go stale.
    pub(crate) engine_error: Option<EngineFailure>,
}

/// An extraction/assembly failure carried out of [`run_all`] so the caller
/// can render the fast-tier findings first.
pub(crate) struct EngineFailure {
    pub(crate) message: String,
    /// `true` when the interactive provisioning prompt already printed the
    /// error verbatim — don't print it twice.
    pub(crate) shown: bool,
}

/// The default run's lint pipeline. The returned [`LintId`] set records which
/// lints actually ran (post-`--fast-only`, post-`allow`) — the staleness
/// domain for `expect` directives.
pub(crate) fn run_all(config: &config::Config, fast_only: bool, auto_delete: bool) -> RunOutput {
    let mut registry = registry::registry(config);
    // `--fast-only` runs only the build-free lints: a semantic lint is
    // *skipped* — not invoked without its model (its `check` rightly demands
    // one) and not silently degraded to a weaker analysis.
    if fast_only {
        registry.retain(|l| !l.requirements().needs_semantic);
    }
    // The build-free FastModel is needed when some enabled lint asks for it,
    // or when a per-crate `[crates.*]` tier is present — the latter so
    // per-crate levels can map diagnostics to their owning crate and crate
    // names can be validated against the membership, even if no lint itself
    // needs the metadata.
    let needs_fast =
        registry.iter().any(|l| l.requirements().needs_fast) || !config.crates.is_empty();
    // The rustc-backed tier runs only when some (still-enabled) lint asks
    // for it — never under `--fast-only` (the retain above emptied those).
    let needs_semantic = registry.iter().any(|l| l.requirements().needs_semantic);
    let fast = needs_fast.then(|| wl_engine::timing::phase("FastModel::load", load_fast_model));
    // A memberless workspace (a bare virtual manifest) has nothing to extract
    // or judge — cargo would just error "the workspace has no members" — so
    // the tier is skipped; semantic lints see zero members and emit nothing.
    // `needs_semantic ⇒ needs_fast` (every semantic lint declares both), so
    // `fast` is loaded whenever this check runs.
    let has_members = fast.as_ref().is_some_and(|f| !f.members().is_empty());
    // Fast tier FIRST, semantic tier second: extraction is the step that can
    // fail (toolchain missing, a member that doesn't compile), and its
    // failure must not swallow the findings the build-free lints already
    // have. The fast diagnostics are held; on an engine error they travel
    // out via `engine_error` and render before the error aborts the run.
    let fast_cx = LintContext {
        fast: fast.as_ref(),
        semantic: None,
        cfg_shadow: None,
        auto_delete,
    };
    let mut diagnostics: Vec<Diagnostic> = wl_engine::timing::phase("LINTS (fast)", || {
        registry
            .iter()
            .filter(|l| !l.requirements().needs_semantic)
            .flat_map(|l| wl_engine::timing::phase(l.id().short(), || l.check(&fast_cx)))
            .collect()
    });
    let mut ran: HashSet<LintId> = registry
        .iter()
        .filter(|l| !l.requirements().needs_semantic)
        .map(|l| l.id())
        .collect();

    let semantic = if needs_semantic && has_members {
        match wl_engine::timing::phase("SEMANTIC (extract+assemble)", || {
            load_semantic_model(config.engine.selectors())
        }) {
            Ok(model) => Some(model),
            Err(failure) => {
                return RunOutput {
                    diagnostics,
                    fast,
                    semantic: None,
                    cfg_shadow: None,
                    ran,
                    engine_error: Some(failure),
                };
            }
        }
    } else {
        None
    };
    // The cfg-shadow index rides along whenever the semantic tier ran: it is
    // what lets `unused-pub` (and the `--fix-auto-delete` cascade, which
    // reuses it) say "possibly used under `cfg(...)` no config compiles"
    // instead of a generic blind-spot disclaimer.
    let shadow = semantic.as_ref().and(fast.as_ref()).map(|fm| {
        wl_engine::timing::phase("cfg_shadow[scan+eval]", || {
            wl_engine::coverage::CfgShadow::compute(
                fm,
                &config.engine.selectors(),
                wl_engine::coverage::host_triple().as_deref(),
            )
        })
    });
    let cx = LintContext {
        fast: fast.as_ref(),
        semantic: semantic.as_ref(),
        cfg_shadow: shadow.as_ref(),
        auto_delete,
    };
    diagnostics.extend(wl_engine::timing::phase("LINTS (semantic)", || {
        registry
            .iter()
            .filter(|l| l.requirements().needs_semantic)
            .flat_map(|l| wl_engine::timing::phase(l.id().short(), || l.check(&cx)))
            .collect::<Vec<_>>()
    }));
    ran.extend(
        registry
            .iter()
            .filter(|l| l.requirements().needs_semantic)
            .map(|l| l.id()),
    );
    // `cx`'s borrows end here (last use above), so the models move out for
    // the `--fix` cascade the caller runs.
    RunOutput {
        diagnostics,
        fast,
        semantic,
        cfg_shadow: shadow,
        ran,
        engine_error: None,
    }
}

pub(crate) fn run_single_check(
    rule: CheckRule,
    fast_only: bool,
    auto_delete: bool,
    config: Option<&config::Config>,
) -> RunOutput {
    let lint = rule.into_lint();
    let requirements = lint.requirements();
    // A semantic lint cannot run without its model; under `--fast-only` that
    // is a contradiction the user should hear about, not a silent no-op.
    if requirements.needs_semantic && fast_only {
        util::fail(format!(
            "error: `{}` needs the rustc-backed semantic tier, which `--fast-only` skips — drop the flag to run it",
            lint.id().short()
        ));
    }
    let fast = requirements.needs_fast.then(load_fast_model);
    // Outside a configured workspace (`config::try_load` → None) the engine
    // falls back to its default single-config matrix. A memberless workspace
    // skips the tier entirely (see run_all).
    let has_members = fast.as_ref().is_some_and(|f| !f.members().is_empty());
    let semantic = (requirements.needs_semantic && has_members).then(|| {
        let engine = config.map(|c| c.engine.clone()).unwrap_or_default();
        load_semantic_model(engine.selectors()).unwrap_or_else(|f| {
            if !f.shown {
                eprintln!("{}", f.message);
            }
            std::process::exit(2);
        })
    });
    let shadow = semantic.as_ref().and(fast.as_ref()).map(|fm| {
        let engine = config.map(|c| c.engine.clone()).unwrap_or_default();
        wl_engine::coverage::CfgShadow::compute(
            fm,
            &engine.selectors(),
            wl_engine::coverage::host_triple().as_deref(),
        )
    });
    let cx = LintContext {
        fast: fast.as_ref(),
        semantic: semantic.as_ref(),
        cfg_shadow: shadow.as_ref(),
        auto_delete,
    };
    let ran = HashSet::from([lint.id()]);
    let diagnostics = lint.check(&cx);
    // `cx`'s borrows end above, so the models can move out for the
    // `--fix-auto-delete` cascade the caller may run.
    RunOutput {
        diagnostics,
        fast,
        semantic,
        cfg_shadow: shadow,
        ran,
        engine_error: None,
    }
}

/// Build the full (rustc-backed) tier: vendored extractor → one embedded
/// dylint extraction per config → Phase-2 assembly. Returns `Err` with the
/// error `Display` verbatim — the `EngineError` toolchain variants carry the
/// full rustup remediation text, which must reach the user unwrapped and
/// untruncated — so the caller can render the fast-tier findings BEFORE the
/// failure aborts the run. On an interactive terminal, provisionable
/// preflight failures (missing pinned toolchain / component / dylint-link)
/// are first offered as a one-keypress install-and-retry
/// ([`provision::Provisioner`]); each stage repairs at most once, so the
/// loop is bounded by the number of preflight checks.
pub(crate) fn load_semantic_model(
    configs: Vec<wl_engine::ConfigSpec>,
) -> Result<wl_engine::SemanticModel, EngineFailure> {
    let root = std::path::absolute(".")
        .unwrap_or_else(|e| util::fail(format!("failed to resolve the workspace root: {e}")));
    let engine = wl_engine::Engine::new(wl_engine::ExtractorSource::vendored());
    let engine_config = wl_engine::EngineConfig {
        workspace_root: root.clone(),
        configs,

        // Relative on purpose: `Engine::extract` enters the workspace
        // root, so the per-config IR dirs land under the target dir of
        // the linted workspace (stable across runs — warm-cache friendly).
        ir_root: std::path::PathBuf::from("target/workspace-lint/ir"),
    };
    let mut provisioner = provision::Provisioner::new();
    let runs = wl_engine::timing::phase("EXTRACT (phase 1)", || {
        loop {
            match engine.extract(&engine_config) {
                Ok(runs) => break Ok(runs),
                Err(e) => match provisioner.repair(&e) {
                    provision::Repair::Retry => continue,
                    provision::Repair::GiveUp { error_shown } => {
                        break Err(EngineFailure {
                            message: e.to_string(),
                            shown: error_shown,
                        });
                    }
                },
            }
        }
    })?;
    wl_engine::timing::phase("ASSEMBLE (phase 2)", || {
        wl_engine::SemanticModel::load(&runs).map_err(|e| EngineFailure {
            message: e.to_string(),
            shown: false,
        })
    })
}

/// Load the build-free `FastModel` (`cargo metadata` + parsed manifests +
/// the lean syntactic module trees). Loud-fail on error: a silent `None`
/// would mask a broken state in CI.
pub(crate) fn load_fast_model() -> FastModel {
    FastModel::load(std::path::Path::new(".")).unwrap_or_else(|e| {
        util::fail(format!(
            "failed to load workspace metadata for manifest-backed lints: {e}"
        ))
    })
}

/// Best-effort [`FastModel`] load used when no enabled lint required one: the
/// generated-file drop and the suppression scanner's parse cache still want it.
/// Non-fatal: a directory that isn't a loadable cargo workspace yields `None`
/// (the drop is skipped; the directive scan parses on demand). Lints that
/// DECLARE `needs_fast` — since the measurement-sweep port that includes
/// `check file-size` — instead go through the loud-fail load: they are
/// workspace-rooted by design and exit 2 outside one.
pub(crate) fn try_load_fast_model() -> Option<FastModel> {
    FastModel::load(std::path::Path::new(".")).ok()
}
