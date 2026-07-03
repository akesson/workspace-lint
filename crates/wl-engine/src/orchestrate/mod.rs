//! Phase-1 orchestration: drive the vendored extractor dylib over a target
//! workspace, once per declared configuration, and guarantee a complete set of
//! IR fragments per run.
//!
//! The mechanism is the spike-proven embed flow (`spike/embed`, SPIKE §12.10):
//! the stable binary calls `dylint::run(opts)` directly — dylint builds/loads
//! its driver for the pinned toolchain and spawns it per crate; cargo fans out
//! over workspace members. Two process-global side effects are inherent to
//! that flow and are documented on [`Engine::extract`]: the `WL_IR_OUT`
//! environment variable (the spawned driver inherits it) and the current
//! directory (dylint checks the CWD workspace).

mod guard;
mod source;
mod toolchain;

pub use source::ExtractorSource;

use std::path::PathBuf;

/// One cargo configuration to extract under (SPIKE §7: one run = one cfg; the
/// flags are load-bearing because cfg-stripping happens before the driver sees
/// `TyCtxt`). The `id` names the IR subdirectory; `cargo_args` are forwarded
/// verbatim to `cargo check`.
#[derive(Debug, Clone)]
pub struct CfgSelector {
    pub id: String,
    pub cargo_args: Vec<String>,
}

impl CfgSelector {
    /// The default configuration (plain `cargo check`).
    pub fn default_cfg() -> Self {
        Self {
            id: "default".into(),
            cargo_args: Vec::new(),
        }
    }

    /// The `--tests` configuration: unit-test harnesses + integration tests,
    /// keyed `<crate>[@bin]+test.json` by the extractor (`sess.opts.test`).
    pub fn tests() -> Self {
        Self {
            id: "tests".into(),
            cargo_args: vec!["--tests".into()],
        }
    }

    /// The `--benches` configuration: bench targets. A default-harness bench
    /// compiles in test mode (keyed `+test` like unit tests); a
    /// `harness = false` bench compiles as a plain bin.
    pub fn benches() -> Self {
        Self {
            id: "benches".into(),
            cargo_args: vec!["--benches".into()],
        }
    }
}

/// What to extract: the target workspace, the config matrix (first entry is
/// the primary config — it defines the candidate set downstream), an optional
/// package selection (empty ⇒ all members), and where the per-config IR
/// directories go.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub workspace_root: PathBuf,
    pub configs: Vec<CfgSelector>,
    pub packages: Vec<String>,
    /// Root for the per-config fragment dirs (`<ir_root>/<config id>/`).
    /// Keep this path stable across runs — the warm-cache economics (SPIKE
    /// §12.9) depend on cargo reusing dylint's target dir.
    pub ir_root: PathBuf,
}

/// One completed per-config extraction: the fragments of every selected crate
/// live in `ir_dir`, completeness-guarded.
#[derive(Debug, Clone)]
pub struct ConfigRun {
    pub id: String,
    pub cargo_args: Vec<String>,
    pub ir_dir: PathBuf,
}

/// The result of [`Engine::extract`]: one [`ConfigRun`] per requested config,
/// in matrix order (first = primary).
#[derive(Debug, Clone)]
pub struct ExtractionRuns {
    pub runs: Vec<ConfigRun>,
    /// The dylib that produced them (useful for diagnostics/logging).
    pub dylib: PathBuf,
}

/// Everything that can go wrong before or during extraction. The toolchain
/// variants render the full actionable remediation (the exact `rustup` /
/// `cargo install` commands) in their `Display` — callers print them verbatim.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(
        "the full tier extracts IR inside a rustc build and needs the pinned toolchain\n\
         \n\
             rustup toolchain install {pin} --profile minimal \\\n\
                 --component rustc-dev --component llvm-tools-preview\n\
         \n\
         hint: `--fast-only` runs the build-free lints without any toolchain"
    )]
    ToolchainMissing { pin: String },

    #[error(
        "the pinned toolchain {pin} is installed but misses the `{component}` component\n\
         \n\
             rustup component add {component} --toolchain {pin}"
    )]
    ComponentMissing { pin: String, component: String },

    #[error(
        "`dylint-link` is not on PATH — the extractor dylib links through it\n\
         \n\
             cargo install dylint-link --locked"
    )]
    DylintLinkMissing,

    #[error("rustup is not installed (or not on PATH) — install it from https://rustup.rs")]
    RustupMissing,

    #[error("materializing the vendored extractor into {dir}: {source}")]
    Materialize {
        dir: PathBuf,
        source: std::io::Error,
    },

    #[error("building the extractor dylib in {dir} failed (see cargo output above)")]
    ExtractorBuild { dir: PathBuf },

    #[error("no wl_extractor@<toolchain> dylib under {dir} after a successful build")]
    DylibNotFound { dir: PathBuf },

    #[error("extraction under config `{config}` failed: {source}")]
    Extraction {
        config: String,
        source: anyhow::Error,
    },

    #[error(
        "IR incomplete under config `{config}`: fragments still missing after a forced \
         re-lint: {missing:?}"
    )]
    Incomplete {
        config: String,
        missing: Vec<String>,
    },

    #[error("reading cargo metadata for {dir}: {source}")]
    Metadata {
        dir: PathBuf,
        source: Box<cargo_metadata::Error>,
    },

    #[error("{context}: {source}")]
    Io {
        context: String,
        source: std::io::Error,
    },
}

/// The Phase-1 engine. Construct with an [`ExtractorSource`] (vendored at user
/// sites, `Repo` in this repository's own tests) and call [`Engine::extract`].
pub struct Engine {
    source: ExtractorSource,
}

impl Engine {
    pub fn new(source: ExtractorSource) -> Self {
        Self { source }
    }

    /// The extractor's pinned toolchain (from `extractor/rust-toolchain.toml`,
    /// surfaced at compile time — the single source of truth).
    pub fn pinned_toolchain() -> &'static str {
        env!("WL_EXTRACTOR_TOOLCHAIN")
    }

    /// Run the full Phase-1 flow: preflight → materialize → build the dylib →
    /// one `dylint::run` per config (+ completeness guard).
    ///
    /// # Process-global effects
    ///
    /// Must be called from a **single-threaded** phase of the program: it sets
    /// the `WL_IR_OUT` environment variable (the spawned driver inherits it —
    /// there is no per-spawn env hook in `dylint::run`) and temporarily
    /// changes the current directory to the target workspace (restored on
    /// return, including the error paths, via an RAII guard).
    pub fn extract(&self, cfg: &EngineConfig) -> Result<ExtractionRuns, EngineError> {
        toolchain::preflight(Self::pinned_toolchain())?;
        let package_dir = self.source.materialize()?;
        let dylib = source::build_dylib(&package_dir)?;

        // The guard's target set comes from cargo metadata on the target
        // workspace — computed before the chdir (explicit manifest path).
        let targets = guard::TargetSet::discover(&cfg.workspace_root, &cfg.packages, cfg)?;

        let _cwd = CwdGuard::enter(&cfg.workspace_root)?;
        let mut runs = Vec::new();
        for selector in &cfg.configs {
            let ir_dir = cfg.ir_root.join(&selector.id);
            std::fs::create_dir_all(&ir_dir).map_err(|source| EngineError::Io {
                context: format!("creating IR dir {}", ir_dir.display()),
                source,
            })?;
            self.run_config(selector, &ir_dir, &dylib, &cfg.packages, targets.as_ref())?;
            runs.push(ConfigRun {
                id: selector.id.clone(),
                cargo_args: selector.cargo_args.clone(),
                ir_dir,
            });
        }
        Ok(ExtractionRuns { runs, dylib })
    }

    /// One `dylint::run` under one config, then the completeness guard: an
    /// expected fragment can be missing because `WL_IR_OUT` is not in cargo's
    /// fingerprint (the SPIKE §11 caching gotcha — a "fresh" crate's lint pass
    /// never runs). On a miss, bump the dylib mtime (invalidates exactly the
    /// workspace members' lint units) and re-run once.
    fn run_config(
        &self,
        selector: &CfgSelector,
        ir_dir: &std::path::Path,
        dylib: &std::path::Path,
        packages: &[String],
        targets: Option<&guard::TargetSet>,
    ) -> Result<(), EngineError> {
        // SAFETY: single-threaded by the documented contract of `extract`.
        unsafe { std::env::set_var("WL_IR_OUT", ir_dir) };
        // The spawned `cargo check`'s stderr — compile progress, the
        // extractor's per-fragment notes, and any real compile diagnostics —
        // goes to a log next to the fragments, NOT the user's terminal: a
        // successful run must stay byte-deterministic for callers that
        // snapshot stderr. On failure the log is replayed verbatim (the
        // compile errors ARE the diagnosis). dylint appends, so truncate
        // between runs.
        let log = ir_dir.with_extension("log");
        let opts = dylint_opts(dylib, packages, &selector.cargo_args, &log);
        let run = |what: &str| {
            let _ = std::fs::write(&log, b"");
            dylint::run(&opts).map_err(|source| {
                if let Ok(captured) = std::fs::read_to_string(&log) {
                    eprint!("{captured}");
                }
                EngineError::Extraction {
                    config: format!("{} ({what})", selector.id),
                    source,
                }
            })
        };
        run("initial")?;

        let Some(targets) = targets else {
            return Ok(()); // guard skipped: unmodeled target-selection flag
        };
        let expected = targets.expected_fragments(&selector.cargo_args);
        // A complete whole-workspace run must produce *exactly* `expected` —
        // anything else in the dir is a leftover from a renamed crate, a
        // removed target, or an older binary's fragment naming, and the loader
        // reads every `*.json`, so a stale fragment would silently assemble
        // dead code into every future run. Prune (only when unscoped: a
        // package-filtered run legitimately shares the dir with siblings'
        // fragments).
        if packages.is_empty() {
            prune_stale_fragments(ir_dir, &expected);
        }
        let missing = guard::missing_fragments(ir_dir, &expected);
        if missing.is_empty() {
            return Ok(());
        }
        eprintln!(
            "wl-engine: {} expected fragment(s) missing under `{}` (cargo freshness skipped \
             their lint pass): {missing:?} — forcing a re-lint",
            missing.len(),
            selector.id,
        );
        guard::force_relint(dylib).map_err(|source| EngineError::Io {
            context: format!("bumping dylib mtime {}", dylib.display()),
            source,
        })?;
        run("forced re-lint")?;
        let still = guard::missing_fragments(ir_dir, &expected);
        if !still.is_empty() {
            return Err(EngineError::Incomplete {
                config: selector.id.clone(),
                missing: still,
            });
        }
        Ok(())
    }
}

/// Delete `*.json` files in `ir_dir` that no complete run of the current
/// binary would produce. Best-effort: a file that won't delete is at worst the
/// same stale-fragment exposure that existed before pruning.
fn prune_stale_fragments(ir_dir: &std::path::Path, expected: &std::collections::BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(ir_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !expected.contains(&name) {
            eprintln!("wl-engine: pruning stale IR fragment {name}");
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The `dylint::run` options of the embed flow: load exactly our dylib by
/// path, workspace members only, config selector forwarded to `cargo check`,
/// child stderr piped to `log` (surfaced only on failure).
fn dylint_opts(
    dylib: &std::path::Path,
    packages: &[String],
    cargo_args: &[String],
    log: &std::path::Path,
) -> dylint::opts::Dylint {
    use dylint::opts::{Check, Dylint, LibrarySelection, Operation};
    Dylint {
        pipe_stderr: Some(log.to_string_lossy().into_owned()),
        pipe_stdout: None,
        // dylint's own orchestrator-process chatter ("Checking with toolchain
        // `nightly-…-<host triple>`", the pipe-stderr experimental warning)
        // prints on OUR stderr, not the piped child's — and the triple makes
        // it platform-dependent. Callers snapshot stderr; keep it silent.
        quiet: true,
        operation: Operation::Check(Check {
            lib_sel: LibrarySelection {
                lib_paths: vec![dylib.to_string_lossy().into_owned()],
                ..Default::default()
            },
            no_deps: true,
            packages: packages.to_vec(),
            // An unscoped run must select EVERY member as a primary package.
            // In a non-virtual workspace (a root package with `[workspace]
            // members`, e.g. thiserror) a plain `cargo check` selects only
            // the root: the member crates compile as mere dependencies, whose
            // lint units dylint does not dylib-fingerprint — so their
            // fragments are never regenerated once cargo is warm, and the
            // completeness guard hard-fails. (This repo's own dogfood never
            // saw it: a virtual workspace's members are all primary.)
            workspace: packages.is_empty(),
            args: cargo_args.to_vec(),
            ..Default::default()
        }),
    }
}

/// RAII current-directory guard: enters the target workspace, restores the
/// caller's directory on drop (error paths included).
struct CwdGuard {
    prev: PathBuf,
}

impl CwdGuard {
    fn enter(dir: &std::path::Path) -> Result<Self, EngineError> {
        let prev = std::env::current_dir().map_err(|source| EngineError::Io {
            context: "reading the current directory".into(),
            source,
        })?;
        std::env::set_current_dir(dir).map_err(|source| EngineError::Io {
            context: format!("entering the target workspace {}", dir.display()),
            source,
        })?;
        Ok(Self { prev })
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.prev);
    }
}
