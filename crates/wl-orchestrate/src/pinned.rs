//! The pinned toolchain, resolved to its on-disk install — and the PATH lever
//! that makes every Phase-1 `cargo` spawn run *that* toolchain.
//!
//! Both Phase-1 builds spawn a bare `cargo` from PATH: our own dylib build,
//! and `dylint::run`'s wrapped `cargo check` (plus its driver provisioning).
//! They select the pinned nightly through rustup — the dylib build via the
//! extractor's `rust-toolchain.toml`, dylint by setting `RUSTUP_TOOLCHAIN` —
//! which only works if the `cargo` found on PATH is rustup's proxy. A version
//! manager's shim earlier on PATH breaks that: mise's `cargo` shim re-applies
//! the *analyzed workspace's* tool env, rewriting `RUSTUP_TOOLCHAIN=nightly…`
//! back to the project's pin (say `1.99.0`). Cargo then runs on 1.99, and
//! hands that `RUSTUP_TOOLCHAIN` to dylint's driver, which builds its
//! `--sysroot` as `$RUSTUP_HOME/toolchains/$RUSTUP_TOOLCHAIN` — a nightly
//! rustc against a 1.99 sysroot: `E0514 found crate std compiled by an
//! incompatible version of rustc` on the first workspace member.
//!
//! The fix is to stop going through anything resolvable: put the pinned
//! toolchain's own `bin/` first on PATH, so `cargo` (and the `rustc` cargo
//! spawns) ARE the pinned binaries, and state `RUSTUP_TOOLCHAIN` explicitly
//! where no rustup proxy is left to set it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::EngineError;

/// The pinned toolchain as installed: its full rustup name (with the host
/// triple — the form rustup proxies export and dylint keys its driver cache
/// on) and its `bin/` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PinnedToolchain {
    name: String,
    bin: PathBuf,
}

impl PinnedToolchain {
    /// Ask rustup where the pin's `cargo` lives. `--toolchain` is explicit,
    /// so neither an inherited `RUSTUP_TOOLCHAIN` nor a shimmed `rustup`
    /// can redirect the answer. Call after preflight: the pin is installed,
    /// so this never triggers rustup's auto-install.
    pub(super) fn resolve(pin: &str) -> Result<Self, EngineError> {
        let unresolved = |detail: String| EngineError::ToolchainUnresolved {
            pin: pin.into(),
            detail,
        };
        let output = Command::new("rustup")
            .args(["which", "--toolchain", pin, "cargo"])
            .output()
            .map_err(|_| EngineError::RustupMissing)?;
        if !output.status.success() {
            return Err(unresolved(
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let cargo = Path::new(stdout.trim());
        Self::from_cargo_path(cargo)
            .ok_or_else(|| unresolved(format!("unexpected cargo path {}", cargo.display())))
    }

    /// Parse rustup's `<RUSTUP_HOME>/toolchains/<name>/bin/cargo` layout.
    fn from_cargo_path(cargo: &Path) -> Option<Self> {
        let bin = cargo.parent()?;
        let name = bin.parent()?.file_name()?.to_str()?;
        (bin.file_name()? == "bin").then(|| Self {
            name: name.to_owned(),
            bin: bin.to_path_buf(),
        })
    }

    /// `PATH` with this toolchain's `bin/` in front of `inherited`.
    fn search_path(&self, inherited: Option<OsString>) -> OsString {
        // No inherited PATH ⇒ just `bin/`: splitting an empty value would
        // yield one empty entry, which Unix reads as the current directory.
        let rest: Vec<PathBuf> = inherited
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        let dirs = std::iter::once(self.bin.clone()).chain(rest);
        std::env::join_paths(dirs).expect("a rustup toolchain dir contains no PATH separator")
    }

    /// Pin one spawned command to this toolchain: its `bin/` first on PATH,
    /// and the `RUSTUP_TOOLCHAIN` a rustup proxy would have exported (the
    /// `dylint-link` linker wrapper names its output after it).
    pub(super) fn pin(&self, command: &mut Command) {
        command
            .env("PATH", self.search_path(std::env::var_os("PATH")))
            .env("RUSTUP_TOOLCHAIN", &self.name);
    }

    /// Pin the *process* PATH until the guard drops — the lever for spawns we
    /// don't construct (`dylint::run` has no per-spawn env hook; it sets
    /// `RUSTUP_TOOLCHAIN` itself). Same single-threaded contract as the other
    /// process-global effects of `Engine::extract`.
    pub(super) fn enter(&self) -> SearchPathGuard {
        let prev = std::env::var_os("PATH");
        let pinned = self.search_path(prev.clone());
        // SAFETY: single-threaded by the documented contract of `extract`.
        unsafe { std::env::set_var("PATH", pinned) };
        SearchPathGuard { prev }
    }
}

/// Restores the process PATH on drop (error paths included).
pub(super) struct SearchPathGuard {
    prev: Option<OsString>,
}

impl Drop for SearchPathGuard {
    fn drop(&mut self) {
        // SAFETY: single-threaded by the documented contract of `extract`.
        unsafe {
            match &self.prev {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rustup_toolchain_layout() {
        let cargo = Path::new(
            "/home/u/.rustup/toolchains/nightly-2026-04-16-aarch64-apple-darwin/bin/cargo",
        );
        let pinned = PinnedToolchain::from_cargo_path(cargo).unwrap();
        assert_eq!(pinned.name, "nightly-2026-04-16-aarch64-apple-darwin");
        assert_eq!(
            pinned.bin,
            Path::new("/home/u/.rustup/toolchains/nightly-2026-04-16-aarch64-apple-darwin/bin")
        );
    }

    #[test]
    fn rejects_paths_outside_a_bin_dir() {
        assert_eq!(
            PinnedToolchain::from_cargo_path(Path::new("/usr/local/cargo")),
            None
        );
        assert_eq!(PinnedToolchain::from_cargo_path(Path::new("cargo")), None);
    }

    /// The pinned `bin/` must come FIRST — a shim dir anywhere after it is
    /// then unreachable for `cargo`/`rustc` (the E0514 regression).
    #[test]
    fn search_path_puts_the_toolchain_bin_first() {
        let pinned = PinnedToolchain {
            name: "nightly-x".into(),
            bin: PathBuf::from("/tc/bin"),
        };
        let inherited = std::env::join_paths(["/shims", "/usr/bin"]).unwrap();
        let dirs: Vec<PathBuf> =
            std::env::split_paths(&pinned.search_path(Some(inherited))).collect();
        assert_eq!(
            dirs,
            [PathBuf::from("/tc/bin"), "/shims".into(), "/usr/bin".into()]
        );
        let alone: Vec<PathBuf> = std::env::split_paths(&pinned.search_path(None)).collect();
        assert_eq!(alone, [PathBuf::from("/tc/bin")]);
    }
}
