//! The PATH lever that makes every Phase-1 `cargo` spawn go through rustup's
//! proxies — the only `cargo` that honours the toolchain selection the engine
//! relies on.
//!
//! Every Phase-1 build spawns a bare `cargo` from PATH: our own dylib build,
//! `dylint::run`'s wrapped `cargo check`, and dylint's on-demand driver build.
//! Each selects its toolchain through rustup: the dylib build and the driver
//! build via a `rust-toolchain` file in their package dir, the extraction
//! check by setting `RUSTUP_TOOLCHAIN` to the pin. That only works when the
//! `cargo` found on PATH is rustup's proxy. A version manager's shim earlier
//! on PATH breaks it: mise's `cargo` shim re-applies the *analyzed
//! workspace's* tool env, rewriting `RUSTUP_TOOLCHAIN=nightly…` back to the
//! project's pin (say `1.99.0`). Cargo then runs on 1.99 and hands that
//! `RUSTUP_TOOLCHAIN` to dylint's driver, which builds its `--sysroot` as
//! `$RUSTUP_HOME/toolchains/$RUSTUP_TOOLCHAIN` — a nightly rustc against a
//! 1.99 sysroot: `E0514 found crate std compiled by an incompatible version
//! of rustc` on the first workspace member.
//!
//! So the proxy directory (`$CARGO_HOME/bin`) goes first on PATH. Not the
//! pinned toolchain's own `bin/`: bypassing the proxy also drops the
//! `RUSTUP_TOOLCHAIN` it exports to its children, which dylint's driver
//! build script requires (a cold driver provisioning then fails with
//! `environment variable not found: RUSTUP_TOOLCHAIN`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// rustup's proxy directory, when it can be located. `None` (no `rustup`
/// next to a `cargo` in `$CARGO_HOME/bin`) leaves PATH untouched — the
/// behaviour before this lever existed, so an unusual rustup install can't
/// regress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RustupProxies {
    bin: Option<PathBuf>,
}

impl RustupProxies {
    /// `$CARGO_HOME/bin` (default `~/.cargo/bin`) — where rustup installs its
    /// proxies — if it holds both `rustup` and `cargo`.
    pub(super) fn locate() -> Self {
        let cargo_home = std::env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::home_dir().map(|h| h.join(".cargo")));
        Self {
            bin: cargo_home.and_then(|home| Self::proxy_dir(&home.join("bin"))),
        }
    }

    fn proxy_dir(bin: &Path) -> Option<PathBuf> {
        let has =
            |name: &str| bin.join(name).is_file() || bin.join(format!("{name}.exe")).is_file();
        (has("rustup") && has("cargo")).then(|| bin.to_path_buf())
    }

    /// `PATH` with the proxy dir in front of `inherited` (unchanged when no
    /// proxy dir was found).
    fn search_path(&self, inherited: Option<OsString>) -> Option<OsString> {
        let bin = self.bin.as_ref()?;
        // No inherited PATH ⇒ just the proxy dir: splitting an empty value
        // would yield one empty entry, which Unix reads as the current dir.
        let rest: Vec<PathBuf> = inherited
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        let dirs = std::iter::once(bin.clone()).chain(rest);
        Some(std::env::join_paths(dirs).expect("a cargo home path contains no PATH separator"))
    }

    /// Route one spawned command's `cargo` through the proxies.
    pub(super) fn pin(&self, command: &mut Command) {
        if let Some(path) = self.search_path(std::env::var_os("PATH")) {
            command.env("PATH", path);
        }
    }

    /// Route the *process* PATH through the proxies until the guard drops —
    /// the lever for spawns we don't construct (`dylint::run` has no
    /// per-spawn env hook). Same single-threaded contract as the other
    /// process-global effects of `Engine::extract`.
    pub(super) fn enter(&self) -> SearchPathGuard {
        let prev = std::env::var_os("PATH");
        if let Some(path) = self.search_path(prev.clone()) {
            // SAFETY: single-threaded by the documented contract of `extract`.
            unsafe { std::env::set_var("PATH", path) };
        }
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

    fn proxies(bin: &str) -> RustupProxies {
        RustupProxies {
            bin: Some(PathBuf::from(bin)),
        }
    }

    /// The proxy dir must come FIRST — a shim dir anywhere after it is then
    /// unreachable for `cargo` (the E0514 regression).
    #[test]
    fn search_path_puts_the_proxy_dir_first() {
        let inherited = std::env::join_paths(["/shims", "/usr/bin"]).unwrap();
        let path = proxies("/home/u/.cargo/bin")
            .search_path(Some(inherited))
            .unwrap();
        let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        assert_eq!(
            dirs,
            [
                PathBuf::from("/home/u/.cargo/bin"),
                "/shims".into(),
                "/usr/bin".into()
            ]
        );
        let alone = proxies("/p").search_path(None).unwrap();
        assert_eq!(
            std::env::split_paths(&alone).collect::<Vec<_>>(),
            [PathBuf::from("/p")]
        );
    }

    #[test]
    fn no_proxy_dir_leaves_path_alone() {
        let none = RustupProxies { bin: None };
        assert_eq!(none.search_path(Some("/usr/bin".into())), None);
    }

    /// A dir counts as rustup's proxy dir only with BOTH `rustup` and
    /// `cargo` in it — a stray `cargo` (a shim dir, a distro package) is not.
    #[test]
    fn proxy_dir_requires_rustup_and_cargo() {
        let dir = std::env::temp_dir().join(format!("wl-proxies-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cargo"), b"").unwrap();
        assert_eq!(RustupProxies::proxy_dir(&dir), None);
        std::fs::write(dir.join("rustup"), b"").unwrap();
        assert_eq!(RustupProxies::proxy_dir(&dir), Some(dir.clone()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
