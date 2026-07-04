//! Structural quality assertions across every `scenarios()` Diagnostic.
//!
//! Sourced into `messages.rs` via `#[cfg(test)] #[path = "messages_quality.rs"]
//! mod quality_tests;` so the assertions live alongside the canonical
//! scenarios they cross-check. Kept in its own file to keep `messages.rs`
//! under the `file-size` limit while letting the quality block grow as
//! more scenarios land.

use super::*;
use wl_diagnostic::Applicability;
use wl_diagnostic::render::{Format, render_one};

fn scenarios_iter() -> impl Iterator<Item = (&'static str, Diagnostic)> {
    scenarios().into_iter()
}

#[test]
fn message_has_no_renderer_owned_prefix_or_trailing_dot() {
    for (name, d) in scenarios_iter() {
        let m = &d.message;
        assert!(
            !m.starts_with("error:")
                && !m.starts_with("Error:")
                && !m.starts_with("warning:")
                && !m.starts_with("Warning:"),
            "scenario `{name}`: message must not include the level prefix (renderer adds it): {m:?}",
        );
        assert!(
            !m.trim_end().ends_with('.'),
            "scenario `{name}`: clippy-style messages omit trailing period: {m:?}",
        );
        assert!(!m.is_empty(), "scenario `{name}`: empty message");
    }
}

#[test]
fn warn_or_higher_carries_at_least_one_help() {
    for (name, d) in scenarios_iter() {
        let needs_help = matches!(
            d.level,
            wl_diagnostic::Level::Warn | wl_diagnostic::Level::Deny
        );
        if needs_help {
            assert!(
                !d.helps.is_empty(),
                "scenario `{name}`: {:?}-level diagnostic must carry at least one `help:` line",
                d.level,
            );
        }
    }
}

#[test]
fn machine_applicable_suggestions_parse() {
    for (name, d) in scenarios_iter() {
        for sug in &d.suggestions {
            if sug.applicability != Applicability::MachineApplicable {
                continue;
            }
            let path = std::path::Path::new(&sug.span.file);
            let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
            let replacement = sug.replacement.trim();
            if replacement.is_empty() {
                continue;
            }
            match ext {
                "rs" => {
                    let wrapped = format!("fn __ws_lint_test() {{ {replacement} }}");
                    assert!(
                        syn::parse_str::<syn::File>(&wrapped).is_ok()
                            || syn::parse_str::<syn::File>(replacement).is_ok(),
                        "scenario `{name}`: MachineApplicable replacement in `{}` must parse as Rust\nreplacement:\n{replacement}",
                        sug.span.file.display(),
                    );
                }
                "toml" => {
                    assert!(
                        replacement.parse::<toml::Value>().is_ok()
                            || toml::from_str::<toml::Value>(&format!("[lint]\n{replacement}"))
                                .is_ok()
                            || replacement.starts_with('#'),
                        "scenario `{name}`: MachineApplicable replacement in `{}` must parse as TOML (or be a comment directive)\nreplacement:\n{replacement}",
                        sug.span.file.display(),
                    );
                }
                _ => {}
            }
        }
    }
}

#[test]
fn no_imperative_avoidance_phrases() {
    let banned = [
        "you should",
        "please ",
        "Please ",
        "we should",
        "consider that",
    ];
    for (name, d) in scenarios_iter() {
        let blobs = std::iter::once(d.message.as_str())
            .chain(d.helps.iter().map(String::as_str))
            .chain(d.notes.iter().map(String::as_str));
        for blob in blobs {
            for phrase in banned {
                assert!(
                    !blob.contains(phrase),
                    "scenario `{name}`: phrase `{phrase}` in `{blob}` — use imperative voice",
                );
            }
        }
    }
}

#[test]
fn no_double_spaces_or_edge_whitespace() {
    for (name, d) in scenarios_iter() {
        let blobs = std::iter::once(("message", d.message.as_str()))
            .chain(d.helps.iter().map(|h| ("help", h.as_str())))
            .chain(d.notes.iter().map(|n| ("note", n.as_str())));
        for (kind, blob) in blobs {
            assert_eq!(
                blob.trim(),
                blob,
                "scenario `{name}` {kind}: leading/trailing whitespace in {blob:?}",
            );
            assert!(
                !blob.contains("  "),
                "scenario `{name}` {kind}: double space in {blob:?}",
            );
        }
    }
}

#[test]
fn cross_format_consistency() {
    for (name, d) in scenarios_iter() {
        let mut human_buf = Vec::new();
        render_one(Format::Human, &d, &mut human_buf).unwrap();
        let human_out = String::from_utf8(human_buf).unwrap();

        let mut json_buf = Vec::new();
        render_one(Format::Json, &d, &mut json_buf).unwrap();
        let json_out = String::from_utf8(json_buf).unwrap();

        let mut gh_buf = Vec::new();
        render_one(Format::Github, &d, &mut gh_buf).unwrap();
        let gh_out = String::from_utf8(gh_buf).unwrap();

        let short_lint = d
            .lint
            .strip_prefix("workspace-lint::")
            .unwrap_or(d.lint.as_ref());
        // Suppression directives use snake_case ident form
        // (`workspace_lint::allow!(unused_pub)`); the human renderer prints
        // that form in its `#[warn(...)]` trailer. Either shape identifies
        // the lint.
        let snake_lint = short_lint.replace('-', "_");
        let gh_short_lint = short_lint.replace("::", "%3A%3A");

        assert!(
            human_out.contains(short_lint) || human_out.contains(&snake_lint),
            "scenario `{name}`: human renderer missing lint `{short_lint}` (or `{snake_lint}`)\n{human_out}",
        );
        assert!(
            json_out.contains(short_lint),
            "scenario `{name}`: json renderer missing lint `{short_lint}`\n{json_out}",
        );
        assert!(
            gh_out.contains(short_lint) || gh_out.contains(&gh_short_lint),
            "scenario `{name}`: github renderer missing lint `{short_lint}`\n{gh_out}",
        );

        if let Some(span) = &d.primary {
            let path_str = span.file.display().to_string();
            assert!(
                human_out.contains(&path_str),
                "scenario `{name}`: human renderer missing path `{path_str}`",
            );
            assert!(
                json_out.contains(&path_str),
                "scenario `{name}`: json renderer missing path `{path_str}`",
            );
            let gh_path = path_str.replace(',', "%2C");
            assert!(
                gh_out.contains(&path_str) || gh_out.contains(&gh_path),
                "scenario `{name}`: github renderer missing path `{path_str}`",
            );
        }
    }
}
