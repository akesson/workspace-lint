//! The completeness guard (SPIKE §11 caching gotcha, WS5.1).
//!
//! `WL_IR_OUT` is not in cargo's fingerprint, so a crate that reads "fresh" is
//! not recompiled, its lint pass never runs, and no fragment is (re)written —
//! while `dylint::run` still returns Ok. A fresh crate's *existing* fragment
//! is still valid (its inputs are unchanged), so the guard is a pure
//! existence check against the fragment set a complete run must produce.
//! Ported from the spike embed (`spike/embed/src/main.rs`). The re-lint
//! force lever — invalidating exactly the workspace members' lint units,
//! registry deps staying fresh — lives in the `relink` module: the dylib
//! reaches dylint through an mtime-keyed path, so a mtime bump changes the
//! `DYLINT_LIBS` value every member unit env-dep-tracks.

use std::collections::BTreeSet;
use std::path::Path;

use super::{EngineConfig, EngineError};

/// The linted targets of the selected packages, from cargo metadata — the
/// config-independent half of the expected-fragment computation.
/// [`TargetSet::expected_fragments`] keys it per config.
pub(super) struct TargetSet {
    /// lib / bin / proc-macro targets: linted in every config.
    compile_units: BTreeSet<String>,
    /// The subset of `compile_units` that `--tests` builds as cfg(test)
    /// harnesses — targets with the manifest `test` flag on (the default).
    /// A `[lib] test = false` lib compiles under `--tests` only in plain
    /// mode (as a dependency), which is cargo-fresh from the default config —
    /// expecting its `+test` fragment would force a futile re-lint and then
    /// hard-error every run.
    harnessed: BTreeSet<String>,
    /// Integration-test targets: compiled (and linted) only under `--tests`.
    /// Also gated on the `test` flag — `test = false` opts a target out of
    /// `--tests` entirely.
    test_targets: BTreeSet<String>,
    /// `<pkg>@build` stems for members with a build script — keyed by
    /// *package* name (every build script's target is `build-script-build`).
    /// Deliberately NOT part of [`TargetSet::expected_fragments`]: a build
    /// unit compiles once per shared cargo target dir, so its fragment lands
    /// only in whichever config's run first compiles it — enforcement is
    /// satisfied by presence in ANY of the current run's config dirs
    /// ([`missing_build_fragments`]), while every config's prune keep-set
    /// includes these names so no dir's copy is swept.
    build_units: BTreeSet<String>,
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
        // `--benches` stays unmodeled: a bench fragment's `+test` suffix
        // depends on the target's `harness` flag, which cargo_metadata 0.23
        // no longer exposes — the guard skips (with a warning) rather than
        // guess wrong and force spurious re-lints.
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
            harnessed: BTreeSet::new(),
            test_targets: BTreeSet::new(),
            build_units: BTreeSet::new(),
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
                        "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" => {
                            set.compile_units.insert(name.clone());
                            if t.test {
                                set.harnessed.insert(name.clone());
                            }
                        }
                        // Bins carry the extractor's `@bin` infix: a package's
                        // bin may share the lib's crate name (`src/lib.rs` +
                        // `src/main.rs`), and un-infixed the two units would
                        // collide on one fragment filename.
                        "bin" => {
                            set.compile_units.insert(format!("{name}@bin"));
                            if t.test {
                                set.harnessed.insert(format!("{name}@bin"));
                            }
                        }
                        "test" if t.test => {
                            set.test_targets.insert(name.clone());
                        }
                        // The extractor keys build fragments on the PACKAGE
                        // (the target name is always `build-script-build`).
                        "custom-build" => {
                            set.build_units
                                .insert(format!("{}@build", p.name.replace('-', "_")));
                        }
                        _ => {} // example/bench — never linted here
                    }
                }
            }
        }
        Ok(Some(set))
    }

    /// The fragment filenames one config must produce, exactly as the
    /// extractor's `write_fragment` keys them: `<crate>[@bin].json` for a
    /// default `cargo check`; `<crate>[@bin]+test.json` for everything
    /// compiled under `--tests` (unit-test harnesses of lib/bin/proc-macro AND
    /// integration tests, all with `sess.opts.test` — a bin's harness keeps
    /// the `@bin` infix, since the sibling lib harness shares its crate name).
    /// Only `harnessed` targets flip to `+test`: `--tests` selects by the
    /// manifest `test` flag, so a `test = false` target never compiles in
    /// test mode (its cross-crate uses are still credited — the assembler's
    /// foreign-reach channel covers configs that lack the defining crate).
    pub(super) fn expected_fragments(&self, cargo_args: &[String]) -> BTreeSet<String> {
        let tests = cargo_args.iter().any(|a| a == "--tests");
        let mut expected = BTreeSet::new();
        if tests {
            for name in self.harnessed.iter().chain(&self.test_targets) {
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

impl TargetSet {
    /// Build-fragment filenames (`<pkg>@build.json`) — the cross-config half
    /// of the expected set (see the `build_units` field doc).
    pub(super) fn build_fragments(&self) -> BTreeSet<String> {
        self.build_units
            .iter()
            .map(|stem| format!("{stem}.json"))
            .collect()
    }
}

/// Build-fragment filenames present in NONE of the current run's config dirs.
/// A dir that doesn't exist yet (a later config on a cold run) simply
/// contributes nothing. Deliberately scans only the dirs the assembler will
/// load — a copy in a stale, no-longer-configured sibling dir would pass an
/// existence check but never reach assembly.
pub(super) fn missing_build_fragments(
    dirs: &[std::path::PathBuf],
    names: &BTreeSet<String>,
) -> Vec<String> {
    names
        .iter()
        .filter(|name| !dirs.iter().any(|d| d.join(name).exists()))
        .cloned()
        .collect()
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
            "workspace_lint@bin.json",
            "workspace_lint_marker.json",
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
        assert!(tests.contains("workspace_lint@bin+test.json"));
        assert!(tests.contains("dogfood+test.json"), "{tests:?}");
        assert!(tests.iter().all(|f| f.ends_with("+test.json")));
        assert!(tests.len() > default.len());

        // Build fragments are the cross-config half: package-keyed, config-
        // independent, and NEVER in the per-config expected set (a build unit
        // compiles once per shared target dir). This repo has exactly one
        // build script: crates/wl-engine/build.rs (the extractor embedder).
        let build = set.build_fragments();
        assert_eq!(
            build.iter().map(String::as_str).collect::<Vec<_>>(),
            ["wl_engine@build.json"]
        );
        assert!(!default.contains("wl_engine@build.json"));
        assert!(!tests.contains("wl_engine@build.json"));
    }

    /// Build-fragment enforcement is satisfied by ANY current config dir, and
    /// tolerates dirs that don't exist yet (a later config on a cold run).
    #[test]
    fn build_fragments_satisfied_across_config_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let default_dir = tmp.path().join("default");
        let tests_dir = tmp.path().join("tests"); // never created
        std::fs::create_dir_all(&default_dir).unwrap();
        let names: BTreeSet<String> = ["wl_engine@build.json".to_string()].into();

        let dirs = vec![default_dir.clone(), tests_dir];
        assert_eq!(
            missing_build_fragments(&dirs, &names),
            vec!["wl_engine@build.json"]
        );
        std::fs::write(default_dir.join("wl_engine@build.json"), b"{}").unwrap();
        assert!(missing_build_fragments(&dirs, &names).is_empty());
    }

    /// Scoped runs scope the build set with the same package filter.
    #[test]
    fn package_filter_scopes_build_fragments() {
        let cfg = engine_cfg(vec![CfgSelector::default_cfg()]);
        let with_build = TargetSet::discover(&repo_root(), &["wl-engine".to_string()], &cfg)
            .unwrap()
            .unwrap();
        assert_eq!(with_build.build_fragments().len(), 1);
        let without = TargetSet::discover(&repo_root(), &["wl-ir".to_string()], &cfg)
            .unwrap()
            .unwrap();
        assert!(without.build_fragments().is_empty());
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

    /// A `test = false` target is expected under the default config but never
    /// under `--tests` (cargo won't build its harness — expecting it would
    /// force a futile re-lint, then hard-error).
    #[test]
    fn unharnessed_targets_not_expected_under_tests() {
        let set = TargetSet {
            compile_units: ["alpha".to_string(), "beta".to_string()].into(),
            harnessed: ["beta".to_string()].into(), // alpha: [lib] test = false
            test_targets: BTreeSet::new(),
            build_units: BTreeSet::new(),
        };
        let default = set.expected_fragments(&[]);
        assert!(default.contains("alpha.json") && default.contains("beta.json"));
        let tests = set.expected_fragments(&["--tests".to_string()]);
        assert_eq!(
            tests.iter().map(String::as_str).collect::<Vec<_>>(),
            ["beta+test.json"],
            "only the harnessed unit flips to +test"
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
