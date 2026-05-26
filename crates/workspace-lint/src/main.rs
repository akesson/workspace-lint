mod centralized_deps;
mod cli;
mod cli_crate_version;
mod config;
mod crate_size;
#[allow(dead_code)]
// Diagnostic types include a few helpers (LintId, SilenceAnchor::{file,line},
// some Applicability variants, clean_path/clean_pathbuf) that aren't yet
// referenced; upcoming steps (suppression map, --fix, snapshot tests) will
// use them. Suppress until then to keep `cargo clippy -D warnings` green.
mod diagnostic;
mod expand;
mod file_size;
mod freshness;
mod unused_deps;
mod unused_pub;
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
            let diagnostics = run_all_from_config();
            report_and_exit(diagnostics, format);
        }
        Some(Commands::Done) => {
            let config = config::load();
            if let Some(ref fc) = config.freshness {
                freshness::mark_done(fc);
            }
        }
        Some(Commands::Check { rule }) => {
            let diagnostics = run_single_check(rule);
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

    diagnostics
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
