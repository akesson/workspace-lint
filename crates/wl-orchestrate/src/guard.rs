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
    /// `[[bench]]`-kind targets: compiled (and linted) only under `--benches`.
    /// Gated on the manifest `bench` flag ([`wl_fast::Manifest::
    /// disabled_bench_targets`]) — `bench = false` opts a target out of
    /// `--benches`, and cargo_metadata does not expose the flag.
    bench_targets: BTreeSet<String>,
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
    /// package selection. Every config kind is modeled (`Default`/`Tests`/
    /// `Benches` — see [`Self::expected_fragments`]); the command parser
    /// already rejects every unmodelable target-selection flag (`--lib`,
    /// `--all-targets`, …) at the config surface.
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
            bench_targets: BTreeSet::new(),
            build_units: BTreeSet::new(),
        };
        for p in &md.packages {
            if !member_ids.contains(&p.id.to_string()) || !want_pkg(p.name.as_str()) {
                continue;
            }
            // `[[bench]] bench = false` lives only in the manifest — loaded
            // lazily (first bench-kind target). An unreadable manifest (the
            // synthetic-metadata tests) yields no exclusions, the default
            // (`bench = true`) direction.
            let mut disabled_benches: Option<BTreeSet<String>> = None;
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
                        "bench" => {
                            let disabled = disabled_benches.get_or_insert_with(|| {
                                wl_fast::Manifest::load(p.manifest_path.as_std_path())
                                    .map(|m| m.disabled_bench_targets())
                                    .unwrap_or_default()
                            });
                            if !disabled.contains(t.name.as_str()) {
                                set.bench_targets.insert(name.clone());
                            }
                        }
                        // The extractor keys build fragments on the PACKAGE
                        // (the target name is always `build-script-build`).
                        "custom-build" => {
                            set.build_units
                                .insert(format!("{}@build", p.name.replace('-', "_")));
                        }
                        _ => {} // example — never linted here
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
    /// integration tests — a bin's harness keeps the `@bin` infix, since the
    /// sibling lib harness shares its crate name). The extractor keys the
    /// suffix on `sess.opts.test` OR test target kind: a `[[test]]
    /// harness = false` target compiles without `--test` but is still a test
    /// unit, and both sides of this contract name it `<name>+test.wlir`.
    /// Only `harnessed` targets flip to `+test`: `--tests` selects by the
    /// manifest `test` flag, so a `test = false` target never compiles in
    /// test mode (its cross-crate uses are still credited — the assembler's
    /// foreign-reach channel covers configs that lack the defining crate).
    /// `Benches` deliberately under-expects: only the `[[bench]]`-kind
    /// targets themselves (as `<name>+test.wlir` — a bench unit compiles
    /// with `CARGO_TARGET_TMPDIR` set, so emission suffixes it `+test`
    /// whatever its harness flag). The lib/bin *bench-harness* units cargo
    /// also builds under `--benches` (their `bench` flag defaults true) are
    /// tolerated byproducts, never expectations: their flag is manifest-only
    /// and per-target, and over-expecting a unit cargo won't build is the
    /// hard-error blocker class. Under-expecting is guard-safe — a forced
    /// re-lint regenerates every unit, and the prune keep-vocabulary is
    /// kind-agnostic ([`crate_name_universe`]).
    pub(super) fn expected_fragments(&self, kinds: Kinds) -> BTreeSet<String> {
        let mut expected = BTreeSet::new();
        match kinds {
            Kinds::Tests => {
                for name in self.harnessed.iter().chain(&self.test_targets) {
                    expected.insert(format!("{name}+test.wlir"));
                }
            }
            Kinds::Benches => {
                for name in &self.bench_targets {
                    expected.insert(format!("{name}+test.wlir"));
                }
            }
            Kinds::Default => {
                for name in &self.compile_units {
                    expected.insert(format!("{name}.wlir"));
                }
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

/// The crate-part vocabulary of every fragment stem the extractor could write
/// for the current workspace: every member target's crate name (`t.name`,
/// dashes→underscores — libs, bins, tests, benches, examples) plus every
/// member package name (build fragments are package-keyed as `<pkg>@build`).
///
/// Deliberately kind-agnostic, unlike [`TargetSet::discover`] (which models
/// only the guard-covered kinds): the scoped pruner keys its keep-check on the
/// crate part of a fragment stem, and its contract is that neither closure
/// imprecision nor a kind-modeling gap may ever delete a *valid* fragment. So
/// its vocabulary must recognize every stem the extractor can emit — not just
/// the ones the completeness guard expects. A stem whose crate part is in this
/// set belongs to some current member target; one that isn't is a rename/
/// removal leftover and is safe to prune.
pub(super) fn crate_name_universe(md: &cargo_metadata::Metadata) -> BTreeSet<String> {
    let member_ids: BTreeSet<String> = md
        .workspace_members
        .iter()
        .map(|id| id.to_string())
        .collect();
    let mut names = BTreeSet::new();
    for p in &md.packages {
        if !member_ids.contains(&p.id.to_string()) {
            continue;
        }
        // Package name: the crate part of a `<pkg>@build` build fragment.
        names.insert(p.name.replace('-', "_"));
        // Every target's crate name: an integration test (`tests/foo.rs` →
        // `foo`) or extra bin (`src/bin/foo.rs` → `foo`) compiles under a
        // crate name that is NOT its package's — the class the old
        // package-name-only prune deleted every warm run.
        for t in &p.targets {
            names.insert(t.name.replace('-', "_"));
        }
    }
    names
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
            bench_targets: BTreeSet::new(),
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

    /// A `[[test]] harness = false` target is still a test unit: cargo builds
    /// it under `--tests` (just without passing `--test` to rustc), so the
    /// guard expects `<name>+test.wlir` — and the extractor now keys the
    /// suffix on the target KIND too, so emission agrees. Keying emission on
    /// `sess.opts.test` alone made every harness-less workspace fail the
    /// guard forever (the 2026-07-10 validation blocker).
    #[test]
    fn harness_false_test_targets_expected_as_plus_test() {
        let set = TargetSet {
            compile_units: ["alpha".to_string()].into(),
            harnessed: ["alpha".to_string()].into(),
            // [[test]] name = "custom" harness = false — cargo_metadata carries
            // no harness field; kind "test" + `test = true` is all we see.
            test_targets: ["custom".to_string()].into(),
            bench_targets: BTreeSet::new(),
            build_units: BTreeSet::new(),
        };
        let tests = set.expected_fragments(Kinds::Tests);
        assert!(tests.contains("custom+test.wlir"), "{tests:?}");
        // And never under the default config (cargo check builds no tests).
        assert!(
            !set.expected_fragments(Kinds::Default)
                .iter()
                .any(|f| f.starts_with("custom"))
        );
    }

    /// One-member synthetic metadata with a lib and three `[[bench]]`
    /// targets, `manifest_path` pointing at a real manifest so the
    /// `bench = false` exclusion is readable.
    fn bench_metadata(manifest: &std::path::Path) -> cargo_metadata::Metadata {
        let target = |name: &str, kind: &str| {
            serde_json::json!({
                "kind": [kind], "crate_types": ["bin"], "name": name,
                "src_path": "/w/alpha/x.rs", "edition": "2021",
                "doc": false, "doctest": false, "test": false,
                "required-features": [],
            })
        };
        serde_json::from_value(serde_json::json!({
            "packages": [{
                "name": "alpha", "version": "0.1.0",
                "id": "path+file:///w/alpha#0.1.0",
                "license": null, "license_file": null, "description": null,
                "source": null,
                "dependencies": [],
                "targets": [
                    target("alpha", "lib"),
                    target("custom", "bench"),
                    target("default_harness", "bench"),
                    target("excluded", "bench"),
                ],
                "features": {},
                "manifest_path": manifest.to_string_lossy(),
                "metadata": null, "publish": null, "authors": [],
                "categories": [], "keywords": [], "readme": null,
                "repository": null, "homepage": null, "documentation": null,
                "edition": "2021", "links": null, "default_run": null,
                "rust_version": null,
            }],
            "workspace_members": ["path+file:///w/alpha#0.1.0"],
            "workspace_default_members": ["path+file:///w/alpha#0.1.0"],
            "resolve": null,
            "workspace_root": "/w", "target_directory": "/w/target",
            "version": 1, "metadata": null,
        }))
        .expect("synthetic metadata deserializes")
    }

    /// `cargo bench` configs are guard-modeled: expected = the `[[bench]]`-
    /// kind targets as `<name>+test.wlir` (bench units compile with
    /// `CARGO_TARGET_TMPDIR` set, so emission suffixes them `+test` whatever
    /// their harness flag) — minus targets whose manifest entry opts out with
    /// `bench = false` (cargo never builds them under `--benches`, and
    /// cargo_metadata does not carry the flag, so it is read from the
    /// manifest). Lib/bin bench-harness byproducts are deliberately NOT
    /// expected — under-expecting is the guard-safe direction.
    #[test]
    fn bench_targets_expected_with_manifest_exclusions() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("Cargo.toml");
        std::fs::write(
            &manifest,
            "[package]\nname = \"alpha\"\nversion = \"0.1.0\"\n\n\
             [[bench]]\nname = \"custom\"\nharness = false\n\n\
             [[bench]]\nname = \"excluded\"\nbench = false\n",
        )
        .unwrap();
        let set = TargetSet::discover(&bench_metadata(&manifest), &[]);
        let benches = set.expected_fragments(Kinds::Benches);
        assert_eq!(
            benches.iter().map(String::as_str).collect::<Vec<_>>(),
            ["custom+test.wlir", "default_harness+test.wlir"],
            "declared benches expected, `bench = false` excluded"
        );
        // Bench targets never leak into the other kinds' expectations.
        for kinds in [Kinds::Default, Kinds::Tests] {
            assert!(
                !set.expected_fragments(kinds)
                    .iter()
                    .any(|f| f.starts_with("custom") || f.starts_with("default_harness")),
                "bench targets expected only under a bench config"
            );
        }
        // An unreadable manifest degrades to no exclusions (default
        // bench = true), never to dropping declared benches.
        let set = TargetSet::discover(&bench_metadata(&tmp.path().join("missing.toml")), &[]);
        assert!(
            set.expected_fragments(Kinds::Benches)
                .contains("excluded+test.wlir")
        );
    }

    #[test]
    fn missing_fragments_is_a_pure_existence_check() {
        let tmp = tempfile::tempdir().unwrap();
        let expected: BTreeSet<String> = ["a.wlir".to_string(), "b.wlir".to_string()].into();
        std::fs::write(tmp.path().join("a.wlir"), b"{}").unwrap();
        assert_eq!(missing_fragments(tmp.path(), &expected), vec!["b.wlir"]);
    }

    /// The scoped pruner's keep-vocabulary spans every member's *targets* AND
    /// package names — not just package names. Crucially it contains this
    /// repo's integration-test crate names (`tests/*.rs` → crate name = file
    /// stem ≠ package `workspace-lint`), the exact class the old
    /// package-name-only prune deleted on every warm run.
    #[test]
    fn crate_name_universe_covers_targets_not_just_packages() {
        let universe = crate_name_universe(&repo_metadata());
        // A member package and its lib/bin crate share this name.
        assert!(universe.contains("workspace_lint"), "{universe:?}");
        // Integration-test crates under crates/workspace-lint/tests/ whose
        // crate name is the file stem, NOT the package name.
        assert!(universe.contains("dogfood"), "{universe:?}");
        assert!(universe.contains("cases"), "{universe:?}");
        // A normal member (package name == lib crate name, underscored).
        assert!(universe.contains("wl_orchestrate"), "{universe:?}");
        // Never a name no member target produces.
        assert!(!universe.contains("definitely_not_a_crate"), "{universe:?}");
    }
}
