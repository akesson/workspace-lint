//! Which files does a crate's *source text* name, and which `.rs` files exist?
//!
//! The two sets `orphan-file` needs from the build-free tier. Split out of the
//! [module walker](super::module_tree), which builds the tree; this derives the
//! reach query from it.
//!
//! The distinction that matters is *what a gap in each set costs*. The semantic
//! tier's `loaded_files` is rustc's own record of the files it opened and is the
//! only thing that decides a file is dead. [`declared_reach`] answers a
//! different, weaker question — *could some cfg have made rustc open this file?*
//! — and a `yes` turns "orphan, delete it" into "no declared config compiles
//! it". So a gap here promotes an honest finding into a destructive one, while a
//! gap in `loaded_files` merely loses a finding. That asymmetry is why
//! `resolve_mod_files` returns every `cfg_attr` arm and why `named_macro_files`
//! scans the whole syntax tree rather than item position.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::types::Target;

/// Every file this crate's source text names, canonicalized: each target's root,
/// each module's backing file, each `include!`-spliced file, and each file named
/// by an `include!` / `include_str!` / `include_bytes!` anywhere.
///
/// A cfg-agnostic over-approximation, deliberately. See the module docs.
pub(super) fn declared_reach(targets: &[Target]) -> HashSet<PathBuf> {
    let mut reached: HashSet<PathBuf> = HashSet::new();
    let mut insert = |path: &Path| {
        reached.insert(crate::canonicalized(path));
    };
    for target in targets {
        insert(&target.src_path);
        for module in target.all_modules() {
            insert(&module.file);
            // A file spliced in via `include!(...)` is reached even though it is
            // no module's own `file` (its items live in the including module).
            for gen_file in &module.generated_files {
                insert(gen_file);
            }
            for named in &module.named_files {
                insert(named);
            }
        }
    }
    reached
}

/// Every `.rs` file under `<manifest_dir>/src/` — the candidate set
/// `orphan-file` judges. Empty when the crate has no `src/` directory.
pub(super) fn src_rs_files(manifest_dir: &Path) -> Vec<PathBuf> {
    let src_dir = manifest_dir.join("src");
    if !src_dir.is_dir() {
        return Vec::new();
    }
    rs_files_under(&src_dir)
}

/// Recursively list `.rs` files under `dir`, excluding `target/` and
/// hidden directories.
fn rs_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let read = match std::fs::read_dir(&current) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if name.starts_with('.') || name == "target" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module_tree::test_support::build;
    use crate::types::TargetKind;

    /// The one-lib-target vector `declared_reach` takes.
    fn lib_targets(src: &Path) -> Vec<Target> {
        vec![Target {
            kind: TargetKind::Lib,
            name: "demo".into(),
            src_path: src.join("lib.rs"),
            root: build(src, "lib.rs"),
        }]
    }

    fn reaches(reach: &HashSet<PathBuf>, name: &str) -> bool {
        reach.iter().any(|p| p.ends_with(name))
    }

    #[test]
    fn declared_reach_covers_module_files_and_not_stale_ones() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "mod reached;\n").unwrap();
        std::fs::write(src.join("reached.rs"), "").unwrap();
        std::fs::write(src.join("stale.rs"), "").unwrap();

        let reach = declared_reach(&lib_targets(&src));
        assert!(reaches(&reach, "lib.rs"));
        assert!(reaches(&reach, "reached.rs"));
        assert!(
            !reaches(&reach, "stale.rs"),
            "nothing names stale.rs — it is the orphan the lint exists to find"
        );
        assert_eq!(src_rs_files(dir.path()).len(), 3);
    }

    /// The `memmap2`/`socket2`/`tempfile` platform idiom. Both arms name a real
    /// file and exactly one compiles; the walker evaluates no cfg, so it must
    /// reach both. Missing the untaken arm is what made `orphan-file` tell a mac
    /// developer to delete `imp_windows.rs`.
    #[test]
    fn declared_reach_covers_every_cfg_attr_path_arm() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "#[cfg_attr(unix, path = \"imp_unix.rs\")]\n\
             #[cfg_attr(windows, path = \"imp_windows.rs\")]\n\
             mod imp;\n",
        )
        .unwrap();
        std::fs::write(src.join("imp_unix.rs"), "mod deeper;\n").unwrap();
        std::fs::write(src.join("imp_windows.rs"), "").unwrap();
        std::fs::create_dir_all(src.join("imp_unix")).unwrap();
        std::fs::write(src.join("imp_unix/deeper.rs"), "").unwrap();

        let reach = declared_reach(&lib_targets(&src));
        assert!(reaches(&reach, "imp_unix.rs"));
        assert!(reaches(&reach, "imp_windows.rs"));
        // Walking every arm means each arm's own subtree is reached too.
        assert!(reaches(&reach, "deeper.rs"));
    }

    /// `include!` in expression position and `include_str!` in an initializer
    /// are invisible to the item walk. rustc reads both; calling either an
    /// orphan would advise deleting a compiled-in file.
    #[test]
    fn declared_reach_covers_macro_named_files_outside_item_position() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub static T: [u8; 1] = include!(\"table.rs\");\n\
             pub const S: &str = include_str!(\"snippet.rs\");\n\
             pub const B: &[u8] = include_bytes!(\"blob.rs\");\n\
             pub fn f() -> u32 { include!(\"expr.rs\") }\n",
        )
        .unwrap();
        for f in ["table.rs", "snippet.rs", "blob.rs", "expr.rs"] {
            std::fs::write(src.join(f), "0").unwrap();
        }

        let reach = declared_reach(&lib_targets(&src));
        for f in ["table.rs", "snippet.rs", "blob.rs", "expr.rs"] {
            assert!(reaches(&reach, f), "{f} must be reached");
        }
    }
}
