mod cli;
mod config;
#[allow(dead_code)]
// Diagnostic types include a few helpers (some Applicability variants,
// clean_path/clean_pathbuf) that aren't yet referenced; upcoming steps
// (--fix, snapshot tests) will use them. Suppress until then to keep
// `cargo clippy -D warnings` green.
mod diagnostic;
mod directives;
mod expand;
mod fix;
#[allow(dead_code)]
// `LintId::ALL`, `FIXTURABLE_LINTS`, and the `Lint::id()` trait method are
// referenced from the registry-coverage and scenario tests, not the binary
// runtime. The string-form `lint` field on each `Diagnostic` is what drives
// suppression, rendering, and severity lookup.
mod lints;
#[allow(dead_code)]
// Compiled into the binary only because all module-level tests must be
// visible to `cargo test`. The `scenarios()` builder is `pub` but the
// snapshot tests inside `mod tests` are what actually exercise the
// diagnostics — see the file's module docs.
mod messages;
mod suggest;
mod suppress;

use clap::Parser;
use std::io::{self, IsTerminal};

use cli::{CheckRule, Cli, Commands};
use config::MacrosConfig;
use diagnostic::Diagnostic;
use diagnostic::render::{Format, render};
use lints::{LintContext, LintId};
use syn_workspace::Workspace;

fn main() {
    let cli = Cli::parse();
    let format = parse_format(cli.message_format.as_deref());

    match cli.command {
        None => {
            let (config, config_diags) = config::load();
            let (mut diagnostics, workspace) = run_all(&config);
            // Config-validation findings join the stream before suppression
            // (so a `# workspace-lint: allow(config)` directive can silence
            // them) and before leveling (so `[lints] config = …` applies).
            diagnostics.extend(config_diags);
            // Suppression filters by directives and appends `stale-expect` /
            // `unknown-lint` findings; leveling runs last so the appended ones
            // are leveled too and `allow`-ed lints are dropped from the final
            // set before the exit-code tally.
            apply_suppression(workspace.as_ref(), &mut diagnostics);
            apply_lint_levels(&config, &mut diagnostics);
            if cli.fix {
                fix::run(&diagnostics);
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
            // For single-check runs we still honor the `[lints]` table if a
            // config file exists, so `workspace-lint check file-size` and the
            // default run agree on severity.
            let config_for_levels = config::try_load();
            let (mut diagnostics, workspace) = run_single_check(rule);
            apply_suppression(workspace.as_ref(), &mut diagnostics);
            if let Some(cfg) = &config_for_levels {
                apply_lint_levels(cfg, &mut diagnostics);
            }
            if cli.fix {
                fix::run(&diagnostics);
            }
            report_and_exit(diagnostics, format);
        }
        Some(Commands::Expand {
            command,
            glob,
            marker,
            auto_stage,
        }) => {
            let ec = CheckRule::into_expand_config(command, glob, marker, auto_stage);
            expand::run(&ec);
        }
    }
}

/// Apply the `[lints]` table to the collected diagnostics: **drop** any whose
/// effective level is `allow`, and rewrite the rest to their effective level
/// (per-lint override → `[lints] default` → built-in `warn`). Diagnostics
/// whose level the lint chose itself (`level_is_explicit`, e.g. an
/// `architecture` rule's `severity`) are left untouched, so a blanket
/// `[lints] <lint> = …` can't silently clobber a deliberate per-rule severity.
fn apply_lint_levels(config: &config::Config, diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.retain_mut(|d| {
        if d.level_is_explicit {
            return true;
        }
        let Some(id) = LintId::from_short(d.lint_short()) else {
            // A diagnostic carrying an unknown lint id shouldn't happen (they
            // all come from `LintId::*.id()`); keep it rather than drop.
            return true;
        };
        match config.lints.effective(id).to_diagnostic_level() {
            None => false, // `allow` → drop before render & exit-code tally
            Some(level) => {
                d.level = level;
                true
            }
        }
    });
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
/// When a [`Workspace`] is available, the scanner reuses its cached
/// `syn::File` ASTs and skips re-parsing every `.rs` file. Without one
/// (e.g. for lint subcommands that don't load a workspace), we fall back
/// to the on-demand parse path.
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

fn run_all(config: &config::Config) -> (Vec<Diagnostic>, Option<Workspace>) {
    if let Some(ref ec) = config.expand {
        expand::run(ec);
    }

    let registry = lints::registry(config);
    let needs_ws = registry.iter().any(|l| l.requirements().needs_workspace);
    let workspace = needs_ws.then(|| load_workspace(config.macros.as_ref()));
    let cx = LintContext {
        workspace: workspace.as_ref(),
    };
    let diagnostics: Vec<Diagnostic> = registry.iter().flat_map(|l| l.check(&cx)).collect();
    (diagnostics, workspace)
}

fn run_single_check(rule: CheckRule) -> (Vec<Diagnostic>, Option<Workspace>) {
    let lint = rule.into_lint();
    let workspace = lint
        .requirements()
        .needs_workspace
        .then(|| load_workspace(None));
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
fn load_workspace(macros: Option<&MacrosConfig>) -> Workspace {
    let mut ws = Workspace::load(".").unwrap_or_else(|e| {
        eprintln!("failed to load workspace for resolver-backed lints: {e}");
        std::process::exit(1);
    });
    for w in ws.warnings() {
        eprintln!("workspace-lint: {w}");
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
    let deny_count = render(format, &diagnostics, out).unwrap_or_else(|e| {
        let _ = io::stderr().is_terminal(); // ignore
        eprintln!("error: failed to write diagnostics: {e}");
        std::process::exit(2);
    });
    // Only `Deny`-level diagnostics flip exit. Configure escalation via the
    // `[lints]` table; without it, every diagnostic stays advisory.
    if deny_count > 0 {
        std::process::exit(1);
    }
}
