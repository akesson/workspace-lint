//! The completeness guard (SPIKE §11 caching gotcha, WS5.1).
//!
//! `WL_IR_OUT` is not in cargo's fingerprint, so a crate that reads "fresh" is
//! not recompiled, its lint pass never runs, and no fragment is (re)written —
//! while `dylint::run` still returns Ok. A fresh crate's *existing* fragment
//! is still valid (its inputs are unchanged), so the guard is a pure
//! existence check against the fragment set a complete run must produce.
//! Ported from the spike embed (`spike/embed/src/main.rs`), where the
//! mechanism was verified: bumping the dylib mtime invalidates exactly the
//! workspace members' lint units (dylint dep-info-tracks the dylib per
//! primary-package unit) — registry deps stay fresh.

use std::collections::BTreeSet;
use std::path::Path;

use super::{EngineConfig, EngineError};

/// The linted targets of the selected packages, from cargo metadata — the
/// config-independent half of the expected-fragment computation.
/// [`TargetSet::expected_fragments`] keys it per config.
pub(super) struct TargetSet {
    /// lib / bin / proc-macro targets: linted in every config.
    compile_units: BTreeSet<String>,
    /// Integration-test targets: compiled (and linted) only under `--tests`.
    test_targets: BTreeSet<String>,
}

impl TargetSet {
    /// Compute the target set for a workspace + package selection. Returns
    /// `Ok(None)` (guard skipped, with a warning) when any config carries a
    /// target-selection flag we don't model, so the guard never fires
    /// spuriously.
    pub(super) fn discover(
        workspace_root: &Path,
        packages: &[String],
        cfg: &EngineConfig,
    ) -> Result<Option<Self>, EngineError> {
        // Flags that change which *targets* compile, beyond the `--tests` we
        // model. (Feature flags — `--features`, `--all-features`,
        // `--no-default-features` — change cfg/content, not the target set.)
        const UNMODELED: &[&str] = &[
            "--lib",
            "--bins",
            "--bin",
            "--examples",
            "--example",
            "--benches",
            "--bench",
            "--test",
            "--all-targets",
            "--doc",
            "-p",
            "--package",
            "--workspace",
            "--exclude",
        ];
        for selector in &cfg.configs {
            if let Some(flag) = selector
                .cargo_args
                .iter()
                .find(|a| UNMODELED.contains(&a.as_str()))
            {
                eprintln!(
                    "wl-engine: completeness guard skipped — unmodeled target-selection flag \
                     `{flag}` in config `{}`",
                    selector.id
                );
                return Ok(None);
            }
        }

        let md = cargo_metadata::MetadataCommand::new()
            .manifest_path(workspace_root.join("Cargo.toml"))
            .no_deps()
            .exec()
            .map_err(|source| EngineError::Metadata {
                dir: workspace_root.to_path_buf(),
                source: Box::new(source),
            })?;
        let member_ids: BTreeSet<String> = md
            .workspace_members
            .iter()
            .map(|id| id.to_string())
            .collect();
        let want_pkg = |name: &str| packages.is_empty() || packages.iter().any(|p| p == name);

        let mut set = Self {
            compile_units: BTreeSet::new(),
            test_targets: BTreeSet::new(),
        };
        for p in &md.packages {
            if !member_ids.contains(&p.id.to_string()) || !want_pkg(p.name.as_str()) {
                continue;
            }
            for t in &p.targets {
                let name = t.name.replace('-', "_");
                // Compare kinds via Display so we don't couple to
                // cargo_metadata's enum representation (as the spike did).
                for k in &t.kind {
                    match k.to_string().as_str() {
                        "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro"
                        | "bin" => {
                            set.compile_units.insert(name.clone());
                        }
                        "test" => {
                            set.test_targets.insert(name.clone());
                        }
                        _ => {} // example/bench/custom-build — never linted here
                    }
                }
            }
        }
        Ok(Some(set))
    }

    /// The fragment filenames one config must produce, exactly as the
    /// extractor's `write_fragment` keys them: `<crate>.json` for a default
    /// `cargo check`; `<crate>+test.json` for everything compiled under
    /// `--tests` (unit-test harnesses of lib/bin/proc-macro AND integration
    /// tests, all with `sess.opts.test`).
    pub(super) fn expected_fragments(&self, cargo_args: &[String]) -> BTreeSet<String> {
        let tests = cargo_args.iter().any(|a| a == "--tests");
        let mut expected = BTreeSet::new();
        if tests {
            for name in self.compile_units.iter().chain(&self.test_targets) {
                expected.insert(format!("{name}+test.json"));
            }
        } else {
            for name in &self.compile_units {
                expected.insert(format!("{name}.json"));
            }
        }
        expected
    }
}

/// Expected fragment filenames not present in `ir_dir`.
pub(super) fn missing_fragments(ir_dir: &Path, expected: &BTreeSet<String>) -> Vec<String> {
    expected
        .iter()
        .filter(|name| !ir_dir.join(name).exists())
        .cloned()
        .collect()
}

/// Force the next `dylint::run` to re-lint every workspace member by bumping
/// the lint dylib's mtime (dylint fingerprints the dylib into each
/// primary-package unit's dep-info).
pub(super) fn force_relint(dylib: &Path) -> std::io::Result<()> {
    let f = std::fs::OpenOptions::new().append(true).open(dylib)?;
    f.set_modified(std::time::SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrate::CfgSelector;

    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
    }

    fn engine_cfg(configs: Vec<CfgSelector>) -> EngineConfig {
        EngineConfig {
            workspace_root: repo_root(),
            configs,
            packages: Vec::new(),
            ir_root: std::path::PathBuf::from("unused"),
        }
    }

    /// Against this very repository (offline: `--no-deps` needs no lockfile or
    /// network): the default config expects one fragment per member crate.
    #[test]
    fn expected_set_matches_this_workspace() {
        let cfg = engine_cfg(vec![CfgSelector::default_cfg(), CfgSelector::tests()]);
        let set = TargetSet::discover(&repo_root(), &[], &cfg)
            .unwrap()
            .unwrap();

        let default = set.expected_fragments(&[]);
        for frag in [
            "syn_workspace.json",
            "workspace_lint.json",
            "wl_ir.json",
            "wl_engine.json",
        ] {
            assert!(default.contains(frag), "{frag} missing from {default:?}");
        }
        assert!(
            default.iter().all(|f| !f.contains("+test")),
            "default config must not expect +test fragments"
        );

        // --tests: every compile unit flips to +test AND integration-test
        // targets appear (this repo has several under crates/workspace-lint).
        let tests = set.expected_fragments(&["--tests".to_string()]);
        assert!(tests.contains("workspace_lint+test.json"));
        assert!(tests.contains("dogfood+test.json"), "{tests:?}");
        assert!(tests.iter().all(|f| f.ends_with("+test.json")));
        assert!(tests.len() > default.len());
    }

    /// A package filter narrows the expected set to that crate's targets.
    #[test]
    fn package_filter_narrows_expectations() {
        let cfg = engine_cfg(vec![CfgSelector::default_cfg()]);
        let set = TargetSet::discover(&repo_root(), &["wl-ir".to_string()], &cfg)
            .unwrap()
            .unwrap();
        assert_eq!(set.expected_fragments(&[]).len(), 1);
        assert!(set.expected_fragments(&[]).contains("wl_ir.json"));
    }

    /// An unmodeled target-selection flag anywhere in the matrix skips the
    /// guard entirely rather than risking a spurious failure.
    #[test]
    fn unmodeled_flag_skips_guard() {
        let cfg = engine_cfg(vec![
            CfgSelector::default_cfg(),
            CfgSelector {
                id: "lib-only".into(),
                cargo_args: vec!["--lib".into()],
            },
        ]);
        assert!(
            TargetSet::discover(&repo_root(), &[], &cfg)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn missing_fragments_is_a_pure_existence_check() {
        let tmp = tempfile::tempdir().unwrap();
        let expected: BTreeSet<String> = ["a.json".to_string(), "b.json".to_string()].into();
        std::fs::write(tmp.path().join("a.json"), b"{}").unwrap();
        assert_eq!(missing_fragments(tmp.path(), &expected), vec!["b.json"]);
    }
}
