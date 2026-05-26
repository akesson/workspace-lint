mod centralized_deps;
mod cli;
mod cli_crate_version;
mod config;
mod crate_size;
#[allow(dead_code)]
// Diagnostic types are wired in across multiple steps; suppress until checks
// migrate over.
mod diagnostic;
mod expand;
mod file_size;
mod freshness;
mod unused_deps;
mod unused_pub;
mod workspace;

use clap::Parser;
use cli::{CheckRule, Cli, Commands};

pub(crate) struct Issue {
    pub title: String,
    pub details: Vec<String>,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        None => run_all_from_config(),
        Some(Commands::Done) => {
            let config = config::load();
            if let Some(ref fc) = config.freshness {
                freshness::mark_done(fc);
            }
        }
        Some(Commands::Check { rule }) => {
            let issues = run_single_check(rule);
            report_and_exit(issues);
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

fn run_all_from_config() {
    let config = config::load();

    if let Some(ref ec) = config.expand {
        expand::run(ec);
    }

    let mut issues = Vec::new();

    if let Some(ref cv) = config.cli_crate_version {
        issues.extend(cli_crate_version::check(cv));
    }
    if config.checks.centralized_deps {
        issues.extend(centralized_deps::check());
    }
    if let Some(ref fc) = config.freshness {
        issues.extend(freshness::check(fc));
    }
    if let Some(ref fc) = config.file_size {
        issues.extend(file_size::check(fc));
    }
    if let Some(ref fc) = config.crate_size {
        issues.extend(crate_size::check(fc));
    }
    if let Some(ref uc) = config.unused_deps {
        issues.extend(unused_deps::check(uc));
    }
    if let Some(ref up) = config.unused_pub {
        issues.extend(unused_pub::check(up));
    }

    report_and_exit(issues);
}

fn run_single_check(rule: CheckRule) -> Vec<Issue> {
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

fn report_and_exit(issues: Vec<Issue>) {
    if issues.is_empty() {
        eprintln!("Workspace lint: all passed");
    } else {
        eprintln!(
            "Workspace lint: {} issue{}",
            issues.len(),
            if issues.len() == 1 { "" } else { "s" }
        );
        eprintln!();
        for issue in &issues {
            eprintln!("- [ ] {}", issue.title);
            for detail in &issue.details {
                eprintln!("      {detail}");
            }
            eprintln!();
        }
        if issues.len() > 1 {
            eprintln!("Tip: fix each item in a subagent");
        }
        std::process::exit(1);
    }
}
