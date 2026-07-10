//! The shared source-measurement sweep: ONE tokei walk over the workspace,
//! serving both size lints (each used to run its own — `file-size` a
//! cwd-rooted whole-tree walk, `crate-size` one walk per member dir — so a
//! run with both scanned the tree twice, from different roots).
//!
//! The sweep only *measures*; what counts toward a budget stays each lint's
//! call: `file-size` counts a non-Rust file's host `code` **plus** its
//! embedded-language children (a `rust` fence in a Markdown file bills the
//! host), while `crate-size` counts host `code` only, and both route matched
//! `.rs` files through [`shipped_source`]'s test-mass exclusion.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tokei::{Config as TokeiConfig, Languages};

use crate::metadata::FastModel;
use crate::shipped_source;

/// One measured file from the sweep.
pub struct MeasuredFile {
    /// Absolute path (tokei is run on absolute roots, so the walk never
    /// depends on the process cwd).
    pub abs: PathBuf,
    /// Workspace-root-relative form — the glob-matching and display surface.
    /// `None` for a file swept under an out-of-root member (it has no
    /// root-relative spelling).
    pub rel: Option<PathBuf>,
    /// tokei's host-language code lines for the file itself.
    pub code: usize,
    /// The sum of tokei's embedded-language `children` reports (e.g. Rust
    /// inside a doc code fence), billed to this host file.
    pub embedded: usize,
}

impl MeasuredFile {
    pub fn is_rust(&self) -> bool {
        self.abs.extension().and_then(|e| e.to_str()) == Some("rs")
    }
}

/// The workspace's measured files, plus the member geometry needed for the
/// dev-target test. Built once per run via `FastModel::source_measure`.
pub struct SourceMeasure {
    /// Sorted by absolute path for deterministic iteration.
    files: Vec<MeasuredFile>,
    /// Member manifest dirs, longest-first so a nested member wins the
    /// owner lookup in [`Self::in_dev_target`].
    member_dirs: Vec<PathBuf>,
}

impl SourceMeasure {
    pub(crate) fn scan(fast: &FastModel) -> Self {
        // Sweep the workspace root plus any member living outside it (cargo
        // allows out-of-root members; a root-only walk would miss them).
        let mut roots: Vec<String> = vec![fast.root().display().to_string()];
        for krate in fast.members() {
            if !krate.manifest_dir.starts_with(fast.root()) {
                roots.push(krate.manifest_dir.display().to_string());
            }
        }
        let root_refs: Vec<&str> = roots.iter().map(String::as_str).collect();
        let mut languages = Languages::new();
        crate::timing::phase("tokei_sweep[source_measure]", || {
            languages.get_statistics(&root_refs, &[], &TokeiConfig::default());
        });

        // Fold host reports and embedded-children reports (which carry the
        // host file's name) into one entry per file.
        let mut by_file: HashMap<PathBuf, (usize, usize)> = HashMap::new();
        for language in languages.values() {
            for report in &language.reports {
                by_file.entry(report.name.clone()).or_default().0 += report.stats.code;
            }
            for child_reports in language.children.values() {
                for report in child_reports {
                    by_file.entry(report.name.clone()).or_default().1 += report.stats.code;
                }
            }
        }
        let mut files: Vec<MeasuredFile> = by_file
            .into_iter()
            .map(|(abs, (code, embedded))| {
                let rel = fast.crate_relative_path(&abs);
                MeasuredFile {
                    rel: rel.is_relative().then_some(rel),
                    abs,
                    code,
                    embedded,
                }
            })
            .collect();
        files.sort_by(|a, b| a.abs.cmp(&b.abs));

        let mut member_dirs: Vec<PathBuf> = fast
            .members()
            .iter()
            .map(|k| k.manifest_dir.clone())
            .collect();
        member_dirs.sort_by_key(|d| std::cmp::Reverse(d.as_os_str().len()));

        SourceMeasure { files, member_dirs }
    }

    /// Every measured file, sorted by absolute path.
    pub fn files(&self) -> &[MeasuredFile] {
        &self.files
    }

    /// Is `file` under a cargo dev-target dir (`tests/`, `benches/`,
    /// `examples/`) of its owning crate? Member-aware first (the innermost
    /// member whose dir contains the file), falling back to the
    /// nearest-`Cargo.toml` walk for files outside every member (e.g. an
    /// excluded package's sources matched by a broad glob).
    pub fn in_dev_target(&self, file: &Path) -> bool {
        match self.member_dirs.iter().find(|d| file.starts_with(d)) {
            Some(dir) => shipped_source::in_dev_target_dir(dir, file),
            None => shipped_source::in_dev_target_dir_rootless(file),
        }
    }

    /// Shipped (test-mass-excluded) Rust code lines for the requested files —
    /// see `shipped_source::shipped_lines_by_file`. Out-of-line test-mod
    /// targets are resolved within the requested set; dev-target exclusion is
    /// the caller's concern ([`Self::in_dev_target`]).
    pub fn shipped_rust_lines(&self, files: &[PathBuf]) -> HashMap<PathBuf, usize> {
        shipped_source::shipped_lines_by_file(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sweep THIS workspace (the same pattern `metadata.rs`'s
    /// `load_this_workspace` uses) and sanity-check the surfaces.
    #[test]
    fn sweeps_this_workspace() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let fast = FastModel::load(&root).expect("load this workspace");
        let measure = fast.source_measure();

        let this_file = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/source_measure.rs");
        let entry = measure
            .files()
            .iter()
            .find(|f| f.abs.ends_with("wl-fast/src/source_measure.rs"))
            .expect("this file is swept");
        assert!(entry.is_rust());
        assert!(entry.code > 0);
        assert_eq!(
            entry.rel.as_deref(),
            Some(Path::new("crates/wl-fast/src/source_measure.rs"))
        );
        assert!(!measure.in_dev_target(&this_file));

        // A member's tests/ tree is a dev target.
        let dev = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/x.rs");
        assert!(measure.in_dev_target(&dev));
    }
}
