mod cli;
mod config;
mod deep;
mod diagnostic;
mod directives;
mod expand;
mod fix;
mod git;
mod lints;
mod messages;
mod suggest;
mod suppress;
mod util;

use clap::Parser;
use std::io;

use cli::{CheckRule, Cli, Commands};
use config::MacrosConfig;
use diagnostic::Diagnostic;
use diagnostic::render::{Format, render};
use lints::{LintContext, LintId};
use syn_workspace::{LoadOptions, Workspace};

fn main() {
    let cli = Cli::parse();
    let format = parse_format(cli.message_format.as_deref());

    match cli.command {
        None => {
            // `--fix` mutates tracked files in place; gate on a clean working
            // tree up front so the whole change stays reviewable as one diff.
            if cli.fix {
                git::ensure_clean_for_fix(std::path::Path::new("."), cli.allow_dirty);
            }
            let (config, config_diags) = config::load();
            // `expand` mutates files in place (and may `git add` with
            // `auto-stage`), so it runs only under `--fix` — gated by the
            // clean-tree check above. This keeps the default/editor run (e.g.
            // rust-analyzer's `check.overrideCommand`) side-effect-free. It runs
            // before lints so the structural pass measures post-expansion files.
            if cli.fix
                && let Some(ref ec) = config.expand
            {
                expand::run(ec);
            }
            let (mut diagnostics, workspace) = run_all(&config, !cli.no_build_env);
            // Config-validation findings join the stream before suppression
            // (so a `# workspace-lint: allow(config)` directive can silence
            // them) and before leveling (so `[lints] config = …` applies).
            diagnostics.extend(config_diags);
            // The per-crate `[crates.<name>]` tier's crate names are validated
            // against the resolved workspace membership (a typo'd or stale crate
            // name is otherwise a silent no-op). Needs the loaded `Workspace`,
            // so it runs here rather than in the pure-TOML config audit.
            if let Some(ws) = workspace.as_ref() {
                diagnostics.extend(config::audit_crate_membership(&config, ws));
            }
            // Generated (`include!`d) code participates in analysis but is not a
            // place users can act on; drop findings anchored in it before
            // suppression so they never consume an `expect!` or skew stale-expect.
            drop_generated_anchored(workspace.as_ref(), &mut diagnostics);
            // Suppression filters by directives and appends `stale-expect` /
            // `unknown-lint` findings; leveling runs last so the appended ones
            // are leveled too and `allow`-ed lints are dropped from the final
            // set before the exit-code tally.
            apply_suppression(workspace.as_ref(), &mut diagnostics);
            apply_lint_levels(&config, workspace.as_ref(), &mut diagnostics);
            if cli.fix {
                apply_fix(
                    cli.no_deep,
                    cli.scip_index.as_deref(),
                    &mut diagnostics,
                    workspace.as_ref(),
                );
            }
            report_and_exit(diagnostics, format);
        }
        Some(Commands::Done) => {
            // `done` only touches freshness targets; config diagnostics aren't
            // rendered here, so drop them.
            let (config, _) = config::load();
            if let Some(ref fc) = config.freshness {
                lints::freshness::mark_done(fc);
            }
        }
        Some(Commands::Check { rule }) => {
            if cli.fix {
                git::ensure_clean_for_fix(std::path::Path::new("."), cli.allow_dirty);
            }
            // For single-check runs we still honor the `[lints]` table if a
            // config file exists, so `workspace-lint check file-size` and the
            // default run agree on severity.
            let config_for_levels = config::try_load();
            let (mut diagnostics, workspace) = run_single_check(rule, !cli.no_build_env);
            drop_generated_anchored(workspace.as_ref(), &mut diagnostics);
            apply_suppression(workspace.as_ref(), &mut diagnostics);
            if let Some(cfg) = &config_for_levels {
                apply_lint_levels(cfg, workspace.as_ref(), &mut diagnostics);
            }
            if cli.fix {
                apply_fix(
                    cli.no_deep,
                    cli.scip_index.as_deref(),
                    &mut diagnostics,
                    workspace.as_ref(),
                );
            }
            report_and_exit(diagnostics, format);
        }
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
    }
}

/// The `--fix` action shared by the default and `check` runs. Unless
/// `--no-deep` is set, deep verification (rust-analyzer SCIP) runs first: it
/// downgrades any disproved structural suggestion (so `fix::run` skips it) and
/// returns the `expect` directive insertions to write for those false
/// positives. `fix::run` then applies the surviving structural fixes plus those
/// insertions in one pass. The clean-tree gate already ran at the call site, so
/// every change here lands in a reviewable working tree.
fn apply_fix(
    no_deep: bool,
    scip_index: Option<&std::path::Path>,
    diagnostics: &mut [Diagnostic],
    workspace: Option<&Workspace>,
) {
    let inserts = if no_deep {
        Vec::new()
    } else {
        deep::verify_findings(diagnostics, workspace, scip_index)
    };
    fix::run(diagnostics, &inserts);
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
    workspace: Option<&Workspace>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let crate_dirs = crate_dirs(workspace);
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

/// Build the per-crate match table. Empty when no workspace was loaded — then
/// every diagnostic resolves to the global level.
fn crate_dirs(workspace: Option<&Workspace>) -> Vec<CrateDir> {
    let Some(ws) = workspace else {
        return Vec::new();
    };
    let mut dirs: Vec<CrateDir> = ws
        .members()
        .map(|c| {
            let rel = ws.crate_relative_path(&c.manifest_dir);
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
    anchor: &diagnostic::SilenceAnchor,
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

/// Scan the workspace for `allow!`/`expect!` directives and use them to
/// filter the diagnostic stream. Stale `expect` directives are appended as
/// new diagnostics — these pass back through the suppression map so an
/// `allow(stale-expect)` directive silences them (e.g. README example code
/// that mentions an expect directive without intending to fire one).
///
/// When a [`Workspace`] is available, the scanner uses its known module list
/// to parse each backing `.rs` file exactly once up front (the `Workspace`
/// itself stores only file *paths*, not ASTs — it re-parses on demand to stay
/// `Send + Sync`). Without one (e.g. for lint subcommands that don't load a
/// workspace), we fall back to the per-file on-demand parse path.
/// Drop every diagnostic anchored inside a file that was spliced into the model
/// via `include!(...)` (generated code). Generated code *participates* in
/// analysis — its references count, so it clears `unused-deps` / `unused-pub` /
/// `module-tree` false positives — but it is not a place a user can act on, so a
/// finding *on* it (a generated `pub fn` reported unused, a long generated file,
/// …) is noise. Runs **before** [`apply_suppression`] so a generated finding
/// never consumes an `expect!` directive or pollutes the `stale-expect` tally.
/// File comparison is path-base-insensitive ([`diagnostic::SilenceAnchor::same_file`])
/// because generated paths are absolute while many lints anchor workspace-relative.
fn drop_generated_anchored(workspace: Option<&Workspace>, diagnostics: &mut Vec<Diagnostic>) {
    let Some(ws) = workspace else {
        return;
    };
    let generated: Vec<&std::path::Path> = ws.generated_files().collect();
    if generated.is_empty() {
        return;
    }
    diagnostics.retain(|d| match d.silence_anchor.file() {
        Some(file) => !generated
            .iter()
            .any(|g| diagnostic::SilenceAnchor::same_file(file, g)),
        None => true,
    });
}

fn apply_suppression(
    workspace: Option<&syn_workspace::Workspace>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let directives_list = match workspace {
        Some(ws) => directives::scan_with_workspace(ws),
        None => directives::scan(std::path::Path::new(".")),
    };
    // Validate the lint names referenced by directives before consuming the
    // list — a typo'd `expect!(file_siz)` is otherwise a silent no-op.
    let mut unknown = suppress::unknown_lint_diagnostics(&directives_list);
    let mut map = suppress::SuppressionMap::from_directives(directives_list);
    suppress::apply(&mut map, diagnostics);
    let mut stale = map.stale_expects();
    suppress::apply(&mut map, &mut stale);
    suppress::apply(&mut map, &mut unknown);
    diagnostics.extend(stale);
    diagnostics.extend(unknown);
}

fn run_all(
    config: &config::Config,
    harvest_build_env: bool,
) -> (Vec<Diagnostic>, Option<Workspace>) {
    let registry = lints::registry(config);
    // A loaded workspace is needed when some enabled lint asks for it, or when
    // a per-crate `[crates.*]` tier is present — the latter so per-crate levels
    // can map diagnostics to their owning crate and crate names can be
    // validated against the membership, even if no lint itself needs the resolver.
    let needs_ws =
        registry.iter().any(|l| l.requirements().needs_workspace) || !config.crates.is_empty();
    let workspace = needs_ws.then(|| load_workspace(config.macros.as_ref(), harvest_build_env));
    let cx = LintContext {
        workspace: workspace.as_ref(),
    };
    let diagnostics: Vec<Diagnostic> = registry.iter().flat_map(|l| l.check(&cx)).collect();
    (diagnostics, workspace)
}

fn run_single_check(
    rule: CheckRule,
    harvest_build_env: bool,
) -> (Vec<Diagnostic>, Option<Workspace>) {
    let lint = rule.into_lint();
    let workspace = lint
        .requirements()
        .needs_workspace
        .then(|| load_workspace(None, harvest_build_env));
    let cx = LintContext {
        workspace: workspace.as_ref(),
    };
    (lint.check(&cx), workspace)
}

/// Load the resolver-backed `Workspace` and (when configured) register the
/// external-macro expansion-uses set. Loud-fail on resolver error: a silent
/// `Vec::new()` would mask a broken state in CI. Non-fatal load warnings
/// (auxiliary targets that failed to parse) are surfaced to stderr —
/// `Workspace` no longer prints them itself.
fn load_workspace(macros: Option<&MacrosConfig>, harvest_build_env: bool) -> Workspace {
    let opts = LoadOptions {
        harvest_build_env,
        ..LoadOptions::default()
    };
    let mut ws = Workspace::load_with_options(".", opts).unwrap_or_else(|e| {
        util::fail(format!(
            "failed to load workspace for resolver-backed lints: {e}"
        ))
    });
    for w in ws.warnings() {
        // Label as a warning so a degraded load (e.g. a failed build-env harvest
        // that silently falls back to literal-only include resolution) reads as
        // something gone wrong rather than blending into normal output.
        eprintln!("workspace-lint: warning: {w}");
    }
    if let Some(m) = macros {
        let paths = m.external.iter().flat_map(|m| {
            m.expansion_uses
                .iter()
                .map(|p| syn_workspace::ResolvedPath::from_user_str(p))
        });
        ws.register_external_macro_uses(paths);
    }
    ws
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
    let deny_count = render(format, &diagnostics, out)
        .unwrap_or_else(|e| util::fail(format!("error: failed to write diagnostics: {e}")));
    // Exit-code policy (see [`util::fail`]): only a surviving `Deny` flips the
    // code to `1` ("the linted code has findings"); operational failures use `2`.
    // Configure escalation via `[lints]`; without it every diagnostic stays
    // advisory and the run exits `0`.
    if deny_count > 0 {
        std::process::exit(1);
    }
}
