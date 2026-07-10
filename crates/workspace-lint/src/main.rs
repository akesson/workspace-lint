mod baseline_write;
mod cli;
mod config;
mod directives;
mod docs;
mod dup_stats;
mod expand;
mod fix;
mod init;
// The message-surface catalog is a test fixture (every distinct diagnostic next
// to its rendered output) with no production caller — `scenarios()` is consumed
// only by the snapshot tests and the registry-coverage guard. Gate it out of
// shipped builds so it doesn't count against `crate-size`.
#[cfg(test)]
mod messages;
mod provision;
mod registry;
mod suggest;
mod suppress;

use clap::Parser;
use std::collections::{HashMap, HashSet};
use std::io;

use cli::{CheckRule, Cli, Commands};
use wl_diagnostic::Diagnostic;
use wl_diagnostic::render::{Format, render};
use wl_engine::fast::FastModel;
use wl_lint_api::{LintContext, LintId, git, util};

fn main() {
    let mut cli = Cli::parse();
    let format = parse_format(cli.message_format.as_deref());
    // `--fix-auto-delete` is `--fix` plus the unused-pub deletion cascade, so
    // every `--fix` behavior keys off the disjunction.
    let fix = cli.fix || cli.fix_auto_delete;

    match cli.command.take() {
        None => {
            config::reroot_to_config(true);
            run_default(&cli, format, fix)
        }
        Some(Commands::Init { force }) => {
            init::run(force);
        }
        Some(Commands::Check { rule }) => {
            // Same re-rooting as the default run: a `check` from a member dir
            // honors the root's `[lints]` levels and engine matrix (without
            // it, `try_load` found nothing and the engine ran memberless).
            config::reroot_to_config(false);
            run_check(rule, &cli, format, fix)
        }
        Some(Commands::Explain { lint }) => docs::explain(&lint),
        Some(Commands::Expand {
            command,
            glob,
            marker,
            auto_stage,
        }) => {
            // `expand` rewrites files (and may `git add`); gate it on a clean
            // tree just like `--fix`, so its changes stay reviewable. Override
            // with `--allow-dirty`.
            git::ensure_clean_for_fix(std::path::Path::new("."), cli.allow_dirty);
            let ec = CheckRule::into_expand_config(command, glob, marker, auto_stage);
            expand::run(&ec);
        }
        Some(Commands::DumpIr { file, json }) => {
            dump_ir(&file, json);
        }
    }
}

/// The default (no-subcommand) run: the full diagnostic pipeline described in
/// CLAUDE.md. Extracted from `main` so the CLI dispatch stays a thin match.
fn run_default(cli: &Cli, format: Format, fix: bool) {
    // `--fix` mutates tracked files in place; gate on a clean working
    // tree up front so the whole change stays reviewable as one diff.
    if fix {
        git::ensure_clean_for_fix(std::path::Path::new("."), cli.allow_dirty);
    }
    let (config, config_diags) = wl_engine::timing::phase("config::load", config::load);
    // `--baseline-write` regenerates the `[duplicate-code]` baseline and exits.
    // Placed before the fix/expand work so it is a pure read-then-write of the
    // one file; default-run only (rejected under `check`) so the recorded set
    // matches the config CI lints with.
    if cli.baseline_write {
        baseline_write::run(&config);
    }
    // Hidden debug surface: exercise the (still dark) semantic-tier
    // plumbing end-to-end — extract on the pinned toolchain, assemble,
    // print stats — then exit 0 without running any lints.
    if cli.engine_dump {
        let model = load_semantic_model(config.engine.selectors());
        print_engine_stats(&model);
        return;
    }
    // `expand` mutates files in place (and may `git add` with
    // `auto-stage`), so it runs only under `--fix` — gated by the
    // clean-tree check above. This keeps the default/editor run (e.g.
    // rust-analyzer's `check.overrideCommand`) side-effect-free. It runs
    // before lints so the structural pass measures post-expansion files.
    if fix && let Some(ref ec) = config.expand {
        expand::run(ec);
    }
    let RunOutput {
        mut diagnostics,
        fast,
        semantic,
        cfg_shadow,
        mut ran,
    } = wl_engine::timing::phase("run_all", || run_all(&config, cli.fast_only));
    // Config-audit findings (below) are produced on every default
    // run, so expects for `config` are always judgeable here.
    ran.insert(LintId::Config);
    // Config-validation findings join the stream before suppression
    // (so a `# workspace-lint: allow(config)` directive can silence
    // them) and before leveling (so `[lints] config = …` applies).
    diagnostics.extend(config_diags);
    // The per-crate `[crates.<name>]` tier's crate names are validated
    // against the resolved workspace membership (a typo'd or stale crate
    // name is otherwise a silent no-op). Needs the loaded `FastModel`,
    // so it runs here rather than in the pure-TOML config audit.
    if let Some(fm) = fast.as_ref() {
        diagnostics.extend(config::audit_crate_membership(&config, fm));
    }
    // Backfill the FastModel when no enabled lint required it: the
    // generated-file drop and the suppression scanner's parse cache
    // read it. Best-effort — outside a cargo workspace both consumers
    // degrade gracefully.
    let fast = fast.or_else(try_load_fast_model);
    // Generated (`include!`d) code participates in analysis but is not a
    // place users can act on; drop findings anchored in it before
    // suppression so they never consume an `expect!` or skew stale-expect.
    // (unused-pub also excludes generated candidates at the lint layer —
    // `ir::generated_set` — because the `--fix-auto-delete` cascade below
    // *replaces* its findings after this drop; here the drop covers every
    // other lint's anchors.)
    drop_generated_anchored(fast.as_ref(), &mut diagnostics);
    // Under `--fix-auto-delete` (and only then — plain `--fix` never
    // deletes), converge unused-pub deletions in one pass: deleting
    // a dead item frees whatever it solely reached. The cascade replaces
    // the plain unused-pub findings with the converged set and emits the
    // import surgery that keeps the tree compiling. Runs *before*
    // suppression so its findings (some newly `Unused` this pass) flow
    // through directive filtering + stale-expect accounting normally;
    // its own removal decisions consult a non-mutating suppression query
    // so an `expect!`-silenced item — and its callees — stay intact.
    let mut trimmed_imports = false;
    if cli.fix_auto_delete
        && ran.contains(&LintId::UnusedPub)
        && let (Some(fm), Some(sm)) = (fast.as_ref(), semantic.as_ref())
    {
        trimmed_imports = run_unused_pub_cascade(
            config.unused_pub.clone().unwrap_or_default(),
            config.unused_pub_overrides(),
            cfg_shadow.as_ref(),
            fm,
            sm,
            &mut diagnostics,
        );
    }
    // Suppression filters by directives and appends `stale-expect` /
    // `unknown-lint` findings; leveling runs last so the appended ones
    // are leveled too and `allow`-ed lints are dropped from the final
    // set before the exit-code tally.
    wl_engine::timing::phase("apply_suppression[scan expect!/allow!/directives]", || {
        apply_suppression(fast.as_ref(), ran, &mut diagnostics)
    });
    apply_lint_levels(&config, fast.as_ref(), &mut diagnostics);
    if fix {
        let summary = fix::run(&diagnostics);
        // Any applied fix can leave fmt-relevant residue (a
        // `pub`→`pub(crate)` rewrite alone can push a line past the
        // width limit), deletions especially so.
        if trimmed_imports || summary.deleted_any || summary.modified > 0 {
            eprintln!("note: run `cargo fmt` to tidy the fixed files");
        }
        // A fix run converges by re-running, not in one pass: a narrowing
        // lowers what its signature exposes (so types it pinned `pub` become
        // tightenable), and a deletion can flip verdicts the *next* compile
        // sees. Deletions cascade in-pass; narrow consequences can't. Say so
        // rather than letting the next run's new fixes read as a bug.
        if summary.modified > 0 {
            eprintln!(
                "note: applied fixes can enable further tightenings — re-run until no fixes remain"
            );
        }
    }
    report_and_exit(diagnostics, format);
}

/// `workspace-lint check <rule>`: one lint, CLI-configured, sharing the default
/// run's suppression/leveling/fix tail.
fn run_check(rule: CheckRule, cli: &Cli, format: Format, fix: bool) {
    if cli.baseline_write {
        util::fail(
            "--baseline-write only works on the default run — the baseline must be \
             generated under the repo config CI lints with",
        );
    }
    // `--stats` measures instead of linting: every group `find_clones`
    // returns plus its literal divergence, no suppression or leveling — the
    // raw view the duplicate-code thresholds are calibrated against.
    if let Some(config) = rule.duplicate_code_stats() {
        let report = wl_lints::duplicate_code::measure(&load_fast_model(), &config);
        print!("{}", dup_stats::render(&report));
        std::process::exit(0);
    }
    if fix {
        git::ensure_clean_for_fix(std::path::Path::new("."), cli.allow_dirty);
    }
    // For single-check runs we still honor the `[lints]` table if a
    // config file exists, so `workspace-lint check file-size` and the
    // default run agree on severity.
    let config_for_levels = config::try_load();
    // Resolved up front (the rule is consumed by `run_single_check`):
    // the `--fix-auto-delete` cascade below needs the same config the
    // lint itself ran with.
    let unused_pub_config = rule.unused_pub_config();
    let RunOutput {
        mut diagnostics,
        fast,
        semantic,
        cfg_shadow,
        ran,
    } = run_single_check(rule, cli.fast_only, config_for_levels.as_ref());
    // Same backfill as the default run: the drop and the directive
    // scanner's parse cache want the FastModel even when the single
    // checked rule didn't require one.
    let fast = fast.or_else(try_load_fast_model);
    drop_generated_anchored(fast.as_ref(), &mut diagnostics);
    // The deletion cascade, exactly as in the default run — without it
    // a `check unused-pub --fix-auto-delete` would delete items but
    // leave their `use` sites dangling (E0432). No per-crate tier on
    // the CLI surface.
    if cli.fix_auto_delete
        && let Some(global) = unused_pub_config
        && let (Some(fm), Some(sm)) = (fast.as_ref(), semantic.as_ref())
    {
        run_unused_pub_cascade(
            global,
            HashMap::new(),
            cfg_shadow.as_ref(),
            fm,
            sm,
            &mut diagnostics,
        );
    }
    apply_suppression(fast.as_ref(), ran, &mut diagnostics);
    if let Some(cfg) = &config_for_levels {
        apply_lint_levels(cfg, fast.as_ref(), &mut diagnostics);
    }
    if fix {
        fix::run(&diagnostics);
    }
    report_and_exit(diagnostics, format);
}

/// Decode a single `.wlir` IR fragment and print it — the debug window into the
/// binary transport (the reason `wl_ir` keeps serde). Never runs extraction — a
/// pure file read. This is the I/O + exit-code shell; the decode/render is the
/// pure [`render_ir`], so the interesting logic is unit-tested without a
/// tempfile or a subprocess.
fn dump_ir(path: &std::path::Path, json: bool) -> ! {
    let rendered = std::fs::read(path)
        .map_err(|e| format!("reading {}: {e}", path.display()))
        .and_then(|bytes| render_ir(&bytes, json));
    match rendered {
        Ok(text) => {
            println!("{text}");
            std::process::exit(0);
        }
        Err(message) => {
            eprintln!("dump-ir: {message}");
            std::process::exit(2);
        }
    }
}

/// Validate a `.wlir` fragment's header, deserialize the rkyv archive to an owned
/// [`wl_engine::wl_ir::IrFragment`], and render it as serde JSON (`--json`,
/// jq-able) or Rust `{:#?}` debug. Pure over the file bytes (header validated
/// before the archive slice, so a short buffer errors rather than panics).
fn render_ir(bytes: &[u8], json: bool) -> Result<String, String> {
    use wl_engine::wl_ir;
    wl_ir::validate_header(bytes).map_err(|e| e.to_string())?;
    let frag = wl_ir::from_archive_bytes(&bytes[wl_ir::HEADER_LEN..]).map_err(|e| e.to_string())?;
    if json {
        serde_json::to_string_pretty(&frag).map_err(|e| e.to_string())
    } else {
        Ok(format!("{frag:#?}"))
    }
}

/// Apply the lint-level cascade to the collected diagnostics: **drop** any
/// whose effective level is `allow`, and rewrite the rest to their effective
/// level. The level resolves through the per-crate tier first — each
/// diagnostic is mapped to its owning workspace member (by its silence
/// anchor's path), then leveled via [`config::Config::effective_level`]
/// (per-crate override → per-crate default → global override → global default
/// → built-in `warn`). Diagnostics whose level the lint chose itself
/// (`level_is_explicit`, e.g. an `architecture` rule's `severity`) are left
/// untouched, so a blanket `[lints] <lint> = …` can't silently clobber a
/// deliberate per-rule severity.
fn apply_lint_levels(
    config: &config::Config,
    fast: Option<&FastModel>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let crate_dirs = crate_dirs(fast);
    diagnostics.retain_mut(|d| {
        if d.level_is_explicit {
            return true;
        }
        let Some(id) = LintId::from_short(d.lint_short()) else {
            // A diagnostic carrying an unknown lint id shouldn't happen (they
            // all come from `LintId::*.id()`); keep it rather than drop.
            return true;
        };
        let krate = owning_crate(&crate_dirs, &d.silence_anchor);
        match config.effective_level(id, krate).to_diagnostic_level() {
            None => false, // `allow` → drop before render & exit-code tally
            Some(level) => {
                d.level = level;
                true
            }
        }
    });
}

/// A workspace member's manifest-dir match candidates. Each member carries
/// **both** its workspace-relative and absolute manifest dir, because
/// diagnostics anchor with mixed path bases: `unused-deps` / `file-size` /
/// `crate-size` emit workspace-relative paths, while resolver-span lints
/// (`unused-pub`) emit absolute ones. Matching either form maps any diagnostic
/// to its crate. `depth` is the relative component count, used to match the
/// most specific crate first for nested layouts.
struct CrateDir {
    forms: Vec<std::path::PathBuf>,
    name: String,
    depth: usize,
}

/// Build the per-crate match table. Empty when no [`FastModel`] was loaded —
/// then every diagnostic resolves to the global level.
fn crate_dirs(fast: Option<&FastModel>) -> Vec<CrateDir> {
    let Some(fm) = fast else {
        return Vec::new();
    };
    let mut dirs: Vec<CrateDir> = fm
        .members()
        .iter()
        .map(|c| {
            let rel = fm.crate_relative_path(&c.manifest_dir);
            let depth = rel.components().count();
            CrateDir {
                forms: vec![rel, c.manifest_dir.clone()],
                name: c.name.clone(),
                depth,
            }
        })
        .collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.depth));
    dirs
}

/// The Cargo name of the workspace member that owns `anchor`, found by matching
/// the anchor's path (in either base) against each member's manifest-dir forms.
/// `None` for a workspace-level anchor or a path outside every member. An empty
/// (root) relative form is skipped so it can't match every path.
fn owning_crate<'a>(
    crate_dirs: &'a [CrateDir],
    anchor: &wl_diagnostic::SilenceAnchor,
) -> Option<&'a str> {
    let file = anchor.file()?;
    crate_dirs
        .iter()
        .find(|cd| {
            cd.forms
                .iter()
                .any(|f| !f.as_os_str().is_empty() && file.starts_with(f))
        })
        .map(|cd| cd.name.as_str())
}

fn parse_format(arg: Option<&str>) -> Format {
    match arg {
        None => Format::Human,
        Some(s) => Format::parse(s).unwrap_or_else(|e| {
            eprintln!("error: {e}");
            std::process::exit(2);
        }),
    }
}

/// Drop every diagnostic anchored inside a file that was spliced into the model
/// via `include!(...)` (generated code). Generated code *participates* in
/// analysis — its references count, so it clears `unused-deps` / `unused-pub` /
/// `orphan-file` false positives — but it is not a place a user can act on, so a
/// finding *on* it (a generated `pub fn` reported unused, a long generated file,
/// …) is noise. Runs **before** [`apply_suppression`] so a generated finding
/// never consumes an `expect!` directive or pollutes the `stale-expect` tally.
///
/// The drop set is the [`FastModel`]'s generated files: its build-free walk
/// resolves literal / `CARGO_*` includes — the *checked-in* generated files
/// lints can anchor findings in. (`OUT_DIR` splices never enter any model —
/// the rustc engine's fragments live under the target directory and are
/// filtered by the lints themselves.) No model (not a loadable cargo
/// workspace) degrades to no drop rather than failing the run.
///
/// Generated paths are absolute; lints anchor workspace-relative. We normalize
/// each generated path to that same workspace-relative form via
/// `crate_relative_path` (which also reconciles `/var`↔`/private/var` and
/// Windows UNC prefixes) and match anchors by **exact** path equality. A loose
/// suffix match would let a generated `crates/a/src/x.rs` drop a real finding on
/// a *different* crate's handwritten `crates/b/src/x.rs` (or a bare `src/x.rs`).
fn drop_generated_anchored(fast: Option<&FastModel>, diagnostics: &mut Vec<Diagnostic>) {
    let Some(fm) = fast else {
        return;
    };
    // One normalizer for both the generated set and the anchors, so the
    // equality test can't be skewed by two implementations.
    let normalize = |p: &std::path::Path| fm.crate_relative_path(p);
    let generated: std::collections::HashSet<std::path::PathBuf> =
        fm.generated_files().map(normalize).collect();
    if generated.is_empty() {
        return;
    }
    diagnostics.retain(|d| match d.silence_anchor.file().map(normalize) {
        Some(file) => !generated.contains(&file),
        None => true,
    });
}

/// Best-effort [`FastModel`] load used when no enabled lint required one: the
/// generated-file drop and the suppression scanner's parse cache still want it.
/// Non-fatal: a directory that isn't a loadable cargo workspace yields `None`
/// (the drop is skipped; the directive scan parses on demand). Lints that
/// DECLARE `needs_fast` — since the measurement-sweep port that includes
/// `check file-size` — instead go through the loud-fail load: they are
/// workspace-rooted by design and exit 2 outside one.
fn try_load_fast_model() -> Option<FastModel> {
    FastModel::load(std::path::Path::new(".")).ok()
}

/// Scan the workspace for `allow!`/`expect!` directives and use them to
/// filter the diagnostic stream. Stale `expect` directives are appended as
/// new diagnostics — these pass back through the suppression map so an
/// `allow(stale-expect)` directive silences them (e.g. README example code
/// that mentions an expect directive without intending to fire one).
///
/// When a [`FastModel`] is available, the scanner uses its known module list
/// to parse each backing `.rs` file exactly once up front (the model itself
/// stores only file *paths*, not ASTs — it re-parses on demand to stay
/// `Send + Sync`). Without one (a non-cargo directory), we fall back to the
/// per-file on-demand parse path.
fn apply_suppression(
    fast: Option<&FastModel>,
    ran: HashSet<LintId>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let directives_list = match fast {
        Some(fm) => directives::scan_with_model(fm),
        None => directives::scan(std::path::Path::new(".")),
    };
    // Validate the lint names referenced by directives before consuming the
    // list — a typo'd `expect!(file_siz)` is otherwise a silent no-op.
    let mut unknown = suppress::unknown_lint_diagnostics(&directives_list);
    let mut map = suppress::SuppressionMap::from_directives(directives_list);
    suppress::apply(&mut map, diagnostics);
    // This function itself produces `stale-expect` / `unknown-lint`, so
    // expects targeting them are always judgeable, whatever the caller ran.
    let mut ran = ran;
    ran.insert(LintId::StaleExpect);
    ran.insert(LintId::UnknownLint);
    // The origin paths in the map are relative to this same root (the fast
    // tier's root, or `.` on the model-less fallback), so `root.join(origin)`
    // round-trips when `stale_expects` reads a file to build its deletion span.
    let root = fast
        .map(FastModel::root)
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut stale = map.stale_expects(&ran, root);
    suppress::apply(&mut map, &mut stale);
    suppress::apply(&mut map, &mut unknown);
    diagnostics.extend(stale);
    diagnostics.extend(unknown);
}

/// The `--fix-auto-delete` unused-pub cascade: replace the plain unused-pub
/// findings with the transitively-converged set and append the import surgery.
/// Builds a read-only [`suppress::SuppressionMap`] so an `expect!`/`allow!`-
/// silenced deletion never seeds a removal (its callees stay live) — the map
/// is scanned again by [`apply_suppression`] for the real filtering, a cheap
/// re-scan paid only under `--fix-auto-delete`.
/// Returns `true` when the cascade emitted import surgery — the caller prints a
/// `cargo fmt` hint after applying, since trimming `use` leaves valid but
/// unformatted residue (collapsed blank lines, single-element `{…}` groups).
fn run_unused_pub_cascade(
    global: wl_lints::unused_pub::UnusedPubConfig,
    per_crate: HashMap<String, wl_lints::unused_pub::UnusedPubConfig>,
    cfg_shadow: Option<&wl_engine::coverage::CfgShadow>,
    fast: &FastModel,
    semantic: &wl_engine::SemanticModel,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let directives_list = directives::scan_with_model(fast);
    let map = suppress::SuppressionMap::from_directives(directives_list);
    let suppressed = |d: &Diagnostic| map.would_suppress(d);
    // The cfg-shadow veto's index, computed by the runner alongside the
    // semantic tier and reused here. `None` only when the runner never built
    // one (a run path without the semantic tier can't reach this cascade);
    // the empty fallback then means "no vetoes", matching a workspace with
    // no cfg-gated code.
    let owned;
    let shadow = match cfg_shadow {
        Some(s) => s,
        None => {
            owned = wl_engine::coverage::CfgShadow::default();
            &owned
        }
    };

    let result = wl_lints::unused_pub::cascade::run(
        &wl_lint_api::config::PerCrate::new(global, per_crate),
        fast,
        semantic,
        shadow,
        &suppressed,
    );

    // The cascade output is the authoritative unused-pub picture — swap it in
    // for the plain-check findings, then add the dangling-`use` deletions.
    let unused_pub_id = LintId::UnusedPub.id();
    let did_surgery = !result.surgery.is_empty();
    diagnostics.retain(|d| d.lint != unused_pub_id);
    diagnostics.extend(result.diagnostics);
    diagnostics.extend(result.surgery);
    did_surgery
}

/// The default run's lint pipeline. The returned [`LintId`] set records which
/// lints actually ran (post-`--fast-only`, post-`allow`) — the staleness
/// domain for `expect` directives.
/// One lint pass's outputs: the diagnostic stream plus the shared models the
/// caller's `--fix-auto-delete` cascade reuses, and the ran-set that scopes
/// `stale-expect`.
struct RunOutput {
    diagnostics: Vec<Diagnostic>,
    fast: Option<FastModel>,
    semantic: Option<wl_engine::SemanticModel>,
    cfg_shadow: Option<wl_engine::coverage::CfgShadow>,
    ran: HashSet<LintId>,
}

fn run_all(config: &config::Config, fast_only: bool) -> RunOutput {
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
    let semantic = (needs_semantic && has_members).then(|| {
        wl_engine::timing::phase("SEMANTIC (extract+assemble)", || {
            load_semantic_model(config.engine.selectors())
        })
    });
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
    };
    let ran: HashSet<LintId> = registry.iter().map(|l| l.id()).collect();
    let diagnostics: Vec<Diagnostic> = wl_engine::timing::phase("LINTS (all)", || {
        registry
            .iter()
            .flat_map(|l| wl_engine::timing::phase(l.id().short(), || l.check(&cx)))
            .collect()
    });
    // `cx`'s borrows end here (last use above), so the models move out for
    // the `--fix` cascade the caller runs.
    RunOutput {
        diagnostics,
        fast,
        semantic,
        cfg_shadow: shadow,
        ran,
    }
}

fn run_single_check(
    rule: CheckRule,
    fast_only: bool,
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
        load_semantic_model(engine.selectors())
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
    }
}

/// Build the full (rustc-backed) tier: vendored extractor → one embedded
/// dylint extraction per config → Phase-2 assembly. Loud-fail on error,
/// mirroring [`load_fast_model`], but printing the error `Display` verbatim —
/// the `EngineError` toolchain variants carry the full rustup remediation
/// text, which must reach the user unwrapped and untruncated. On an
/// interactive terminal, provisionable preflight failures (missing pinned
/// toolchain / component / dylint-link) are first offered as a one-keypress
/// install-and-retry ([`provision::Provisioner`]); each stage repairs at most
/// once, so the loop is bounded by the number of preflight checks.
///
/// NOTE (deferred): today extraction runs before any lint, so an extraction
/// failure aborts the run before the fast-tier lints report. Reordering so
/// fast lints still report their findings first is deferred to the first
/// semantic-lint port, where the behavior becomes testable.
fn load_semantic_model(configs: Vec<wl_engine::ConfigSpec>) -> wl_engine::SemanticModel {
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
                Ok(runs) => break runs,
                // Returning at all means a remediation ran successfully → retry;
                // every other path exits inside.
                Err(e) => provisioner.repair_or_fail(&e),
            }
        }
    });
    wl_engine::timing::phase("ASSEMBLE (phase 2)", || {
        wl_engine::SemanticModel::load(&runs).unwrap_or_else(|e| util::fail(e))
    })
}

/// The hidden `--engine-dump` debug output: a compact plain-line stats block
/// on stdout summarizing one extract→assemble round trip. This is the only
/// consumer of the semantic model until the first semantic-lint port.
fn print_engine_stats(model: &wl_engine::SemanticModel) {
    let configs: Vec<&str> = model.config_ids().collect();
    println!("configs: {}", configs.join(", "));
    println!(
        "primary import edges: {}",
        model.primary().import_edge_count()
    );
    let union = model.union_verdict();
    println!(
        "unused-pub union: {} lead(s), {} retired by the matrix, {} primary-only",
        union.leads.len(),
        union.retired.len(),
        union.primary_only_leads
    );
    let deps = model.deps_verdict();
    let unused: usize = deps.crates.iter().map(|c| c.unused.len()).sum();
    let flagged = deps.crates.iter().filter(|c| !c.unused.is_empty()).count();
    println!(
        "unused-deps: {unused} unused dep(s) across {flagged} crate(s); dev deps judged: {}",
        if deps.dev_deps_judged { "yes" } else { "no" }
    );
}

/// Load the build-free `FastModel` (`cargo metadata` + parsed manifests +
/// the lean syntactic module trees). Loud-fail on error: a silent `None`
/// would mask a broken state in CI.
fn load_fast_model() -> FastModel {
    FastModel::load(std::path::Path::new(".")).unwrap_or_else(|e| {
        util::fail(format!(
            "failed to load workspace metadata for manifest-backed lints: {e}"
        ))
    })
}

fn report_and_exit(diagnostics: Vec<Diagnostic>, format: Format) {
    // Human format goes to stderr (so JSON/GitHub piping is clean); machine
    // formats go to stdout.
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let out: &mut dyn io::Write = match format {
        Format::Human => &mut stderr,
        Format::Json | Format::Github => &mut stdout,
    };
    let deny_count = wl_engine::timing::phase("render", || render(format, &diagnostics, out))
        .unwrap_or_else(|e| util::fail(format!("error: failed to write diagnostics: {e}")));
    // Exit-code policy (see [`util::fail`]): only a surviving `Deny` flips the
    // code to `1` ("the linted code has findings"); operational failures use `2`.
    // Configure escalation via `[lints]`; without it every diagnostic stays
    // advisory and the run exits `0`.
    if deny_count > 0 {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod dump_ir_tests {
    use super::render_ir;
    use wl_engine::wl_ir::{self, IrFragment, SCHEMA_VERSION};

    fn sample_wlir() -> Vec<u8> {
        wl_ir::write_bytes(&IrFragment {
            schema_version: SCHEMA_VERSION,
            crate_name: "demo".into(),
            target_kind: "lib".into(),
            is_test_cfg: false,
            items: vec![],
            references: vec![],
            loaded_files: vec![],
        })
        .expect("fixture fragment serializes")
    }

    #[test]
    fn renders_json_and_debug() {
        let bytes = sample_wlir();
        let json = render_ir(&bytes, true).expect("json render");
        assert!(json.trim_start().starts_with('{'), "not JSON:\n{json}");
        assert!(json.contains("demo"), "crate name missing:\n{json}");
        let debug = render_ir(&bytes, false).expect("debug render");
        assert!(debug.contains("demo"), "crate name missing:\n{debug}");
    }

    #[test]
    fn rejects_bad_header() {
        assert!(render_ir(b"not a wlir file", true).is_err());
    }
}
