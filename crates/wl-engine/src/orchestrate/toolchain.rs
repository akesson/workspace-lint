//! Preflight: fail with the exact remediation *before* any build starts.
//!
//! The full tier needs (a) the pinned nightly with `rustc-dev` +
//! `llvm-tools-preview` (the dylib links `rustc_private`), (b) the
//! `dylint-link` linker wrapper on PATH, and (c) — for every `--target`
//! declared in the config matrix — that target's std installed for the pin
//! (the extraction `cargo check` runs on the pinned nightly). All checks are
//! cheap (subprocess-free where possible) and their failures render the exact
//! commands to run — see the `EngineError` variants' `Display`.

use std::collections::BTreeSet;
use std::process::Command;

use super::EngineError;

pub(super) fn preflight(pin: &str, triples: &BTreeSet<String>) -> Result<(), EngineError> {
    let toolchains = rustup(&["toolchain", "list"])?;
    if !toolchains.lines().any(|l| l.starts_with(pin)) {
        return Err(EngineError::ToolchainMissing { pin: pin.into() });
    }
    let components = rustup(&["component", "list", "--installed", "--toolchain", pin])?;
    for component in ["rustc-dev", "llvm-tools"] {
        if !components.lines().any(|l| l.starts_with(component)) {
            return Err(EngineError::ComponentMissing {
                pin: pin.into(),
                component: component.into(),
            });
        }
    }
    if !triples.is_empty() {
        let installed = rustup(&["target", "list", "--installed", "--toolchain", pin])?;
        for triple in triples {
            if !installed.lines().any(|l| l.trim() == triple) {
                return Err(EngineError::TargetMissing {
                    pin: pin.into(),
                    triple: triple.clone(),
                });
            }
        }
    }
    if !on_path("dylint-link") {
        return Err(EngineError::DylintLinkMissing);
    }
    Ok(())
}

/// Run rustup and capture stdout; a spawn failure means rustup isn't there.
fn rustup(args: &[&str]) -> Result<String, EngineError> {
    let output = Command::new("rustup")
        .args(args)
        .output()
        .map_err(|_| EngineError::RustupMissing)?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// PATH scan for an executable (with the Windows `.exe` variant) — cheaper and
/// quieter than spawning a probe process.
fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path)
        .any(|dir| dir.join(name).is_file() || dir.join(format!("{name}.exe")).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The error messages ARE the UX: assert the remediation commands render.
    #[test]
    fn errors_render_actionable_commands() {
        let e = EngineError::ToolchainMissing {
            pin: "nightly-2026-04-16".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("rustup toolchain install nightly-2026-04-16"));
        assert!(msg.contains("--component rustc-dev"));
        assert!(msg.contains("--fast-only"));

        let e = EngineError::ComponentMissing {
            pin: "nightly-2026-04-16".into(),
            component: "rustc-dev".into(),
        };
        assert!(
            e.to_string()
                .contains("rustup component add rustc-dev --toolchain nightly-2026-04-16")
        );

        let e = EngineError::TargetMissing {
            pin: "nightly-2026-04-16".into(),
            triple: "wasm32-unknown-unknown".into(),
        };
        assert!(
            e.to_string().contains(
                "rustup target add wasm32-unknown-unknown --toolchain nightly-2026-04-16"
            )
        );

        let e = EngineError::DylintLinkMissing;
        assert!(e.to_string().contains("cargo install dylint-link --locked"));
    }

    /// `EngineError::remediation` must name the exact command the `Display`
    /// text shows — the CLI offers to *run* the former while the user *reads*
    /// the latter, so drift between them would execute something the user
    /// never saw. Both texts wrap commands over lines with `\`, so compare
    /// whitespace-collapsed.
    #[test]
    fn remediation_argv_matches_display_text() {
        let provisionable = [
            EngineError::ToolchainMissing {
                pin: "nightly-2026-04-16".into(),
            },
            EngineError::ComponentMissing {
                pin: "nightly-2026-04-16".into(),
                component: "rustc-dev".into(),
            },
            EngineError::TargetMissing {
                pin: "nightly-2026-04-16".into(),
                triple: "wasm32-unknown-unknown".into(),
            },
            EngineError::DylintLinkMissing,
        ];
        for e in provisionable {
            let argv = e.remediation().expect("preflight errors are provisionable");
            let command = argv.join(" ");
            let display: String = e
                .to_string()
                .replace('\\', " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                display.contains(&command),
                "Display text must show the remediation command verbatim:\n  {command}\nnot found in:\n  {display}"
            );
        }

        // Not provisionable: rustup itself is a shell pipe from the network.
        assert!(EngineError::RustupMissing.remediation().is_none());
    }

    #[test]
    fn dylint_link_probe_uses_path_scan() {
        // `cargo` is guaranteed to be on PATH in any environment running this
        // test; a nonsense name is guaranteed not to be.
        assert!(on_path("cargo"));
        assert!(!on_path("definitely-not-a-real-binary-xyzzy"));
    }
}
