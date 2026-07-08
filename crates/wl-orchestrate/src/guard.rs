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

use super::Kinds;

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
    /// Compute the target set from an already-read `cargo metadata` and a
    /// package selection. Per-config now: the caller decides whether a
    /// config's kind is modelable at all (`Benches` isn't — a bench
    /// fragment's `+test` suffix depends on the harness flag cargo_metadata
    /// 0.23 no longer exposes); the command parser already rejects every
    /// other unmodelable target-selection flag (`--lib`, `--all-targets`, …)
    /// at the config surface.
    pub(super) fn discover(md: &cargo_metadata::Metadata, packages: &[String]) -> Self {
        let member_ids: BTreeSet<String> = md
            .workspace_members
            .iter()
            .map(|id| id.to_string())
            .collect();
        let want_pkg = |name: &str| packages.is_empty() || packages.iter().any(|p| p == name);
        // A target with `required-features` compiles only when they're all
        // enabled; expecting a fragment cargo never builds would force a
        // futile re-lint, then hard-error. The resolve graph carries each
        // package's enabled feature set. No resolve (`--no-deps` metadata) ⇒
        // exclude such targets — under-expecting is the guard-safe direction.
        let enabled: std::collections::BTreeMap<String, BTreeSet<String>> = md
            .resolve
            .as_ref()
            .map(|r| {
                r.nodes
                    .iter()
                    .map(|n| {
                        (
                            n.id.to_string(),
                            n.features.iter().map(|f| f.to_string()).collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

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
                if !t.required_features.is_empty() {
                    let ok = enabled
                        .get(&p.id.to_string())
                        .is_some_and(|fs| t.required_features.iter().all(|f| fs.contains(f)));
                    if !ok {
                        continue;
                    }
                }
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
        set
    }

    /// The fragment filenames one config must produce, exactly as the
    /// extractor's `write_fragment` keys them: `<crate>[@bin].wlir` for a
    /// default `cargo check`; `<crate>[@bin]+test.wlir` for everything
    /// compiled under `--tests` (unit-test harnesses of lib/bin/proc-macro AND
    /// integration tests, all with `sess.opts.test` — a bin's harness keeps
    /// the `@bin` infix, since the sibling lib harness shares its crate name).
    /// Only `harnessed` targets flip to `+test`: `--tests` selects by the
    /// manifest `test` flag, so a `test = false` target never compiles in
    /// test mode (its cross-crate uses are still credited — the assembler's
    /// foreign-reach channel covers configs that lack the defining crate).
    pub(super) fn expected_fragments(&self, kinds: Kinds) -> BTreeSet<String> {
        let mut expected = BTreeSet::new();
        if kinds == Kinds::Tests {
            for name in self.harnessed.iter().chain(&self.test_targets) {
                expected.insert(format!("{name}+test.wlir"));
            }
        } else {
            for name in &self.compile_units {
                expected.insert(format!("{name}.wlir"));
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
    /// Build-fragment filenames (`<pkg>@build.wlir`) — the cross-config half
    /// of the expected set (see the `build_units` field doc).
    pub(super) fn build_fragments(&self) -> BTreeSet<String> {
        self.build_units
            .iter()
            .map(|stem| format!("{stem}.wlir"))
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

    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .unwrap()
    }

    /// `cargo metadata` for this repo, feeding `discover` (offline: `--no-deps`
    /// needs no lockfile or network, and the guard reads only member facts —
    /// identical under `--no-deps` and `resolve`, except `required-features`
    /// targets, which need the resolve's enabled-feature sets and this repo
    /// has none of).
    fn repo_metadata() -> cargo_metadata::Metadata {
        cargo_metadata::MetadataCommand::new()
            .manifest_path(repo_root().join("Cargo.toml"))
            .no_deps()
            .exec()
            .unwrap()
    }

    /// Against this very repository (offline: `--no-deps` needs no lockfile or
    /// network): the default config expects one fragment per member crate.
    #[test]
    fn expected_set_matches_this_workspace() {
        let set = TargetSet::discover(&repo_metadata(), &[]);

        let default = set.expected_fragments(Kinds::Default);
        for frag in [
            "workspace_lint@bin.wlir",
            "workspace_lint_marker.wlir",
            "wl_ir.wlir",
            "wl_engine.wlir",
            "wl_orchestrate.wlir",
        ] {
            assert!(default.contains(frag), "{frag} missing from {default:?}");
        }
        assert!(
            default.iter().all(|f| !f.contains("+test")),
            "default config must not expect +test fragments"
        );

        // --tests: every compile unit flips to +test AND integration-test
        // targets appear (this repo has several under crates/workspace-lint).
        let tests = set.expected_fragments(Kinds::Tests);
        assert!(tests.contains("workspace_lint@bin+test.wlir"));
        assert!(tests.contains("dogfood+test.wlir"), "{tests:?}");
        assert!(tests.iter().all(|f| f.ends_with("+test.wlir")));
        assert!(tests.len() > default.len());

        // Build fragments are the cross-config half: package-keyed, config-
        // independent, and NEVER in the per-config expected set (a build unit
        // compiles once per shared target dir). This repo has exactly one
        // build script: crates/wl-orchestrate/build.rs (the extractor embedder).
        let build = set.build_fragments();
        assert_eq!(
            build.iter().map(String::as_str).collect::<Vec<_>>(),
            ["wl_orchestrate@build.wlir"]
        );
        assert!(!default.contains("wl_orchestrate@build.wlir"));
        assert!(!tests.contains("wl_orchestrate@build.wlir"));
    }

    /// Build-fragment enforcement is satisfied by ANY current config dir, and
    /// tolerates dirs that don't exist yet (a later config on a cold run).
    #[test]
    fn build_fragments_satisfied_across_config_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let default_dir = tmp.path().join("default");
        let tests_dir = tmp.path().join("tests"); // never created
        std::fs::create_dir_all(&default_dir).unwrap();
        let names: BTreeSet<String> = ["wl_orchestrate@build.wlir".to_string()].into();

        let dirs = vec![default_dir.clone(), tests_dir];
        assert_eq!(
            missing_build_fragments(&dirs, &names),
            vec!["wl_orchestrate@build.wlir"]
        );
        std::fs::write(default_dir.join("wl_orchestrate@build.wlir"), b"{}").unwrap();
        assert!(missing_build_fragments(&dirs, &names).is_empty());
    }

    /// Scoped runs scope the build set with the same package filter.
    #[test]
    fn package_filter_scopes_build_fragments() {
        let with_build = TargetSet::discover(&repo_metadata(), &["wl-orchestrate".to_string()]);
        assert_eq!(with_build.build_fragments().len(), 1);
        let without = TargetSet::discover(&repo_metadata(), &["wl-ir".to_string()]);
        assert!(without.build_fragments().is_empty());
    }

    /// A package filter narrows the expected set to that crate's targets.
    #[test]
    fn package_filter_narrows_expectations() {
        let set = TargetSet::discover(&repo_metadata(), &["wl-ir".to_string()]);
        assert_eq!(set.expected_fragments(Kinds::Default).len(), 1);
        assert!(
            set.expected_fragments(Kinds::Default)
                .contains("wl_ir.wlir")
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
        let default = set.expected_fragments(Kinds::Default);
        assert!(default.contains("alpha.wlir") && default.contains("beta.wlir"));
        let tests = set.expected_fragments(Kinds::Tests);
        assert_eq!(
            tests.iter().map(String::as_str).collect::<Vec<_>>(),
            ["beta+test.wlir"],
            "only the harnessed unit flips to +test"
        );
    }

    #[test]
    fn missing_fragments_is_a_pure_existence_check() {
        let tmp = tempfile::tempdir().unwrap();
        let expected: BTreeSet<String> = ["a.wlir".to_string(), "b.wlir".to_string()].into();
        std::fs::write(tmp.path().join("a.wlir"), b"{}").unwrap();
        assert_eq!(missing_fragments(tmp.path(), &expected), vec!["b.wlir"]);
    }
}
