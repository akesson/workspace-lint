//! Config validation surfaced as real diagnostics.
//!
//! The typed [`Config`](super::Config) deserialization is deliberately
//! permissive (serde drops unknown keys), so a typo'd section, field, or lint
//! name would otherwise vanish silently — a bad failure mode for a linter.
//! This module re-parses the raw TOML into a [`toml::Value`] tree and diffs
//! every key against the known schema, emitting `workspace-lint::config`
//! (structural mistakes) and `workspace-lint::unknown-lint` (a referenced lint
//! that doesn't exist) diagnostics, each with a "did you mean …?" hint.
//!
//! Truly-unusable *values* (an uncompilable glob, a bad level string) still
//! fail fast at deserialize time — they never reach here.

use crate::diagnostic::Diagnostic;
use crate::diagnostic::builder::at_file;
use crate::lints::LintId;
use crate::suggest::closest;

use super::Config;

/// Top-level config sections. `lints` is validated specially (its keys are
/// lint names, not a fixed field set).
const SECTIONS: &[&str] = &[
    "lints",
    "file-size",
    "crate-size",
    "freshness",
    "cli-crate-version",
    "unused-deps",
    "unused-pub",
    "architecture",
    "expand",
    "macros",
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
        "freshness" => s(&["rules"], Some(&["glob", "depends-on"])),
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
        "unused-deps" => s(&["ignore"], None),
        "unused-pub" => s(
            &[
                "exclude-crates",
                "allowlist",
                "kinds",
                "exclude-paths",
                "suppress-intra-crate",
                "auto-delete",
            ],
            None,
        ),
        "macros" => s(&["external"], None),
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
                "lints" => audit_lints(val, config_path, &mut out),
                section if section_schema(section).is_some() => {
                    audit_section(section, val, config_path, &mut out);
                }
                unknown => out.push(unknown_section(unknown, config_path)),
            }
        }
    }

    audit_enabled_but_unconfigured(config, config_path, &mut out);
    out
}

/// `[lints]` keys must be `default` or a known lint short name.
fn audit_lints(val: &toml::Value, config_path: &str, out: &mut Vec<Diagnostic>) {
    let Some(table) = val.as_table() else { return };
    let known: Vec<&str> = LintId::ALL.iter().map(|l| l.short()).collect();
    for key in table.keys() {
        if key == "default" || LintId::from_short(key).is_some() {
            continue;
        }
        let mut d = at_file(
            LintId::UnknownLint.id(),
            format!("unknown lint `{key}` in `[lints]`"),
            config_path,
        );
        if let Some(sugg) = closest(key, &known) {
            d = d.help(format!("did you mean `{sugg}`?"));
        }
        out.push(d.build());
    }
}

/// Validate a known section's table keys and (for `rules`-style sections)
/// each rule entry's fields.
fn audit_section(section: &str, val: &toml::Value, config_path: &str, out: &mut Vec<Diagnostic>) {
    let Some(schema) = section_schema(section) else {
        return;
    };
    let Some(table) = val.as_table() else { return };
    for key in table.keys() {
        if schema.table.contains(&key.as_str()) {
            continue;
        }
        out.push(unknown_field(
            key,
            &format!("[{section}]"),
            schema.table,
            config_path,
        ));
    }
    // Validate rule entries, if any.
    if let (Some(rule_fields), Some(toml::Value::Array(rules))) = (schema.rule, table.get("rules"))
    {
        let ctx = format!("[[{section}.rules]]");
        for rule in rules {
            audit_entry(rule, rule_fields, &ctx, config_path, out);
        }
    }
    // `[[macros.external]]` entries.
    if section == "macros"
        && let Some(toml::Value::Array(entries)) = table.get("external")
    {
        for entry in entries {
            audit_entry(
                entry,
                &["path", "expansion-uses"],
                "[[macros.external]]",
                config_path,
                out,
            );
        }
    }
}

fn audit_entry(
    entry: &toml::Value,
    allowed: &[&str],
    ctx: &str,
    config_path: &str,
    out: &mut Vec<Diagnostic>,
) {
    let Some(table) = entry.as_table() else {
        return;
    };
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            out.push(unknown_field(key, ctx, allowed, config_path));
        }
    }
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
