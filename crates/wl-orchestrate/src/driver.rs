//! The `dylint::run` invocation of the embed flow — options, stderr capture,
//! and the loud re-provisioning fallback for dylint's silently-built driver.
//!
//! The wrapped `cargo check`'s stderr — compile progress, the extractor's
//! per-fragment notes, and any real compile diagnostics — goes to a log next
//! to the fragments, NOT the user's terminal: a successful run must stay
//! byte-deterministic for callers that snapshot stderr. On failure the log is
//! replayed verbatim (the compile errors ARE the diagnosis).
//!
//! That replay has a blind spot. `dylint::run` provisions a per-toolchain
//! driver binary on demand: a temp package depending on `dylint_driver`,
//! resolved fresh from crates.io (no lockfile) and compiled on the pinned
//! nightly. Under `quiet` — which the byte-determinism above requires — that
//! build's stderr goes to the null device inside dylint, so a failure there
//! (a registry hiccup, a dep release that breaks on the pin) surfaces as
//! `command failed: … cargo build` with an empty log and zero diagnostics
//! (observed: the 2026-08-03 scheduled corpus CI run). [`Invocation::run`]
//! closes the hole: an extraction failure with an empty log means the wrapped
//! cargo never started, so the failure was dylint's own provisioning — re-run
//! it with output inherited (determinism is already forfeit on a failing run)
//! and retry once. A persistent failure prints its compile errors; a
//! transient one heals the driver and the retry succeeds.

use std::path::{Path, PathBuf};

use crate::{ConfigSpec, EngineError, relink};

/// One config's `dylint::run` invocation: the dylib generation router, the
/// config's package/arg selection, and the stderr capture log.
pub(super) struct Invocation<'a> {
    dylib: &'a relink::RelinkedDylib,
    packages: &'a [String],
    selector: &'a ConfigSpec,
    log: PathBuf,
}

impl<'a> Invocation<'a> {
    pub(super) fn new(
        dylib: &'a relink::RelinkedDylib,
        packages: &'a [String],
        selector: &'a ConfigSpec,
        ir_dir: &Path,
    ) -> Self {
        Self {
            dylib,
            packages,
            selector,
            log: ir_dir.with_extension("log"),
        }
    }

    /// One `dylint::run`, with the silent-provisioning fallback documented on
    /// the module.
    pub(super) fn run(&self, what: &str) -> Result<(), EngineError> {
        let err = |source: anyhow::Error| EngineError::Extraction {
            config: format!("{} ({what})", self.selector.id),
            source,
        };
        let Err(first) = self.attempt() else {
            return Ok(());
        };
        if replay_log(&self.log) {
            return Err(err(first));
        }
        eprintln!(
            "workspace-lint: extraction failed before cargo produced any output ({first}) — \
             re-running dylint's driver provisioning with diagnostics visible"
        );
        reprovision_driver(self.dylib.canonical()).map_err(&err)?;
        self.attempt().map_err(|second| {
            replay_log(&self.log);
            err(second)
        })
    }

    /// One raw `dylint::run`: truncate the log (dylint appends), then derive
    /// the dylib path per invocation — after a bump, the re-run must hand
    /// dylint the NEW generation path or the `DYLINT_LIBS` env-dep channel
    /// never fires.
    fn attempt(&self) -> anyhow::Result<()> {
        let _ = std::fs::write(&self.log, b"");
        let opts = dylint_opts(
            &self.dylib.current(),
            self.packages,
            &self.selector.cargo_args(),
            &self.log,
        );
        dylint::run(&opts)
    }
}

/// Replay the piped extraction log to stderr; false when it had no content
/// (or was unreadable) — the signal that the wrapped cargo never started.
fn replay_log(log: &Path) -> bool {
    match std::fs::read_to_string(log) {
        Ok(captured) if !captured.is_empty() => {
            eprint!("{captured}");
            true
        }
        _ => false,
    }
}

/// Re-run dylint's driver provisioning for the dylib's toolchain with output
/// inherited, so its cargo errors land on the terminal (module doc has the
/// full why). A no-op — and silent — when the driver is present and current,
/// so a misdiagnosed empty log costs one quick version check, not a rebuild.
fn reprovision_driver(dylib: &Path) -> anyhow::Result<()> {
    use anyhow::Context as _;
    let toolchain = dylib_toolchain(dylib)
        .with_context(|| format!("no `@<toolchain>` in dylib name {}", dylib.display()))?;
    let loud = dylint::opts::Dylint::default();
    dylint::driver_builder::get(&loud, toolchain)
        .map(drop)
        .context("dylint driver provisioning failed (its cargo output is above)")
}

/// The `@<toolchain>` segment of the dylib filename — the same key
/// `dylint::run` itself resolves, naming the driver cache dir and the
/// driver package's `rust-toolchain` channel.
fn dylib_toolchain(dylib: &Path) -> Option<&str> {
    dylib
        .file_stem()?
        .to_str()?
        .split_once('@')
        .map(|(_, toolchain)| toolchain)
}

/// The `dylint::run` options of the embed flow: load exactly our dylib by
/// path, workspace members only, config selector forwarded to `cargo check`,
/// child stderr piped to `log` (surfaced only on failure).
fn dylint_opts(
    dylib: &Path,
    packages: &[String],
    cargo_args: &[String],
    log: &Path,
) -> dylint::opts::Dylint {
    use dylint::opts::{Check, Dylint, LibrarySelection, Operation};
    Dylint {
        pipe_stderr: Some(log.to_string_lossy().into_owned()),
        pipe_stdout: None,
        // dylint's own orchestrator-process chatter ("Checking with toolchain
        // `nightly-…-<host triple>`", the pipe-stderr experimental warning)
        // prints on OUR stderr, not the piped child's — and the triple makes
        // it platform-dependent. Callers snapshot stderr; keep it silent.
        quiet: true,
        operation: Operation::Check(Check {
            lib_sel: LibrarySelection {
                lib_paths: vec![dylib.to_string_lossy().into_owned()],
                ..Default::default()
            },
            no_deps: true,
            packages: packages.to_vec(),
            // An unscoped run must select EVERY member as a primary package.
            // In a non-virtual workspace (a root package with `[workspace]
            // members`, e.g. thiserror) a plain `cargo check` selects only
            // the root: the member crates compile as mere dependencies, whose
            // lint units dylint does not dylib-fingerprint — so their
            // fragments are never regenerated once cargo is warm, and the
            // completeness guard hard-fails. (This repo's own dogfood never
            // saw it: a virtual workspace's members are all primary.)
            workspace: packages.is_empty(),
            args: cargo_args.to_vec(),
            ..Default::default()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dylib_toolchain_is_the_at_segment() {
        assert_eq!(
            dylib_toolchain(Path::new(
                "/t/libwl_extractor@nightly-2026-04-16-aarch64-apple-darwin.dylib"
            )),
            Some("nightly-2026-04-16-aarch64-apple-darwin"),
        );
        // Windows naming: no `lib` prefix, `.dll` extension.
        assert_eq!(
            dylib_toolchain(Path::new(
                "wl_extractor@nightly-2026-04-16-x86_64-pc-windows-msvc.dll"
            )),
            Some("nightly-2026-04-16-x86_64-pc-windows-msvc"),
        );
        assert_eq!(dylib_toolchain(Path::new("/t/no_toolchain.dylib")), None);
    }

    #[test]
    fn replay_log_signals_content() {
        let tmp = tempfile::tempdir().unwrap();
        let log = tmp.path().join("default.log");
        assert!(!replay_log(&log), "an absent log reads as no content");
        std::fs::write(&log, "").unwrap();
        assert!(!replay_log(&log));
        std::fs::write(&log, "error[E0308]: mismatched types\n").unwrap();
        assert!(replay_log(&log));
    }
}
