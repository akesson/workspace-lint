//! Preflight: fail with the exact remediation *before* any build starts.
//!
//! The full tier needs (a) the pinned nightly with `rustc-dev` +
//! `llvm-tools-preview` (the dylib links `rustc_private`), (b) the
//! `dylint-link` linker wrapper on PATH, and (c) — for every `--target`
//! declared in the config matrix — that target's std installed for the pin
//! (the extraction `cargo check` runs on the pinned nightly). All checks are
//! cheap (subprocess-free where possible) and their failures render the exact
//! commands to run — see the `EngineError` variants' `Display`.
//!
//! Split as observe → judge: [`observe`] gathers one rustup-state snapshot,
//! [`gaps`] (pure) judges it. [`preflight`] surfaces the first gap;
//! [`missing`] surfaces them all — the substrate of
//! [`Engine::provision_plan`](super::Engine::provision_plan), which is why
//! the *complete* list matters and not just the first failure.

use std::collections::BTreeSet;
use std::process::Command;

use super::EngineError;

pub(super) fn preflight(pin: &str, triples: &BTreeSet<String>) -> Result<(), EngineError> {
    match missing(pin, triples)?.into_iter().next() {
        Some(gap) => Err(gap),
        None => Ok(()),
    }
}

/// Every provisioning gap between the local rustup state and what the full
/// tier needs, in repair order. The outer `Err` is rustup itself being absent
/// — with no rustup there is nothing to observe (and no safe remediation).
pub(super) fn missing(
    pin: &str,
    triples: &BTreeSet<String>,
) -> Result<Vec<EngineError>, EngineError> {
    Ok(gaps(pin, triples, &observe(pin, triples)?))
}

/// One snapshot of the local rustup/PATH state, as raw listing text — parsing
/// stays in [`gaps`] so the judgment is testable without a live rustup.
struct Observed {
    toolchain_installed: bool,
    /// `rustup component list --installed` output; `None` when the pin itself
    /// is absent (rustup can't list components of an uninstalled toolchain).
    components: Option<String>,
    /// `rustup target list --installed` output; `None` when the pin is absent
    /// or the matrix declares no `--target`.
    targets: Option<String>,
    dylint_link: bool,
}

fn observe(pin: &str, triples: &BTreeSet<String>) -> Result<Observed, EngineError> {
    let toolchains = rustup(&["toolchain", "list"])?;
    let toolchain_installed = toolchains.lines().any(|l| l.starts_with(pin));
    let components = toolchain_installed
        .then(|| rustup(&["component", "list", "--installed", "--toolchain", pin]))
        .transpose()?;
    let targets = (toolchain_installed && !triples.is_empty())
        .then(|| rustup(&["target", "list", "--installed", "--toolchain", pin]))
        .transpose()?;
    Ok(Observed {
        toolchain_installed,
        components,
        targets,
        dylint_link: on_path("dylint-link"),
    })
}

/// Pure judgment: the gaps one observed state leaves, in repair order. When
/// the pin itself is absent, the components are *not* separate gaps (the
/// install command already carries them) but every declared triple is (an
/// uninstalled toolchain has no target std, and rustup can't be asked) — so
/// a plan built from an absent pin is complete, not just its first step.
fn gaps(pin: &str, triples: &BTreeSet<String>, observed: &Observed) -> Vec<EngineError> {
    let mut gaps = Vec::new();
    if !observed.toolchain_installed {
        gaps.push(EngineError::ToolchainMissing { pin: pin.into() });
    }
    if let Some(components) = &observed.components {
        for component in ["rustc-dev", "llvm-tools"] {
            if !components.lines().any(|l| l.starts_with(component)) {
                gaps.push(EngineError::ComponentMissing {
                    pin: pin.into(),
                    component: component.into(),
                });
            }
        }
    }
    for triple in triples {
        let installed = observed
            .targets
            .as_ref()
            .is_some_and(|t| t.lines().any(|l| l.trim() == triple));
        if !installed {
            gaps.push(EngineError::TargetMissing {
                pin: pin.into(),
                triple: triple.clone(),
            });
        }
    }
    if !observed.dylint_link {
        gaps.push(EngineError::DylintLinkMissing);
    }
    gaps
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

    const PIN: &str = "nightly-2026-04-16";

    fn triples(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// A machine with everything installed judges to an empty plan.
    #[test]
    fn fully_provisioned_state_has_no_gaps() {
        let observed = Observed {
            toolchain_installed: true,
            components: Some(
                "rustc-dev-aarch64-apple-darwin\nllvm-tools-aarch64-apple-darwin".into(),
            ),
            targets: Some("aarch64-apple-darwin\nwasm32-unknown-unknown".into()),
            dylint_link: true,
        };
        assert!(gaps(PIN, &triples(&["wasm32-unknown-unknown"]), &observed).is_empty());
    }

    /// The fresh-CI-runner case (nothing installed): the plan must be
    /// complete in ONE observation — install (components ride along, so no
    /// separate component gaps), every declared target's std, dylint-link —
    /// so a consumer never hits the second-round-trip failure of running the
    /// first remediation and then discovering the next.
    #[test]
    fn absent_toolchain_plans_install_all_targets_and_linker() {
        let observed = Observed {
            toolchain_installed: false,
            components: None,
            targets: None,
            dylint_link: false,
        };
        let need = triples(&["aarch64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]);
        let plan = gaps(PIN, &need, &observed);
        let kinds: Vec<&str> = plan
            .iter()
            .map(|e| match e {
                EngineError::ToolchainMissing { .. } => "toolchain",
                EngineError::ComponentMissing { .. } => "component",
                EngineError::TargetMissing { .. } => "target",
                EngineError::DylintLinkMissing => "dylint-link",
                other => panic!("unexpected gap: {other}"),
            })
            .collect();
        assert_eq!(kinds, ["toolchain", "target", "target", "dylint-link"]);
    }

    /// An installed pin narrows the plan to exactly what's still missing.
    #[test]
    fn installed_toolchain_reports_only_missing_components_and_targets() {
        let observed = Observed {
            toolchain_installed: true,
            components: Some("llvm-tools-aarch64-apple-darwin".into()),
            targets: Some("aarch64-apple-darwin".into()),
            dylint_link: true,
        };
        let plan = gaps(PIN, &triples(&["wasm32-unknown-unknown"]), &observed);
        assert_eq!(plan.len(), 2, "one component + one target: {plan:?}");
        assert!(matches!(
            &plan[0],
            EngineError::ComponentMissing { component, .. } if component == "rustc-dev"
        ));
        assert!(matches!(
            &plan[1],
            EngineError::TargetMissing { triple, .. } if triple == "wasm32-unknown-unknown"
        ));
    }

    /// Every gap `gaps` can emit must carry a remediation argv — the
    /// provision plan maps gaps to commands unconditionally.
    #[test]
    fn every_gap_is_provisionable() {
        let observed = Observed {
            toolchain_installed: false,
            components: None,
            targets: None,
            dylint_link: false,
        };
        let plan = gaps(PIN, &triples(&["wasm32-unknown-unknown"]), &observed);
        assert!(!plan.is_empty());
        for gap in &plan {
            assert!(
                gap.remediation().is_some(),
                "gap without remediation: {gap}"
            );
        }
    }
}
