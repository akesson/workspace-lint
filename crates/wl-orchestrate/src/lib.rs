//! Phase-1 orchestration: drive the vendored extractor dylib over a target
//! workspace, once per declared configuration, and guarantee a complete set of
//! IR fragments per run.
//!
//! This is the first phase of workspace-lint's rustc-fidelity engine; the
//! Phase-2 assembler (`wl-engine::semantic`) consumes the [`ExtractionRuns`]
//! this produces. `wl-engine` re-exports this crate as `wl_engine::orchestrate`
//! (and [`coverage`] as `wl_engine::coverage`), so consumers see one engine
//! surface — nothing outside this crate names `wl_orchestrate` directly.
//!
//! The mechanism is the spike-proven embed flow (`spike/embed`, SPIKE §12.10):
//! the stable binary calls `dylint::run(opts)` directly — dylint builds/loads
//! its driver for the pinned toolchain and spawns it per crate; cargo fans out
//! over workspace members. Two process-global side effects are inherent to
//! that flow and are documented on [`Engine::extract`]: the `WL_IR_OUT`
//! environment variable (the spawned driver inherits it) and the current
//! directory (dylint checks the CWD workspace).

pub mod coverage;

mod closure;
mod command;
mod guard;
mod hash;
mod relink;
mod source;
mod toolchain;

pub use command::{CommandError, ConfigSpec, FeatureSel, Kinds, parse_command};
pub use source::ExtractorSource;

use std::path::PathBuf;

/// What to extract: the target workspace, the config matrix (first entry is
/// the primary config — it defines the candidate set downstream; package
/// selection is per-config, on each [`ConfigSpec`]), and where the per-config
/// IR directories go.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub workspace_root: PathBuf,
    pub configs: Vec<ConfigSpec>,
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

/// One config's extraction plan: the member closure cargo compiles in its
/// universe (empty ⇒ whole workspace) and its own completeness target set
/// (`None` ⇒ guard skipped for this config only).
struct ConfigPlan {
    packages: Vec<String>,
    targets: Option<guard::TargetSet>,
}

/// The build-fragment sharing key: configs with the same `(target, features)`
/// compile their build units identically (one compile per universe), so
/// enforcement and dedup operate within a group and never across.
fn universe_key(spec: &ConfigSpec) -> String {
    format!(
        "{}|{}|{}|{}",
        spec.target.as_deref().unwrap_or("host"),
        spec.features.features.join(","),
        spec.features.all_features,
        spec.features.no_default_features,
    )
}

/// The result of [`Engine::extract`]: one [`ConfigRun`] per requested config,
/// in matrix order (first = primary).
#[derive(Debug, Clone)]
pub struct ExtractionRuns {
    pub runs: Vec<ConfigRun>,
    /// The dylib that produced them (useful for diagnostics/logging).
    pub dylib: PathBuf,
    /// Resolved workspace metadata (`cargo metadata` with `resolve`), read once
    /// during extraction and reused by the semantic assembler — the completeness
    /// guard and `WorkspaceMeta` share this single exec.
    pub metadata: cargo_metadata::Metadata,
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

    #[error(
        "the config matrix compiles for `{triple}`, but the pinned toolchain lacks that \
         target's std\n\
         \n\
             rustup target add {triple} --toolchain {pin}"
    )]
    TargetMissing { pin: String, triple: String },

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
         re-lint: {missing:?}\n\
         \n\
         hint: if this persists, delete `target/dylint` in the analyzed workspace to \
         reset the engine's build cache"
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

    #[error(
        "config `{config}` selects package `{package}`, which is not a workspace member\n\
         \n\
         hint: `-p` in an [engine] config names workspace members (the engine expands \
         each to the members it compiles with)"
    )]
    UnknownPackage { package: String, config: String },

    #[error("{context}: {source}")]
    Io {
        context: String,
        source: std::io::Error,
    },
}

impl EngineError {
    /// The command that repairs this failure, as an argv — the same command
    /// the `Display` text tells the user to paste (a lockstep test in
    /// `toolchain.rs` keeps the two from drifting). `None` for failures with
    /// no safe auto-remediation: installing rustup itself is a shell pipe
    /// from the network, and everything past preflight (compile failures,
    /// IO) is not a provisioning problem.
    pub fn remediation(&self) -> Option<Vec<String>> {
        let argv: Vec<&str> = match self {
            Self::ToolchainMissing { pin } => vec![
                "rustup",
                "toolchain",
                "install",
                pin,
                "--profile",
                "minimal",
                "--component",
                "rustc-dev",
                "--component",
                "llvm-tools-preview",
            ],
            Self::ComponentMissing { pin, component } => {
                vec!["rustup", "component", "add", component, "--toolchain", pin]
            }
            Self::TargetMissing { pin, triple } => {
                vec!["rustup", "target", "add", triple, "--toolchain", pin]
            }
            Self::DylintLinkMissing => vec!["cargo", "install", "dylint-link", "--locked"],
            _ => return None,
        };
        Some(argv.into_iter().map(String::from).collect())
    }
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
    pub(crate) fn pinned_toolchain() -> &'static str {
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
        let triples: std::collections::BTreeSet<String> = cfg
            .configs
            .iter()
            .filter_map(|s| s.target.clone())
            .collect();
        wl_fast::timing::phase("preflight[rustup]", || {
            toolchain::preflight(Self::pinned_toolchain(), &triples)
        })?;
        let (package_dir, sources_fresh) =
            wl_fast::timing::phase("materialize[vendored src]", || self.source.materialize())?;
        let dylib = relink::RelinkedDylib::new(wl_fast::timing::phase(
            "build_dylib[cargo build nightly]",
            || {
                // Warm-run fast path: when `materialize` rewrote nothing, the
                // vendored sources on disk are byte-identical to this binary's
                // embedded copy, so a dylib already built from them is current —
                // reuse it instead of re-spawning `cargo build` just to
                // reconfirm. `existing_dylib` is a pure directory read (no cargo
                // spawn, no mtime touch — the mtime is the relink generation
                // key). If the sources changed (a dev extractor edit) or no
                // dylib is present yet, fall through and build. `Repo` sources
                // are live, so `materialize` reports them never-fresh.
                if sources_fresh && let Some(dylib) = source::existing_dylib(&package_dir) {
                    Ok(dylib)
                } else {
                    source::build_dylib(&package_dir)
                }
            },
        )?);

        // One `cargo metadata` (with `resolve`) serves the completeness guard,
        // the closure expansion of host-universe configs, and the semantic
        // assembler (`WorkspaceMeta`, via the returned `ExtractionRuns`): the
        // resolve graph is a superset of the guard's member/target facts, so a
        // single exec — computed before the chdir, with an explicit manifest
        // path — covers all three. Each distinct `--target` triple gets one
        // extra `--filter-platform` exec (a different universe resolves a
        // different dep graph).
        let metadata = wl_fast::timing::phase("cargo_metadata[+resolve]", || {
            cargo_metadata::MetadataCommand::new()
                .manifest_path(cfg.workspace_root.join("Cargo.toml"))
                .exec()
                .map_err(|source| EngineError::Metadata {
                    dir: cfg.workspace_root.clone(),
                    source: Box::new(source),
                })
        })?;
        let mut universe_mds: std::collections::BTreeMap<String, cargo_metadata::Metadata> =
            std::collections::BTreeMap::new();
        for triple in &triples {
            let md = wl_fast::timing::phase(
                format_args!("cargo_metadata[--filter-platform {triple}]"),
                || closure::universe_metadata(&cfg.workspace_root, triple),
            )?;
            universe_mds.insert(triple.clone(), md);
        }

        // Per-config plan: the package closure cargo compiles in that config's
        // universe (empty = whole workspace) and the config's own target set.
        let mut plans: Vec<ConfigPlan> = Vec::new();
        for spec in &cfg.configs {
            let umd = spec
                .target
                .as_ref()
                .map(|t| &universe_mds[t])
                .unwrap_or(&metadata);
            let packages = if spec.packages.is_empty() {
                Vec::new()
            } else {
                closure::member_closure(umd, spec)?
            };
            // `Benches` stays guard-unmodeled (a bench fragment's `+test`
            // suffix depends on the harness flag, which cargo_metadata 0.23
            // no longer exposes) — but the skip is per-config now: one bench
            // entry no longer disables the guard for the whole matrix.
            let targets = if spec.kinds == Kinds::Benches {
                eprintln!(
                    "workspace-lint: completeness guard skipped — bench harness kinds aren't \
                     modeled (config `{}`)",
                    spec.id
                );
                None
            } else {
                Some(guard::TargetSet::discover(umd, &packages))
            };
            plans.push(ConfigPlan { packages, targets });
        }
        // The keep-vocabulary for the scoped pruner: every crate/target name
        // the extractor could write a fragment for, across all members — NOT
        // just package names. An integration test or extra bin compiles under
        // a crate name that is not its package's (`tests/foo.rs` → `foo`);
        // keying the prune on package names alone deleted those valid
        // fragments every run, which the completeness guard then re-demanded
        // — a permanent scoped re-lint loop. Derived from the unfiltered host
        // metadata: target lists come from manifests and are platform-
        // independent, so one vocabulary covers every `--target` config dir.
        let valid_crates = guard::crate_name_universe(&metadata);

        let _cwd = CwdGuard::enter(&cfg.workspace_root)?;
        let mut runs = Vec::new();
        for (spec, plan) in cfg.configs.iter().zip(&plans) {
            let ir_dir = cfg.ir_root.join(&spec.id);
            std::fs::create_dir_all(&ir_dir).map_err(|source| EngineError::Io {
                context: format!("creating IR dir {}", ir_dir.display()),
                source,
            })?;
            wl_fast::timing::phase(format_args!("run_config[{}]", spec.id), || {
                self.run_config(spec, &ir_dir, &dylib, plan, &valid_crates)
            })?;
            runs.push(ConfigRun {
                id: spec.id.clone(),
                cargo_args: spec.cargo_args(),
                ir_dir,
            });
        }
        // Build-script fragments get their own completeness pass, scoped to a
        // **universe group** (configs sharing `(target, features)`): a build
        // unit compiles once per universe — identical flags across that
        // group's configs — so its fragment lands only in whichever group
        // member's run first compiled it. Across *different* universes the
        // copies are semantically distinct (a `--features` flag re-runs the
        // build script), so neither enforcement nor dedup may cross groups.
        let mut groups: std::collections::BTreeMap<String, Vec<usize>> =
            std::collections::BTreeMap::new();
        for (i, spec) in cfg.configs.iter().enumerate() {
            groups.entry(universe_key(spec)).or_default().push(i);
        }
        for idxs in groups.values() {
            self.ensure_build_fragments(cfg, &plans, &runs, idxs, &dylib, &valid_crates)?;
        }
        Ok(ExtractionRuns {
            runs,
            dylib: dylib.canonical().to_path_buf(),
            metadata,
        })
    }

    /// Enforce build-fragment presence across one universe group's config
    /// dirs: missing everywhere → one forced re-lint of each group config
    /// whose expected build set covers a missing fragment (the generation
    /// bump refreshes build units exactly like lib units — verified in the
    /// step-0 spike), then a hard error. Mirrors `run_config`'s per-config
    /// guard. Also dedups the group's surviving copies (newest wins).
    fn ensure_build_fragments(
        &self,
        cfg: &EngineConfig,
        plans: &[ConfigPlan],
        runs: &[ConfigRun],
        group: &[usize],
        dylib: &relink::RelinkedDylib,
        valid_crates: &std::collections::BTreeSet<String>,
    ) -> Result<(), EngineError> {
        let names: std::collections::BTreeSet<String> = group
            .iter()
            .filter_map(|&i| plans[i].targets.as_ref())
            .flat_map(|t| t.build_fragments())
            .collect();
        if names.is_empty() {
            return Ok(());
        }
        let group_runs: Vec<ConfigRun> = group.iter().map(|&i| runs[i].clone()).collect();
        let dirs: Vec<std::path::PathBuf> = group_runs.iter().map(|r| r.ir_dir.clone()).collect();
        let missing = guard::missing_build_fragments(&dirs, &names);
        if !missing.is_empty() {
            eprintln!(
                "workspace-lint: {} build-script fragment(s) missing from every config dir of the \
                 universe (cargo freshness skipped their lint pass): {missing:?} — forcing a \
                 re-lint",
                missing.len(),
            );
            dylib.bump().map_err(|source| EngineError::Io {
                context: format!("bumping dylib mtime {}", dylib.canonical().display()),
                source,
            })?;
            for &i in group {
                let covers = plans[i]
                    .targets
                    .as_ref()
                    .is_some_and(|t| t.build_fragments().iter().any(|n| missing.contains(n)));
                if covers {
                    self.run_config(
                        &cfg.configs[i],
                        &runs[i].ir_dir,
                        dylib,
                        &plans[i],
                        valid_crates,
                    )?;
                }
            }
            let still = guard::missing_build_fragments(&dirs, &names);
            if !still.is_empty() {
                return Err(EngineError::Incomplete {
                    config: cfg.configs[group[0]].id.clone(),
                    missing: still,
                });
            }
        }
        dedup_build_fragments(&group_runs, &names);
        Ok(())
    }

    /// One `dylint::run` under one config, then the completeness guard: an
    /// expected fragment can be missing because `WL_IR_OUT` is not in cargo's
    /// fingerprint (the SPIKE §11 caching gotcha — a "fresh" crate's lint pass
    /// never runs). On a miss, bump the dylib generation (invalidates exactly
    /// the workspace members' lint units — see the `relink` module) and
    /// re-run once.
    fn run_config(
        &self,
        selector: &ConfigSpec,
        ir_dir: &std::path::Path,
        dylib: &relink::RelinkedDylib,
        plan: &ConfigPlan,
        valid_crates: &std::collections::BTreeSet<String>,
    ) -> Result<(), EngineError> {
        let packages = &plan.packages;
        let targets = plan.targets.as_ref();
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
        let run = |what: &str| {
            let _ = std::fs::write(&log, b"");
            // Derive the dylib path per invocation: after a bump, the re-run
            // must hand dylint the NEW generation path or the `DYLINT_LIBS`
            // env-dep channel never fires.
            let opts = dylint_opts(&dylib.current(), packages, &selector.cargo_args(), &log);
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
        let expected = targets.expected_fragments(selector.kinds);
        // A complete whole-workspace run must produce *exactly* `expected` —
        // anything else in the dir is a leftover from a renamed crate, a
        // removed target, or an older binary's fragment naming, and the loader
        // reads every `*.wlir`, so a stale fragment would silently assemble
        // dead code into every future run.
        if packages.is_empty() {
            // Build fragments live in whichever config dir first compiled
            // them (see `ensure_build_fragments`) — every config's keep-set
            // must include their names or the prune would sweep the one copy.
            let mut keep = expected.clone();
            keep.extend(targets.build_fragments());
            prune_stale_fragments(ir_dir, &keep, valid_crates);
        } else {
            // A package-scoped dir's exact population is bet on the closure's
            // precision, so prune only what is provably stale: fragments whose
            // crate part is produced by no current member target at all
            // (renames/removals — the one class that otherwise over-credits
            // forever). `valid_crates` is the crate/target-name vocabulary, not
            // package names: an integration test or extra bin has a fragment
            // stem that is not its package's name, and keying the prune on
            // package names deleted those every run (a scoped re-lint loop).
            prune_nonmember_fragments(ir_dir, valid_crates);
        }
        let missing = guard::missing_fragments(ir_dir, &expected);
        if missing.is_empty() {
            return Ok(());
        }
        eprintln!(
            "workspace-lint: {} expected fragment(s) missing under `{}` (cargo freshness skipped \
             their lint pass): {missing:?} — forcing a re-lint",
            missing.len(),
            selector.id,
        );
        dylib.bump().map_err(|source| EngineError::Io {
            context: format!("bumping dylib mtime {}", dylib.canonical().display()),
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

/// Delete `*.wlir` files in `ir_dir` that no complete run of the current
/// binary would produce. Best-effort: a file that won't delete is at worst the
/// same stale-fragment exposure that existed before pruning.
///
/// Deletion and *reporting* are gated differently. Everything outside the
/// keep-set is deleted, but only a fragment whose crate part matches no
/// current workspace target earns a message — a rename/removal leftover or an
/// older binary's naming scheme, the once-ever cleanups a user might care to
/// see. A current member's off-set fragment is a routine byproduct: a dirty
/// member's plain lib unit re-checked under the `cargo test` config emits a
/// plain fragment that config's keep-set doesn't contain, on *every* edit —
/// announcing it would make stderr vary with cargo freshness (see the
/// byte-determinism note in `run_config`).
fn prune_stale_fragments(
    ir_dir: &std::path::Path,
    keep: &std::collections::BTreeSet<String>,
    valid_crates: &std::collections::BTreeSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(ir_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wlir") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !keep.contains(&name) {
            if !valid_crates.contains(fragment_crate_part(&name)) {
                eprintln!(
                    "workspace-lint: pruning stale IR fragment {name} (no such workspace target)"
                );
            }
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The crate part of a fragment file name: the stem up to the first `@`/`+`
/// marker — the compiled crate name, or the package name for a `<pkg>@build`
/// build fragment.
fn fragment_crate_part(name: &str) -> &str {
    let stem = name.trim_end_matches(".wlir");
    stem.split_once(['@', '+']).map(|(k, _)| k).unwrap_or(stem)
}

/// Delete `*.wlir` files whose crate part (see [`fragment_crate_part`])
/// matches no current workspace target. The conservative prune for
/// package-scoped dirs: a renamed/removed crate's fragment can't over-credit
/// forever, while `valid_crates` — every member's target *and* package names
/// (see [`guard::crate_name_universe`]) — is a superset of what any closure
/// compiles, so closure imprecision can never delete a valid fragment.
///
/// Keying this on package names alone (the original bug) deleted integration-
/// test and extra-bin fragments — `tests/foo.rs` compiles under crate `foo`,
/// not its package name — on every warm run; the completeness guard, which
/// derives the same name from the target list, then re-demanded them and
/// forced a re-lint, so a package-scoped config never reached the cheap
/// cargo-fresh state.
fn prune_nonmember_fragments(
    ir_dir: &std::path::Path,
    valid_crates: &std::collections::BTreeSet<String>,
) {
    let Ok(entries) = std::fs::read_dir(ir_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wlir") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !valid_crates.contains(fragment_crate_part(&name)) {
            eprintln!(
                "workspace-lint: pruning stale IR fragment {name} (no such workspace target)"
            );
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Keep exactly one copy of each build fragment across the run's config dirs.
/// A forced re-lint under a later config duplicates them (the generation bump
/// recompiles build units into that config's dir too); afterwards, a build.rs
/// edit refreshes only the copy in whichever config compiles first, leaving
/// the duplicate permanently stale — and the loader unions every dir, so the
/// stale copy would over-credit forever. Newest mtime wins; best-effort like
/// the pruner.
fn dedup_build_fragments(runs: &[ConfigRun], names: &std::collections::BTreeSet<String>) {
    for name in names {
        let mut copies: Vec<(std::path::PathBuf, std::time::SystemTime)> = runs
            .iter()
            .filter_map(|r| {
                let path = r.ir_dir.join(name);
                let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
                Some((path, mtime))
            })
            .collect();
        if copies.len() < 2 {
            continue;
        }
        copies.sort_by_key(|(_, mtime)| *mtime);
        let (_newest, stale) = copies.split_last().expect("len >= 2");
        for (path, _) in stale {
            eprintln!(
                "workspace-lint: dropping duplicate build fragment {}",
                path.display()
            );
            let _ = std::fs::remove_file(path);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"{}").unwrap();
    }

    fn names_in(dir: &std::path::Path) -> BTreeSet<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The scoped pruner keeps every fragment whose crate part is a current
    /// workspace target — including integration-test (`+test`) and extra-bin
    /// (`@bin`, `@bin+test`) fragments whose crate name is NOT their package
    /// name, and package-keyed `<pkg>@build` fragments. It prunes only a
    /// genuinely-unknown crate (a rename/removal leftover) and never touches
    /// non-`.wlir` files.
    ///
    /// This is the direct regression guard for the scoped re-lint loop: with
    /// the old package-name-only keep-set, `engine_tests+test.wlir` and
    /// `uniffi_bindgen@bin.wlir` were deleted here every run.
    #[test]
    fn prune_nonmember_keeps_test_and_bin_fragments() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // syncfs shape: package `syncfs` owns test crate `engine_tests`;
        // package `syncfs-ffi` owns bin crate `uniffi_bindgen` + a build unit.
        for f in [
            "syncfs.wlir",
            "engine_tests+test.wlir",
            "uniffi_bindgen@bin.wlir",
            "uniffi_bindgen@bin+test.wlir",
            "syncfs_ffi@build.wlir",
            "renamed_old.wlir", // a since-renamed/removed crate: must be pruned
            "notes.txt",        // non-fragment: never touched
        ] {
            touch(dir, f);
        }
        // The crate/target-name vocabulary `crate_name_universe` would produce:
        // target names + package names, NOT just package names.
        let valid = set(&["syncfs", "syncfs_ffi", "engine_tests", "uniffi_bindgen"]);

        prune_nonmember_fragments(dir, &valid);

        assert_eq!(
            names_in(dir),
            set(&[
                "syncfs.wlir",
                "engine_tests+test.wlir",
                "uniffi_bindgen@bin.wlir",
                "uniffi_bindgen@bin+test.wlir",
                "syncfs_ffi@build.wlir",
                "notes.txt",
            ]),
            "only the renamed crate's fragment should be pruned"
        );
    }

    /// The whole-workspace pruner keeps exactly the expected keep-set and
    /// prunes every other `*.wlir`, leaving non-`.wlir` files alone. Both
    /// off-set classes are deleted alike — a current member's byproduct
    /// fragment (a plain lib fragment the `cargo test` config re-emits for
    /// every dirty member) and a genuinely-unknown crate's leftover — only
    /// the *message* distinguishes them (unknown-crate only; not assertable
    /// here, but the deletion contract is).
    #[test]
    fn prune_stale_keeps_only_expected() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for f in [
            "a+test.wlir",
            "b+test.wlir",
            "a.wlir",
            "old.wlir",
            "keep.txt",
        ] {
            touch(dir, f);
        }
        let keep = set(&["a+test.wlir", "b+test.wlir"]);
        let valid_crates = set(&["a", "b"]);

        prune_stale_fragments(dir, &keep, &valid_crates);

        assert_eq!(
            names_in(dir),
            set(&["a+test.wlir", "b+test.wlir", "keep.txt"]),
            "member byproduct (a.wlir) and unknown leftover (old.wlir) are both pruned"
        );
    }

    /// The message gate's vocabulary: the crate part is the stem up to the
    /// first `@`/`+` marker, whole stem otherwise.
    #[test]
    fn fragment_crate_part_parses_every_naming_form() {
        for (name, krate) in [
            ("wl_fast.wlir", "wl_fast"),
            ("wl_fast+test.wlir", "wl_fast"),
            ("workspace_lint@bin.wlir", "workspace_lint"),
            ("workspace_lint@bin+test.wlir", "workspace_lint"),
            ("wl_orchestrate@build.wlir", "wl_orchestrate"),
        ] {
            assert_eq!(fragment_crate_part(name), krate);
        }
    }
}
