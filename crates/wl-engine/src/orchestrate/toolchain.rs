//! Preflight: fail with the exact remediation *before* any build starts.
//!
//! The full tier needs (a) the pinned nightly with `rustc-dev` +
//! `llvm-tools-preview` (the dylib links `rustc_private`) and (b) the
//! `dylint-link` linker wrapper on PATH. All four checks are cheap
//! (subprocess-free where possible) and their failures render the exact
//! commands to run — see the `EngineError` variants' `Display`.

use std::process::Command;

use super::EngineError;

pub(super) fn preflight(pin: &str) -> Result<(), EngineError> {
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

        let e = EngineError::DylintLinkMissing;
        assert!(e.to_string().contains("cargo install dylint-link --locked"));
    }

    #[test]
    fn dylint_link_probe_uses_path_scan() {
        // `cargo` is guaranteed to be on PATH in any environment running this
        // test; a nonsense name is guaranteed not to be.
        assert!(on_path("cargo"));
        assert!(!on_path("definitely-not-a-real-binary-xyzzy"));
    }
}
