mod architecture;
mod centralized_deps;
mod cli;
mod cli_crate_version;
mod config;
mod crate_size;
#[allow(dead_code)]
// Diagnostic types include a few helpers (LintId, some Applicability
// variants, clean_path/clean_pathbuf) that aren't yet referenced; upcoming
// steps (--fix, snapshot tests) will use them. Suppress until then to keep
// `cargo clippy -D warnings` green.
mod diagnostic;
mod directives;
mod expand;
mod feature_drift;
mod file_size;
mod fix;
mod freshness;
#[allow(dead_code)]
// Constants surface through the integration tests (`tests/lint_coverage.rs`,
// `tests/fix_fixtures.rs`) but aren't referenced from the binary itself.
mod lints;
#[allow(dead_code)]
// Compiled into the binary only because all module-level tests must be
// visible to `cargo test`. The `scenarios()` builder is `pub` but the
// snapshot tests inside `mod tests` are what actually exercise the
// diagnostics — see the file's module docs.
mod messages;
mod module_tree;
mod suppress;
mod unused_deps;
mod unused_pub;
mod visibility;

use clap::Parser;
use std::io::{self, IsTerminal};

use cli::{CheckRule, Cli, Commands};
use diagnostic::Diagnostic;
use diagnostic::render::{Format, render};

fn main() {
    let cli = Cli::parse();
    let format = parse_format(cli.message_format.as_deref());

    match cli.command {
        None => {
            let config = config::load();
            // Schema-2 warning is human-only — JSON / GitHub channels
            // shouldn't get prose mixed into their stderr.
            if format == Format::Human {
                config::maybe_warn_on_old_schema(&config);
            }
            let (mut diagnostics, workspace) = run_all(&config);
            apply_lint_levels(&config, &mut diagnostics);
            apply_suppression(workspace.as_ref(), &mut diagnostics);
            if cli.fix {
                fix::run(&diagnostics);
            }
            report_and_exit(diagnostics, format);
        }
        Some(Commands::Done) => {
            let config = config::load();
            if let Some(ref fc) = config.freshness {
                freshness::mark_done(fc);
            }
        }
        Some(Commands::Check { rule }) => {
            // For single-check runs we still honor the `[lints]` table if a
            // config file exists, so `workspace-lint check file-size` and the
            // default run agree on severity.
            let config_for_levels = config::try_load();
            let (mut diagnostics, workspace) = run_single_check(rule);
            if let Some(cfg) = &config_for_levels {
                apply_lint_levels(cfg, &mut diagnostics);
            }
            apply_suppression(workspace.as_ref(), &mut diagnostics);
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

/// Apply the `[lints]` table overrides: any diagnostic whose lint short
/// name appears in `config.lints` has its `level` rewritten to the
/// configured value. Diagnostics not in the table keep the per-check
/// default (typically [`Level::Warn`]).
fn apply_lint_levels(config: &config::Config, diagnostics: &mut [Diagnostic]) {
    for d in diagnostics {
        if let Some(level) = config.lints.level_for(d.lint.as_ref()) {
            d.level = level;
        }
    }
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
    let mut map = suppress::SuppressionMap::from_directives(directives_list);
    suppress::apply(&mut map, diagnostics);
    let mut stale = map.stale_expects();
    suppress::apply(&mut map, &mut stale);
    diagnostics.extend(stale);
}

fn run_all(config: &config::Config) -> (Vec<Diagnostic>, Option<syn_workspace::Workspace>) {
    if let Some(ref ec) = config.expand {
        expand::run(ec);
    }

    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    if let Some(ref cv) = config.cli_crate_version {
        diagnostics.extend(cli_crate_version::check(cv));
    }
    if let Some(ref fc) = config.freshness {
        diagnostics.extend(freshness::check(fc));
    }
    if let Some(ref fc) = config.file_size {
        diagnostics.extend(file_size::check(fc));
    }
    if let Some(ref fc) = config.crate_size {
        diagnostics.extend(crate_size::check(fc));
    }
    // syn-workspace-backed checks share a single resolved Workspace so we
    // pay the cargo_metadata + per-file syn parse once across all of them.
    let architecture_needed = config
        .architecture
        .as_ref()
        .is_some_and(|ac| !ac.rules.is_empty());
    let centralized_deps_needed = config.checks.centralized_deps;
    let module_tree_needed = config.checks.module_tree;
    let feature_drift_needed = config.checks.feature_drift;
    let visibility_needed = config.checks.visibility;
    let unused_deps_needed = config.unused_deps.is_some();
    let unused_pub_needed = config.unused_pub.is_some();
    let mut workspace = None;
    if architecture_needed
        || centralized_deps_needed
        || module_tree_needed
        || feature_drift_needed
        || visibility_needed
        || unused_deps_needed
        || unused_pub_needed
    {
        // Loud-fail: if the resolver can't load the workspace, every
        // resolver-backed lint would silently produce zero diagnostics. CI
        // would see green for a broken state. Match the existing
        // `unused_deps`/`unused_pub` convention and bail with a clear error.
        let mut ws = syn_workspace::Workspace::load(".").unwrap_or_else(|e| {
            eprintln!("failed to load workspace for resolver-backed lints: {e}");
            std::process::exit(1);
        });
        // Layer 3: feed external-macro expansion-uses entries from config
        // into the workspace's implicit-refs set so downstream lints see
        // items reachable only through e.g. `#[tokio::main]`.
        if let Some(ref macros) = config.macros {
            let paths = macros
                .external
                .iter()
                .flat_map(|m| m.expansion_uses.iter().map(|p| canonicalize_user_path(p)));
            ws.register_external_macro_uses(paths);
        }
        if centralized_deps_needed {
            diagnostics.extend(centralized_deps::check(&ws));
        }
        if architecture_needed && let Some(ref ac) = config.architecture {
            diagnostics.extend(architecture::check(ac, &ws));
        }
        if module_tree_needed {
            diagnostics.extend(module_tree::check(&ws));
        }
        if feature_drift_needed {
            diagnostics.extend(feature_drift::check(&ws));
        }
        if visibility_needed {
            diagnostics.extend(visibility::check(&ws));
        }
        if let Some(ref uc) = config.unused_deps {
            diagnostics.extend(unused_deps::check(uc, &ws));
        }
        if let Some(ref up) = config.unused_pub {
            diagnostics.extend(unused_pub::check(up, &ws));
        }
        workspace = Some(ws);
    }

    (diagnostics, workspace)
}

/// Convert a user-facing path string (`tokio::runtime::Builder`,
/// `data-models::api::User`) into a [`ResolvedPath`]. Hyphens in the leading
/// segment are normalized to underscores so cargo crate names match the
/// in-code form the resolver stores. Other segments pass through verbatim.
fn canonicalize_user_path(path: &str) -> syn_workspace::ResolvedPath {
    let mut segments: Vec<String> = path
        .split("::")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if let Some(first) = segments.first_mut() {
        *first = first.replace('-', "_");
    }
    syn_workspace::ResolvedPath::new(segments)
}

fn run_single_check(rule: CheckRule) -> (Vec<Diagnostic>, Option<syn_workspace::Workspace>) {
    match rule {
        CheckRule::CentralizedDeps => {
            let ws = syn_workspace::Workspace::load(".").unwrap_or_else(|e| {
                eprintln!("failed to load workspace for centralized-deps: {e}");
                std::process::exit(1);
            });
            let diagnostics = centralized_deps::check(&ws);
            (diagnostics, Some(ws))
        }
        CheckRule::FileSize {
            glob,
            max_code_lines,
        } => {
            let config = CheckRule::into_file_size_config(glob, max_code_lines);
            (file_size::check(&config), None)
        }
        CheckRule::CrateSize {
            glob,
            max_code_lines,
            include,
        } => {
            let config = CheckRule::into_crate_size_config(glob, max_code_lines, include);
            (crate_size::check(&config), None)
        }
        CheckRule::Freshness { glob, depends_on } => {
            let config = CheckRule::into_freshness_config(glob, depends_on);
            (freshness::check(&config), None)
        }
        CheckRule::CliCrateVersion {
            command,
            pattern,
            crate_name,
        } => {
            let config = CheckRule::into_cli_crate_version_config(command, pattern, crate_name);
            (cli_crate_version::check(&config), None)
        }
        CheckRule::UnusedDeps { ignore } => {
            let config = CheckRule::into_unused_deps_config(ignore);
            let ws = syn_workspace::Workspace::load(".").unwrap_or_else(|e| {
                eprintln!("failed to load workspace for unused-deps: {e}");
                std::process::exit(1);
            });
            let diagnostics = unused_deps::check(&config, &ws);
            (diagnostics, Some(ws))
        }
        CheckRule::UnusedPub {
            on_ci_only,
            exclude_crates,
            allowlist,
            kinds,
            exclude_paths,
            suppress_intra_crate,
        } => {
            let config = CheckRule::into_unused_pub_config(
                on_ci_only,
                exclude_crates,
                allowlist,
                kinds,
                exclude_paths,
                suppress_intra_crate,
            );
            let ws = syn_workspace::Workspace::load(".").unwrap_or_else(|e| {
                eprintln!("failed to load workspace for unused-pub: {e}");
                std::process::exit(1);
            });
            let diagnostics = unused_pub::check(&config, &ws);
            (diagnostics, Some(ws))
        }
    }
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
