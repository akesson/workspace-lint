use globset::GlobSetBuilder;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::{GlobPattern, Globs};
use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_file;
use crate::lints::{Lint, LintContext, LintId};

pub mod config;
#[cfg(test)]
mod tests;

pub(crate) use config::{FreshnessConfig, FreshnessRule};

pub(crate) struct Freshness {
    config: FreshnessConfig,
}

impl Freshness {
    pub fn new(config: FreshnessConfig) -> Self {
        Self { config }
    }

    pub fn from_cli(glob: String, depends_on: String) -> Self {
        Self::new(FreshnessConfig {
            rules: vec![FreshnessRule {
                glob: GlobPattern::from_cli(&glob),
                depends_on: Globs(vec![GlobPattern::from_cli(&depends_on)]),
            }],
        })
    }
}

impl Lint for Freshness {
    fn id(&self) -> LintId {
        LintId::Freshness
    }

    fn check(&self, _cx: &LintContext<'_>) -> Vec<Diagnostic> {
        check(&self.config)
    }
}

pub(crate) fn check(config: &FreshnessConfig) -> Vec<Diagnostic> {
    if std::env::var("CI").is_ok() {
        return Vec::new();
    }
    check_with_root(config, Path::new("."))
}

fn check_with_root(config: &FreshnessConfig, root: &Path) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let lint_id = LintId::Freshness.id();

    for rule in &config.rules {
        let tracked_files = find_files_matching(root, &rule.glob);

        for file in &tracked_files {
            let file_mtime = match mtime(file) {
                Some(t) => t,
                None => continue,
            };

            let parent = file.parent().unwrap_or(Path::new("."));
            let parent_dir = if parent == Path::new("") {
                Path::new(".")
            } else {
                parent
            };

            let dep_files = find_deps_in_dir(parent_dir, &rule.depends_on);
            let newest_dep = dep_files.iter().filter_map(|p| mtime(p)).max();

            if let Some(newest) = newest_dep
                && newest > file_mtime
            {
                let rel = file.strip_prefix(root).unwrap_or(file).to_path_buf();
                diagnostics.push(
                    at_file(
                        lint_id,
                        format!(
                            "`{}` is older than source files it depends on",
                            rel.display()
                        ),
                        rel,
                    )
                    .help(format!(
                        "files matching `{}` in the subtree are newer",
                        rule.depends_on
                            .iter()
                            .map(|g| g.as_str())
                            .collect::<Vec<_>>()
                            .join("`, `")
                    ))
                    .help("run `workspace-lint done` once the tracked file is up to date")
                    .build(),
                );
            }
        }
    }

    diagnostics
}

pub(crate) fn mark_done(config: &FreshnessConfig) {
    mark_done_with_root(config, Path::new("."));
}

fn mark_done_with_root(config: &FreshnessConfig, root: &Path) {
    let now = SystemTime::now();
    let times = std::fs::FileTimes::new().set_modified(now);

    for rule in &config.rules {
        let files = find_files_matching(root, &rule.glob);
        for file in &files {
            let f = match fs_err::File::options().write(true).open(file) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("failed to open {}: {e}", file.display());
                    continue;
                }
            };
            if let Err(e) = f.set_times(times) {
                eprintln!("failed to touch {}: {e}", file.display());
            }
        }
    }
}

fn find_files_matching(root: &Path, glob: &GlobPattern) -> Vec<PathBuf> {
    let matcher = glob.compiled().compile_matcher();

    let mut results = Vec::new();
    for entry in ignore::WalkBuilder::new(root).build().flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path().strip_prefix(root).unwrap_or(entry.path());
        if matcher.is_match(path) {
            results.push(entry.into_path());
        }
    }
    results
}

fn find_deps_in_dir(dir: &Path, deps: &Globs) -> Vec<PathBuf> {
    let mut builder = GlobSetBuilder::new();
    for glob in deps.iter() {
        builder.add(glob.compiled().clone());
    }
    let set = builder
        .build()
        .expect("depends-on globs pre-validated at parse time");

    let mut results = Vec::new();
    for entry in ignore::WalkBuilder::new(dir).build().flatten() {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let rel = match entry.path().strip_prefix(dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        if set.is_match(rel) {
            results.push(entry.into_path());
        }
    }
    results
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs_err::metadata(path).ok()?.modified().ok()
}
