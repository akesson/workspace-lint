//! Config validation surfaced as real diagnostics.
//!
//! The typed [`Config`] deserialization is deliberately
//! permissive (serde drops unknown keys), so a typo'd section, field, or lint
//! name would otherwise vanish silently — a bad failure mode for a linter.
//! This module re-parses the raw TOML into a [`toml::Value`] tree and diffs
//! every key against the known schema, emitting `workspace-lint::config`
//! (structural mistakes) and `workspace-lint::unknown-lint` (a referenced lint
//! that doesn't exist) diagnostics, each with a "did you mean …?" hint.
//!
//! Truly-unusable *values* (an uncompilable glob, a bad level string) still
//! fail fast at deserialize time — they never reach here.

use crate::suggest::closest;
use wl_diagnostic::Diagnostic;
use wl_diagnostic::builder::at_file;
use wl_lint_api::LintId;

use super::Config;

/// Top-level config sections. `lints` is validated specially (its keys are
/// lint names, not a fixed field set).
const SECTIONS: &[&str] = &[
    "lints",
    "file-size",
    "crate-size",
    "cli-crate-version",
    "duplicate-code",
    "unused-deps",
    "unused-pub",
    "architecture",
    "expand",
    "engine",
];

/// Allowed field names for a section's flat table or its `rules`/entry
/// tables. Returned by [`section_schema`].
struct Schema {
    /// Fields valid directly under `[section]`.
    table: &'static [&'static str],
    /// When `Some`, `[section]` is a `rules`-style section; these are the
    /// fields valid in each `[[section.rules]]` entry.
    rule: Option<&'static [&'static str]>,
}

fn section_schema(section: &str) -> Option<Schema> {
    let s = |table, rule| Some(Schema { table, rule });
    match section {
        "file-size" => s(&["rules"], Some(&["glob", "max-code-lines"])),
        "crate-size" => s(&["rules"], Some(&["glob", "max-code-lines", "include"])),
        "cli-crate-version" => s(&["rules"], Some(&["command", "pattern", "crate"])),
        "architecture" => s(
            &["rules"],
            Some(&[
                "name",
                "from",
                "deny",
                "exceptions",
                "severity",
                "reason",
                "suggest",
            ]),
        ),
        "expand" => s(
            &["rules"],
            Some(&["command", "glob", "marker", "auto-stage"]),
        ),
        "duplicate-code" => s(
            &[
                "min-lines",
                "min-tokens",
                "min-instances",
                "ignore-literals",
                "ignore-test-code",
                "cross-crate-only",
                "min-distinct-anchors",
                "min-non-repeating-ratio",
                "max-parameters",
                "max-live-out",
                "classify",
                "component-macros",
                "baseline",
                "include",
                "exclude",
            ],
            None,
        ),
        "unused-deps" => s(&["ignore"], None),
        "unused-pub" => s(
            &[
                "exclude-crates",
                "allowlist",
                "kinds",
                "exclude-paths",
                "suppress-intra-crate",
                "assume-all-public",
                "publish-hint-threshold",
            ],
            None,
        ),
        "engine" => s(&["configs"], None),
        _ => None,
    }
}

/// Validate `raw` (the config TOML text) against the known schema, anchoring
/// every diagnostic at `config_path` (`.workspace-lint.toml` or `Cargo.toml`).
/// `config` is the already-parsed model, used for the "enabled but unconfigured"
/// cross-check.
pub(super) fn audit(raw: &str, config_path: &str, config: &Config) -> Vec<Diagnostic> {
    let mut out = Vec::new();

    if let Ok(toml::Value::Table(root)) = toml::from_str::<toml::Value>(raw) {
        for (key, val) in &root {
            match key.as_str() {
                "lints" => audit_lints(val, "[lints]", config_path, &mut out),
                "crates" => audit_crates_tree(val, config_path, &mut out),
                "macros" => out.push(macros_deprecated(config_path)),
                section if section_schema(section).is_some() => {
                    audit_section(section, val, config_path, &mut out);
                }
                unknown => out.push(unknown_section(unknown, config_path)),
            }
        }
    }

    audit_enabled_but_unconfigured(config, config_path, &mut out);
    audit_engine_configs(config, config_path, &mut out);
    out
}

/// `[engine] configs` entries must parse as supported cargo commands — a
/// rejected one would otherwise be silently skipped when the semantic tier
/// builds its extraction matrix (see [`super::EngineSection::selectors`]).
/// The parser's error text carries the entry-specific guidance (old-vocabulary
/// mapping, unsupported-flag reasons, …). Validated from the typed config
/// (like the enabled-but-unconfigured check): an absent table defaults to the
/// build+test matrix, which is always clean.
fn audit_engine_configs(config: &Config, config_path: &str, out: &mut Vec<Diagnostic>) {
    for entry in &config.engine.configs {
        let Err(err) = wl_engine::parse_command(entry) else {
            continue;
        };
        out.push(
            at_file(
                LintId::Config.id(),
                format!("invalid engine config `{entry}` in `[engine] configs`: {err}"),
                config_path,
            )
            .help(
                "an engine config is a real cargo command the project runs, e.g. `cargo build`, \
                 `cargo test`, or `cargo build --target wasm32-unknown-unknown -p <pkg>`",
            )
            .build(),
        );
    }

    // Order guidance: the first entry is the primary — crates it covers are
    // judged in its universe. A scoped/targeted first entry ahead of a plain
    // whole-workspace host entry inverts the natural home for every shared
    // crate.
    let specs: Vec<wl_engine::ConfigSpec> = config
        .engine
        .configs
        .iter()
        .filter_map(|e| wl_engine::parse_command(e).ok())
        .collect();
    if let Some(first) = specs.first()
        && (first.target.is_some() || !first.packages.is_empty())
        && let Some(host) = specs[1..]
            .iter()
            .find(|s| s.target.is_none() && s.packages.is_empty())
    {
        out.push(
            at_file(
                LintId::Config.id(),
                format!(
                    "`[engine] configs` lists the scoped `{}` before the whole-workspace `{}`; \
                     the first entry is the primary config",
                    first.display, host.display
                ),
                config_path,
            )
            .help("list your main build first so every crate is judged in its natural home config")
            .build(),
        );
    }
}

/// `[lints]`-shaped keys must be `default` or a known lint short name. `ctx` is
/// the rendered table label (`[lints]` or `[crates.<name>.lints]`).
fn audit_lints(val: &toml::Value, ctx: &str, config_path: &str, out: &mut Vec<Diagnostic>) {
    let Some(table) = val.as_table() else { return };
    let known: Vec<&str> = LintId::ALL.iter().map(|l| l.short()).collect();
    for key in table.keys() {
        if key == "default" || LintId::from_short(key).is_some() {
            continue;
        }
        let mut d = at_file(
            LintId::UnknownLint.id(),
            format!("unknown lint `{key}` in `{ctx}`"),
            config_path,
        );
        if let Some(sugg) = closest(key, &known) {
            d = d.help(format!("did you mean `{sugg}`?"));
        }
        out.push(d.build());
    }
}

/// Keys permitted directly inside a `[crates.<name>]` block: per-crate levels
/// (`lints`) plus the two lints that accept per-crate *params*.
const PER_CRATE_KEYS: &[&str] = &["lints", "unused-deps", "unused-pub"];

/// Lints whose per-crate scoping is their glob, not a per-crate param section —
/// so a `[crates.X.<lint>]` block is redirected to a glob rule.
const GLOB_SCOPED_LINTS: &[&str] = &["file-size", "crate-size"];

/// Validate the `[crates.*]` tree's *structure* (not crate names — that needs
/// the resolved workspace; see [`audit_crate_names`]). Each `[crates.<name>]`
/// block may carry `lints` (validated like `[lints]`) and the `unused-deps` /
/// `unused-pub` param sections; any other key is rejected, with glob-scoped
/// lints redirected to a glob rule and everything else to a per-crate level.
fn audit_crates_tree(val: &toml::Value, config_path: &str, out: &mut Vec<Diagnostic>) {
    let Some(crates) = val.as_table() else { return };
    for (crate_name, crate_val) in crates {
        let Some(block) = crate_val.as_table() else {
            continue;
        };
        for (key, sub) in block {
            match key.as_str() {
                "lints" => {
                    audit_lints(
                        sub,
                        &format!("[crates.{crate_name}.lints]"),
                        config_path,
                        out,
                    );
                }
                section @ ("unused-deps" | "unused-pub") => {
                    let ctx = format!("[crates.{crate_name}.{section}]");
                    if let Some(schema) = section_schema(section) {
                        audit_table_fields(sub, schema.table, &ctx, config_path, out);
                    }
                }
                other => out.push(per_crate_bad_key(crate_name, other, config_path)),
            }
        }
    }
}

/// A `[crates.<name>.<key>]` whose `<key>` isn't a per-crate-eligible section.
/// Glob-scoped lints get a redirect to a `glob` rule; any other known lint is
/// told it only takes a per-crate *level*; an unrecognized key gets the usual
/// "did you mean …?".
fn per_crate_bad_key(crate_name: &str, key: &str, config_path: &str) -> Diagnostic {
    let ctx = format!("[crates.{crate_name}]");
    let mut d = at_file(
        LintId::Config.id(),
        format!("`{key}` is not configurable per-crate in `{ctx}`"),
        config_path,
    );
    if GLOB_SCOPED_LINTS.contains(&key) {
        d = d.help(format!(
            "{key} scopes per-crate via its glob — add a `[[{key}.rules]]` with \
             `glob = \"crates/{crate_name}/**\"` instead; for a per-crate severity use \
             `[crates.{crate_name}.lints] {key} = \"…\"`"
        ));
    } else if LintId::from_short(key).is_some() {
        d = d.help(format!(
            "only `unused-deps` / `unused-pub` take per-crate params; for a per-crate severity \
             use `[crates.{crate_name}.lints] {key} = \"…\"`"
        ));
    } else if let Some(sugg) = closest(key, PER_CRATE_KEYS) {
        d = d.help(format!("did you mean `{sugg}`?"));
    }
    d.build()
}

/// Validate the per-crate tier's crate *names* against the resolved workspace
/// `members`. A `[crates.<name>]` whose `<name>` isn't a member is a `config`
/// error (typo or stale entry) with a "did you mean …?" against the members.
/// Called from [`super::audit_crate_membership`] once the workspace is loaded.
pub(super) fn audit_crate_names(
    config: &Config,
    members: &[String],
    config_path: &str,
) -> Vec<Diagnostic> {
    let member_refs: Vec<&str> = members.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    // Stable order so the diagnostic stream (and snapshots) don't depend on
    // HashMap iteration order.
    let mut names: Vec<&String> = config.crates.keys().collect();
    names.sort();
    for name in names {
        if member_refs.contains(&name.as_str()) {
            continue;
        }
        let mut d = at_file(
            LintId::Config.id(),
            format!("`[crates.{name}]` does not match any workspace member"),
            config_path,
        );
        if let Some(sugg) = closest(name, &member_refs) {
            d = d.help(format!("did you mean `{sugg}`?"));
        }
        out.push(d.build());
    }
    out
}

/// Check a flat table's keys against `allowed`, emitting an `unknown-field`
/// `config` diagnostic (with "did you mean …?") for each stray key. Shared by
/// [`audit_section`] and the per-crate param-section validation.
fn audit_table_fields(
    val: &toml::Value,
    allowed: &[&str],
    ctx: &str,
    config_path: &str,
    out: &mut Vec<Diagnostic>,
) {
    let Some(table) = val.as_table() else { return };
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            out.push(unknown_field(key, ctx, allowed, config_path));
        }
    }
}

/// Validate a known section's table keys and (for `rules`-style sections)
/// each rule entry's fields.
fn audit_section(section: &str, val: &toml::Value, config_path: &str, out: &mut Vec<Diagnostic>) {
    let Some(schema) = section_schema(section) else {
        return;
    };
    let Some(table) = val.as_table() else { return };
    audit_table_fields(val, schema.table, &format!("[{section}]"), config_path, out);
    // Validate rule entries, if any.
    if let (Some(rule_fields), Some(toml::Value::Array(rules))) = (schema.rule, table.get("rules"))
    {
        let ctx = format!("[[{section}.rules]]");
        for rule in rules {
            audit_table_fields(rule, rule_fields, &ctx, config_path, out);
        }
    }
}

/// `[macros]` (the `[[macros.external]]` expansion-uses table) fed the syn
/// resolver's blind spot for macro-generated references. The rustc engine
/// sees expansions natively, so the whole surface is obsolete — a loud
/// deprecation rather than silence, because a config carrying the section
/// almost certainly still believes it does something.
fn macros_deprecated(config_path: &str) -> Diagnostic {
    at_file(
        LintId::Config.id(),
        "`[macros]` is obsolete: the engine sees macro expansions natively",
        config_path,
    )
    .help(
        "delete the `[macros]` section; `expansion_uses!` annotations and \
         `# workspace-lint: expansion-uses(...)` comments are no longer read",
    )
    .build()
}

/// A policy lint leveled-on in `[lints]` but missing its config table will
/// never fire — almost certainly a mistake worth flagging. Only fires for an
/// *explicit* per-lint entry, not the global `default` (which silently leaves
/// unconfigured policy lints off, as intended).
fn audit_enabled_but_unconfigured(config: &Config, config_path: &str, out: &mut Vec<Diagnostic>) {
    for &id in LintId::ALL {
        if !id.requires_config() || !config.lints.has_override(id) {
            continue;
        }
        if config.lints.effective(id) == super::LintLevel::Allow {
            continue;
        }
        if config.has_table_for(id) {
            continue;
        }
        let short = id.short();
        out.push(
            at_file(
                LintId::Config.id(),
                format!(
                    "lint `{short}` is enabled in `[lints]` but has no `[{short}]` configuration, so it will never run"
                ),
                config_path,
            )
            .help(format!(
                "add a `[{short}]` section (or `[[{short}.rules]]`), or remove `{short}` from `[lints]`"
            ))
            .build(),
        );
    }
}

fn unknown_section(key: &str, config_path: &str) -> Diagnostic {
    let mut d = at_file(
        LintId::Config.id(),
        format!("unknown configuration section `{key}`"),
        config_path,
    );
    if let Some(sugg) = closest(key, SECTIONS) {
        d = d.help(format!("did you mean `{sugg}`?"));
    }
    d.build()
}

fn unknown_field(key: &str, ctx: &str, allowed: &[&str], config_path: &str) -> Diagnostic {
    let mut d = at_file(
        LintId::Config.id(),
        format!("unknown key `{key}` in `{ctx}`"),
        config_path,
    );
    if let Some(sugg) = closest(key, allowed) {
        d = d.help(format!("did you mean `{sugg}`?"));
    }
    d.build()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config that exercises **every** section and **every** field that
    /// [`SECTIONS`] / [`section_schema`] know about. [`audit`] must report zero
    /// findings: a stray finding means a typed `Config` field exists that the
    /// schema doesn't list (so a valid config would draw a false `unknown key`).
    ///
    /// FORCING FUNCTION: when you add a field to any config struct, add it here
    /// too — otherwise adding the field to the struct but forgetting
    /// `section_schema` makes this test fail the moment the key appears below.
    const EVERY_KEY_CONFIG: &str = r#"
[lints]
default = "warn"
centralized-deps = "deny"
file-size = "deny"

[[file-size.rules]]
glob = "**/*.rs"
max-code-lines = 500

[[crate-size.rules]]
glob = "crates/*"
max-code-lines = 5000
include = ["*.rs"]

[[cli-crate-version.rules]]
command = ["wasm-bindgen", "--version"]
pattern = 'wasm-bindgen (\S+)'
crate = "wasm-bindgen"

[[architecture.rules]]
name = "layering"
from = ["crates/a/**"]
deny = ["crates/b/**"]
exceptions = ["crates/a/src/special.rs"]
severity = "deny"
reason = "a must not depend on b"
suggest = "use the shared crate"

[[expand.rules]]
command = ["mise", "tasks"]
glob = "CLAUDE.md"
marker = "MISE_TASKS"
auto-stage = true

[duplicate-code]
min-lines = 8
min-tokens = 40
min-instances = 2
ignore-literals = true
ignore-test-code = true
cross-crate-only = false
min-distinct-anchors = 4
min-non-repeating-ratio = 0.5
max-parameters = 3
max-live-out = 1
classify = true
component-macros = ["rsx"]
baseline = "duplicate-code.baseline.toml"
include = ["crates/**"]
exclude = ["**/generated/**"]

[unused-deps]
ignore = ["prost", "tonic"]

[unused-pub]
exclude-crates = ["api"]
allowlist = ["*Error"]
kinds = ["function", "struct"]
exclude-paths = ["generated/**"]
suppress-intra-crate = true
assume-all-public = false
publish-hint-threshold = 3

[engine]
configs = ["cargo build", "cargo test"]

[crates.demo.lints]
default = "warn"
unused-pub = "deny"

[crates.demo.unused-deps]
ignore = ["foo"]

[crates.demo.unused-pub]
exclude-crates = ["x"]
allowlist = ["Y"]
"#;

    #[test]
    fn macros_section_is_deprecated() {
        let raw = "[[macros.external]]\npath = \"tokio::main\"\n";
        let config: Config = toml::from_str(raw).expect("config parses");
        let findings = audit(raw, ".workspace-lint.toml", &config);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("`[macros]` is obsolete"));
    }

    #[test]
    fn audit_schema_covers_every_config_key() {
        let config: Config = toml::from_str(EVERY_KEY_CONFIG).expect("config parses");
        let findings = audit(EVERY_KEY_CONFIG, ".workspace-lint.toml", &config);
        assert!(
            findings.is_empty(),
            "audit flagged keys in a fully-populated config — the schema in \
             config/audit.rs is out of sync with the typed Config:\n{}",
            findings
                .iter()
                .map(|d| d.message.clone())
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
