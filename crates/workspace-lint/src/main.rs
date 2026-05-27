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
mod workspace;

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
            let mut diagnostics = run_all_from_config();
            apply_suppression(&mut diagnostics);
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
            let mut diagnostics = run_single_check(rule);
            apply_suppression(&mut diagnostics);
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
fn apply_suppression(diagnostics: &mut Vec<Diagnostic>) {
    let directives_list = directives::scan(std::path::Path::new("."));
    let mut map = suppress::SuppressionMap::from_directives(directives_list);
    suppress::apply(&mut map, diagnostics);
    let mut stale = map.stale_expects();
    suppress::apply(&mut map, &mut stale);
    diagnostics.extend(stale);
}

fn run_all_from_config() -> Vec<Diagnostic> {
    let config = config::load();

    if let Some(ref ec) = config.expand {
        expand::run(ec);
    }

    let mut diagnostics: Vec<Diagnostic> = Vec::new();

    if let Some(ref cv) = config.cli_crate_version {
        diagnostics.extend(cli_crate_version::check(cv));
    }
    if config.checks.centralized_deps {
        diagnostics.extend(centralized_deps::check());
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
    if let Some(ref uc) = config.unused_deps {
        diagnostics.extend(unused_deps::check(uc));
    }
    if let Some(ref up) = config.unused_pub {
        diagnostics.extend(unused_pub::check(up));
    }
    // syn-workspace-backed checks share a single resolved Workspace so we
    // pay the cargo_metadata + per-file syn parse once across all of them.
    let architecture_needed = config
        .architecture
        .as_ref()
        .is_some_and(|ac| !ac.rules.is_empty());
    let module_tree_needed = config.checks.module_tree;
    let feature_drift_needed = config.checks.feature_drift;
    let visibility_needed = config.checks.visibility;
    if (architecture_needed || module_tree_needed || feature_drift_needed || visibility_needed)
        && let Ok(mut ws) = syn_workspace::Workspace::load(".")
    {
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
    }

    diagnostics
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

fn run_single_check(rule: CheckRule) -> Vec<Diagnostic> {
    match rule {
        CheckRule::CentralizedDeps => centralized_deps::check(),
        CheckRule::FileSize {
            glob,
            max_code_lines,
        } => {
            let config = CheckRule::into_file_size_config(glob, max_code_lines);
            file_size::check(&config)
        }
        CheckRule::CrateSize {
            glob,
            max_code_lines,
            include,
        } => {
            let config = CheckRule::into_crate_size_config(glob, max_code_lines, include);
            crate_size::check(&config)
        }
        CheckRule::Freshness { glob, depends_on } => {
            let config = CheckRule::into_freshness_config(glob, depends_on);
            freshness::check(&config)
        }
        CheckRule::CliCrateVersion {
            command,
            pattern,
            crate_name,
        } => {
            let config = CheckRule::into_cli_crate_version_config(command, pattern, crate_name);
            cli_crate_version::check(&config)
        }
        CheckRule::UnusedDeps { ignore } => {
            let config = CheckRule::into_unused_deps_config(ignore);
            unused_deps::check(&config)
        }
        CheckRule::UnusedPub {
            on_ci_only,
            scip_index,
            exclude_crates,
            allowlist,
            kinds,
            exclude_paths,
            cargo_features,
        } => {
            let config = CheckRule::into_unused_pub_config(
                on_ci_only,
                scip_index,
                exclude_crates,
                allowlist,
                kinds,
                exclude_paths,
                cargo_features,
            );
            unused_pub::check(&config)
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
    if deny_count > 0 || (format == Format::Human && !diagnostics.is_empty()) {
        // For now: any diagnostic flips exit. Once `[lints]` levels are wired
        // in step 16, only `Deny`-level findings will trip the exit.
        std::process::exit(1);
    }
}
