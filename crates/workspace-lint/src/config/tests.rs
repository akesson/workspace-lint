use super::*;
use crate::lints::unused_pub::KindFilter;

fn parse(toml: &str) -> Config {
    toml::from_str(toml).expect("config should parse")
}

fn audit_of(toml: &str) -> Vec<Diagnostic> {
    let config = parse(toml);
    audit::audit(toml, ".workspace-lint.toml", &config)
}

fn globs(patterns: &[GlobPattern]) -> Vec<&str> {
    patterns.iter().map(|g| g.as_str()).collect()
}

#[test]
fn parse_full_config() {
    let toml = r#"
[lints]
default = "warn"
centralized-deps = "deny"
unused-pub = "allow"

[[file-size.rules]]
glob = "**/*.rs"
max-code-lines = 500

[[file-size.rules]]
glob = "**/*.ts"
max-code-lines = 300

[[crate-size.rules]]
glob = "crates/*"
max-code-lines = 5000
include = ["*.rs"]

[[freshness.rules]]
glob = "**/CLAUDE.md"
depends-on = "**/*.rs"

[[expand.rules]]
command = ["mise", "tasks"]
glob = "CLAUDE.md"
marker = "MISE_TASKS"
auto-stage = true

[[cli-crate-version.rules]]
command = ["wasm-bindgen", "--version"]
pattern = "wasm-bindgen (\\S+)"
crate = "wasm-bindgen"

[unused-deps]
ignore = ["prost", "tonic"]

[unused-pub]
exclude-crates = ["api", "sdk"]
allowlist = ["*Error", "main"]
kinds = ["function", "struct"]
exclude-paths = ["generated/**"]
"#;

    let config = parse(toml);
    assert_eq!(config.lints.default, Some(LintLevel::Warn));
    assert_eq!(
        config.lints.effective(LintId::CentralizedDeps),
        LintLevel::Deny
    );
    assert_eq!(config.lints.effective(LintId::UnusedPub), LintLevel::Allow);

    let fs_rules = config.file_size.unwrap().rules;
    assert_eq!(fs_rules.len(), 2);
    assert_eq!(fs_rules[0].glob, "**/*.rs");
    assert_eq!(fs_rules[0].max_code_lines, 500);
    assert_eq!(fs_rules[1].glob, "**/*.ts");

    let cs_rules = config.crate_size.unwrap().rules;
    assert_eq!(cs_rules[0].glob, "crates/*");
    assert_eq!(globs(cs_rules[0].include.as_ref().unwrap()), ["*.rs"]);

    let fr_rules = config.freshness.unwrap().rules;
    assert_eq!(fr_rules[0].glob, "**/CLAUDE.md");
    assert_eq!(globs(&fr_rules[0].depends_on.0), ["**/*.rs"]);

    let ex_rules = config.expand.unwrap().rules;
    assert_eq!(ex_rules[0].command, &["mise", "tasks"]);
    assert!(ex_rules[0].auto_stage);

    let cv_rules = config.cli_crate_version.unwrap().rules;
    assert_eq!(cv_rules[0].crate_name, "wasm-bindgen");

    assert_eq!(config.unused_deps.unwrap().ignore, &["prost", "tonic"]);

    let up = config.unused_pub.unwrap();
    assert_eq!(up.exclude_crates, &["api", "sdk"]);
    assert_eq!(globs(&up.allowlist), ["*Error", "main"]);
    assert_eq!(up.kinds, vec![KindFilter::Function, KindFilter::Struct]);
}

#[test]
fn empty_config_warn_default_no_tables() {
    let config = parse("");
    assert!(config.lints.default.is_none());
    // Built-in baseline is warn, so structural lints are on by default.
    assert_eq!(config.lints.effective(LintId::UnusedPub), LintLevel::Warn);
    assert!(config.file_size.is_none());
    assert!(config.unused_pub.is_none());
}

#[test]
fn global_default_allow_disables_structural_but_floors_meta() {
    let config = parse("[lints]\ndefault = \"allow\"\n");
    assert_eq!(config.lints.effective(LintId::UnusedDeps), LintLevel::Allow);
    // Meta-floor: a blanket allow can't silence config validation.
    assert_eq!(config.lints.effective(LintId::Config), LintLevel::Warn);
    assert_eq!(config.lints.effective(LintId::UnknownLint), LintLevel::Warn);
}

#[test]
fn per_lint_override_beats_default() {
    let config = parse("[lints]\ndefault = \"deny\"\nunused-pub = \"allow\"\n");
    assert_eq!(
        config.lints.effective(LintId::CentralizedDeps),
        LintLevel::Deny
    );
    assert_eq!(config.lints.effective(LintId::UnusedPub), LintLevel::Allow);
    assert!(config.lints.has_override(LintId::UnusedPub));
    assert!(!config.lints.has_override(LintId::CentralizedDeps));
}

#[test]
fn unknown_lint_name_is_dropped_from_overrides() {
    // The typed map drops unknown names (the audit reports them); it must
    // not error or masquerade as a real lint.
    let config = parse("[lints]\nunused-dep = \"deny\"\n");
    assert!(config.lints.overrides.is_empty());
}

#[test]
fn unknown_kind_is_a_parse_error() {
    // `method` was documented but never modeled; it now fails fast.
    assert!(toml::from_str::<Config>("[unused-pub]\nkinds = [\"method\"]\n").is_err());
}

#[test]
fn fn_and_mod_kind_aliases_parse() {
    let config = parse("[unused-pub]\nkinds = [\"fn\", \"mod\"]\n");
    let up = config.unused_pub.unwrap();
    assert_eq!(up.kinds, vec![KindFilter::Function, KindFilter::Module]);
}

#[test]
fn parse_unused_pub_defaults() {
    let up = parse("[unused-pub]\n").unused_pub.unwrap();
    assert!(up.allowlist.is_empty());
    assert!(up.kinds.is_empty());
    assert!(up.exclude_paths.is_empty());
}

#[test]
fn parse_crate_size_no_include() {
    let rules = parse("[[crate-size.rules]]\nglob = \"crates/*\"\nmax-code-lines = 5000\n")
        .crate_size
        .unwrap()
        .rules;
    assert!(rules[0].include.is_none());
}

#[test]
fn freshness_depends_on_accepts_string_or_list() {
    let one = parse("[[freshness.rules]]\nglob = \"a\"\ndepends-on = \"**/*.rs\"\n")
        .freshness
        .unwrap()
        .rules;
    assert_eq!(globs(&one[0].depends_on.0), ["**/*.rs"]);

    let many = parse("[[freshness.rules]]\nglob = \"a\"\ndepends-on = [\"x\", \"y\"]\n")
        .freshness
        .unwrap()
        .rules;
    assert_eq!(globs(&many[0].depends_on.0), ["x", "y"]);
}

#[test]
fn architecture_rule_severity_is_optional() {
    let none = parse("[[architecture.rules]]\nfrom = [\"a\"]\ndeny = [\"b\"]\n")
        .architecture
        .unwrap()
        .rules;
    assert_eq!(none[0].severity, None);

    let deny =
        parse("[[architecture.rules]]\nfrom = [\"a\"]\ndeny = [\"b\"]\nseverity = \"deny\"\n")
            .architecture
            .unwrap()
            .rules;
    assert_eq!(deny[0].severity, Some(LintLevel::Deny));
}

// --- audit (config validation as diagnostics) ---

#[test]
fn audit_clean_config_has_no_findings() {
    let diags = audit_of(
        "[lints]\ncentralized-deps = \"deny\"\n\n[[file-size.rules]]\nglob = \"**/*.rs\"\nmax-code-lines = 500\n",
    );
    assert!(diags.is_empty(), "unexpected: {diags:?}");
}

#[test]
fn audit_flags_unknown_section() {
    let diags = audit_of("[file-siz]\n");
    assert!(
        diags
            .iter()
            .any(|d| d.lint.ends_with("::config") && d.message.contains("file-siz"))
    );
}

#[test]
fn audit_flags_unknown_lint_name() {
    let diags = audit_of("[lints]\nunused-dep = \"deny\"\n");
    let d = diags
        .iter()
        .find(|d| d.lint.ends_with("::unknown-lint"))
        .expect("expected an unknown-lint diagnostic");
    assert!(d.message.contains("unused-dep"));
    assert!(d.helps.iter().any(|h| h.contains("unused-deps")));
}

#[test]
fn audit_flags_unknown_field_in_table() {
    let diags = audit_of("[unused-pub]\nallowlistt = []\n");
    assert!(
        diags
            .iter()
            .any(|d| d.lint.ends_with("::config") && d.message.contains("allowlistt"))
    );
}

#[test]
fn audit_flags_unknown_rule_field() {
    // An *optional* rule field typo is a soft diagnostic; a required-field
    // typo (`max_code_lines`) fails fast as a missing-field parse error.
    let diags =
        audit_of("[[crate-size.rules]]\nglob = \"a\"\nmax-code-lines = 5\ninclud = [\"*.rs\"]\n");
    assert!(
        diags
            .iter()
            .any(|d| d.lint.ends_with("::config") && d.message.contains("includ"))
    );
}

#[test]
fn audit_flags_policy_lint_enabled_without_config() {
    let diags = audit_of("[lints]\nfile-size = \"deny\"\n");
    assert!(diags.iter().any(|d| {
        d.lint.ends_with("::config")
            && d.message.contains("file-size")
            && d.message.contains("never run")
    }));
}

#[test]
fn audit_does_not_flag_default_unconfigured_policy_lint() {
    // The global `default` leaving policy lints unconfigured is fine; only
    // an *explicit* override without a table is the mistake.
    let diags = audit_of("[lints]\ndefault = \"warn\"\n");
    assert!(diags.is_empty(), "unexpected: {diags:?}");
}

// --- per-crate config tier ---

#[test]
fn parse_per_crate_levels_and_params() {
    let toml = r#"
[lints]
default = "warn"

[crates.legacy.lints]
file-size = "allow"
default = "allow"

[crates.api.lints]
unused-pub = "deny"

[crates.api.unused-pub]
allowlist = ["*Builder"]

[crates.worker.unused-deps]
ignore = ["prost", "tonic"]
"#;
    let config = parse(toml);
    assert_eq!(config.crates.len(), 3);

    let legacy = &config.crates["legacy"];
    assert_eq!(legacy.lints.default, Some(LintLevel::Allow));
    assert_eq!(
        legacy.lints.overrides.get(&LintId::FileSize),
        Some(&LintLevel::Allow)
    );

    let worker = config.crates["worker"].unused_deps.as_ref().unwrap();
    assert_eq!(worker.ignore, &["prost", "tonic"]);
    assert!(config.crates["api"].unused_pub.is_some());
    assert!(config.crates["api"].unused_deps.is_none());
}

#[test]
fn effective_level_per_crate_override_beats_global() {
    let config =
        parse("[lints]\nfile-size = \"deny\"\n\n[crates.legacy.lints]\nfile-size = \"allow\"\n");
    // Global level applies everywhere except the opted-out crate.
    assert_eq!(
        config.effective_level(LintId::FileSize, None),
        LintLevel::Deny
    );
    assert_eq!(
        config.effective_level(LintId::FileSize, Some("other")),
        LintLevel::Deny
    );
    assert_eq!(
        config.effective_level(LintId::FileSize, Some("legacy")),
        LintLevel::Allow
    );
}

#[test]
fn per_crate_default_opts_whole_crate_out_except_explicit() {
    let config = parse(
        "[lints]\nfile-size = \"deny\"\nunused-pub = \"deny\"\n\n\
         [crates.legacy.lints]\ndefault = \"allow\"\nunused-pub = \"warn\"\n",
    );
    // The per-crate `default = allow` overrides even a global *explicit*
    // entry, so the crate is opted out wholesale...
    assert_eq!(
        config.effective_level(LintId::FileSize, Some("legacy")),
        LintLevel::Allow
    );
    // ...except for keys the crate explicitly sets.
    assert_eq!(
        config.effective_level(LintId::UnusedPub, Some("legacy")),
        LintLevel::Warn
    );
    // Other crates are untouched.
    assert_eq!(
        config.effective_level(LintId::FileSize, Some("other")),
        LintLevel::Deny
    );
}

#[test]
fn per_crate_without_default_falls_through_to_global() {
    let config =
        parse("[lints]\ndefault = \"deny\"\n\n[crates.api.lints]\nunused-pub = \"allow\"\n");
    // Explicit per-crate override wins.
    assert_eq!(
        config.effective_level(LintId::UnusedPub, Some("api")),
        LintLevel::Allow
    );
    // A key with no per-crate entry (and no per-crate default) falls
    // through to the global baseline.
    assert_eq!(
        config.effective_level(LintId::FileSize, Some("api")),
        LintLevel::Deny
    );
}

#[test]
fn per_crate_default_allow_still_floors_meta_lints() {
    // A per-crate baseline `allow` can't silence config validation, mirroring
    // the global meta-floor.
    let config = parse("[crates.api.lints]\ndefault = \"allow\"\n");
    assert_eq!(
        config.effective_level(LintId::Config, Some("api")),
        LintLevel::Warn
    );
    assert_eq!(
        config.effective_level(LintId::UnknownLint, Some("api")),
        LintLevel::Warn
    );
}

#[test]
fn per_crate_overrides_collect_only_present_sections() {
    let config = parse(
        "[crates.worker.unused-deps]\nignore = [\"prost\"]\n\n[crates.api.unused-pub]\nallowlist = [\"*X\"]\n",
    );
    let dep_ov = config.unused_deps_overrides();
    assert_eq!(dep_ov.len(), 1);
    assert_eq!(dep_ov["worker"].ignore, &["prost"]);

    let pub_ov = config.unused_pub_overrides();
    assert_eq!(pub_ov.len(), 1);
    assert!(pub_ov.contains_key("api"));
}

// --- per-crate audit (structure + membership) ---

#[test]
fn audit_per_crate_glob_lint_redirects_to_glob() {
    let diags = audit_of("[crates.legacy.file-size]\nmax-code-lines = 100\n");
    let d = diags
        .iter()
        .find(|d| d.lint.ends_with("::config"))
        .expect("config diag");
    assert!(d.message.contains("file-size"));
    assert!(d.message.contains("not configurable per-crate"));
    assert!(d.helps.iter().any(|h| h.contains("glob")));
}

#[test]
fn audit_per_crate_non_param_lint_points_to_levels() {
    let diags = audit_of("[crates.api.architecture]\nrules = []\n");
    let d = diags
        .iter()
        .find(|d| d.lint.ends_with("::config"))
        .expect("config diag");
    assert!(d.message.contains("architecture"));
    assert!(d.helps.iter().any(|h| h.contains("crates.api.lints")));
}

#[test]
fn audit_per_crate_unknown_lint_name_in_lints() {
    let diags = audit_of("[crates.api.lints]\nunused-dep = \"deny\"\n");
    let d = diags
        .iter()
        .find(|d| d.lint.ends_with("::unknown-lint"))
        .expect("unknown-lint diag");
    assert!(d.message.contains("unused-dep"));
    assert!(d.message.contains("[crates.api.lints]"));
    assert!(d.helps.iter().any(|h| h.contains("unused-deps")));
}

#[test]
fn audit_per_crate_unknown_param_field() {
    let diags = audit_of("[crates.api.unused-pub]\nallowlistt = []\n");
    assert!(
        diags
            .iter()
            .any(|d| d.lint.ends_with("::config") && d.message.contains("allowlistt"))
    );
}

#[test]
fn audit_per_crate_unknown_block_key_suggests_eligible() {
    let diags = audit_of("[crates.api.lintz]\nfoo = 1\n");
    let d = diags
        .iter()
        .find(|d| d.lint.ends_with("::config"))
        .expect("config diag");
    assert!(d.helps.iter().any(|h| h.contains("lints")));
}

#[test]
fn audit_crate_names_flags_unknown_member() {
    let config = parse("[crates.workerr.unused-deps]\nignore = []\n");
    let members = vec!["worker".to_string(), "api".to_string()];
    let diags = audit::audit_crate_names(&config, &members, ".workspace-lint.toml");
    let d = diags
        .iter()
        .find(|d| d.lint.ends_with("::config"))
        .expect("config diag");
    assert!(d.message.contains("workerr"));
    assert!(d.helps.iter().any(|h| h.contains("worker")));
}

#[test]
fn audit_crate_names_clean_for_real_members() {
    let config = parse("[crates.worker.unused-deps]\nignore = []\n");
    let members = vec!["worker".to_string()];
    assert!(audit::audit_crate_names(&config, &members, ".workspace-lint.toml").is_empty());
}

// --- extract_metadata_section ---

#[test]
fn extract_metadata_with_lints() {
    let cargo_toml = r#"
[workspace]
members = ["crates/*"]

[workspace.metadata.workspace-lint.lints]
centralized-deps = "deny"
"#;
    let raw = extract_metadata_section(cargo_toml).unwrap();
    let config: Config = toml::from_str(&raw).unwrap();
    assert_eq!(
        config.lints.effective(LintId::CentralizedDeps),
        LintLevel::Deny
    );
}

#[test]
fn extract_metadata_with_rules() {
    let cargo_toml = r#"
[workspace]
members = []

[[workspace.metadata.workspace-lint.file-size.rules]]
glob = "**/*.rs"
max-code-lines = 500
"#;
    let raw = extract_metadata_section(cargo_toml).unwrap();
    let rules = toml::from_str::<Config>(&raw)
        .unwrap()
        .file_size
        .unwrap()
        .rules;
    assert_eq!(rules[0].glob, "**/*.rs");
}

#[test]
fn extract_metadata_returns_none_no_workspace() {
    assert!(extract_metadata_section("[package]\nname = \"foo\"\n").is_none());
}

#[test]
fn extract_metadata_returns_none_no_lint_section() {
    let cargo_toml = "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.metadata.other-tool]\nkey = \"value\"\n";
    assert!(extract_metadata_section(cargo_toml).is_none());
}

#[test]
fn parse_expand_defaults() {
    let rules = parse("[[expand.rules]]\ncommand = [\"echo\", \"hi\"]\nglob = \"README.md\"\nmarker = \"HELLO\"\n")
        .expand
        .unwrap()
        .rules;
    assert!(!rules[0].auto_stage);
}

// --- [engine] table ---

fn selector_ids(section: &EngineSection) -> Vec<String> {
    section.selectors().into_iter().map(|s| s.id).collect()
}

#[test]
fn engine_defaults_to_single_default_config() {
    // Absent table and present-table-absent-key both mean `["default"]`.
    assert_eq!(parse("").engine.configs, ["default"]);
    assert_eq!(parse("[engine]\n").engine.configs, ["default"]);
}

#[test]
fn engine_selectors_map_entries_in_order() {
    let config = parse("[engine]\nconfigs = [\"default\", \"--tests\"]\n");
    assert_eq!(selector_ids(&config.engine), ["default", "tests"]);
    // First entry = primary: declaration order is preserved.
    let config = parse("[engine]\nconfigs = [\"tests\", \"default\"]\n");
    assert_eq!(selector_ids(&config.engine), ["tests", "default"]);
}

#[test]
fn engine_selectors_accept_tests_alias() {
    let config = parse("[engine]\nconfigs = [\"tests\"]\n");
    let selectors = config.engine.selectors();
    assert_eq!(selectors.len(), 1);
    assert_eq!(selectors[0].id, "tests");
    assert_eq!(selectors[0].cargo_args, ["--tests"]);
}

#[test]
fn engine_selectors_fall_back_to_default_when_empty() {
    // An explicit empty list — and an all-unknown list (the audit already
    // reported the entries) — still yields a primary config.
    for toml in [
        "[engine]\nconfigs = []\n",
        "[engine]\nconfigs = [\"nonsense\"]\n",
    ] {
        assert_eq!(selector_ids(&parse(toml).engine), ["default"]);
    }
}

#[test]
fn audit_engine_clean_for_known_configs() {
    let diags = audit_of("[engine]\nconfigs = [\"default\", \"--tests\", \"tests\"]\n");
    assert!(diags.is_empty(), "unexpected: {diags:?}");
}

#[test]
fn audit_flags_unknown_engine_config() {
    let diags = audit_of("[engine]\nconfigs = [\"default\", \"--test\"]\n");
    let d = diags
        .iter()
        .find(|d| d.lint.ends_with("::config"))
        .expect("expected a config diagnostic");
    assert!(d.message.contains("--test"));
    assert!(d.helps.iter().any(|h| h.contains("--tests")));
}

#[test]
fn audit_flags_unknown_engine_field() {
    let diags = audit_of("[engine]\nconfig = [\"default\"]\n");
    assert!(
        diags
            .iter()
            .any(|d| d.lint.ends_with("::config") && d.message.contains("`config`")),
        "unexpected: {diags:?}"
    );
}

#[test]
fn engine_known_list_matches_selector_for() {
    // The audit suggests from KNOWN; every suggestion must actually map.
    for entry in EngineSection::KNOWN {
        assert!(
            EngineSection::selector_for(entry).is_some(),
            "`{entry}` is in KNOWN but has no selector"
        );
    }
}
