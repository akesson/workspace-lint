//! The build-free fast tier's data layer.
//!
//! [`FastModel`] is the workspace view lints get *without compiling
//! anything*: the workspace shape from one `cargo metadata --no-deps` call,
//! each member's parsed [`Manifest`], and the lean syntactic module trees
//! walked by `module_tree` (module-file resolution, per-module `cfg_features`,
//! broken `mod` declarations, orphan files, and literal-`include!` splicing —
//! none of syn-workspace's resolver passes). It backs the pure-metadata
//! consumers (centralized-deps, crate-size, per-crate config leveling) and the
//! syntactic ones (feature-drift, module-tree, the directive scanner's parse
//! cache, the generated-file drop).
//!
//! A leaf crate: no engine, no diagnostics, no rustc anywhere near it.
//! Extracted from `wl-engine` when the two tiers outgrew one crate-size
//! budget; `wl-engine` re-exports it as `wl_engine::fast`, so consumers see
//! the same paths. Also hosts [`timing`] (the `WL_TIMING` phase instrument),
//! which both tiers and the binary share, [`shipped_source`] (the
//! `#[cfg(test)]`-aware shipped-line counter behind file-size / crate-size /
//! duplicate-code's test-mass exclusion), and [`clones`] (the name-invariant
//! Type-2 clone finder behind duplicate-code — a syntactic scanner like its
//! siblings; the *lint* stays in `wl-lints`).
//!
//! The manifest layer and the walker are salvaged from `syn-workspace`
//! (copied, not moved — the duplication is deliberate and disappears when
//! syn-workspace retires).

pub mod cfg_regions;
pub mod clones;
mod doc_fences;
mod include_resolve;
mod manifest;
mod metadata;
mod module_tree;
mod reach;
pub mod shipped_source;
pub mod source_measure;
mod syn_util;
pub mod timing;
mod types;

pub use manifest::{DeclaredDep, DepEntry, DepLocation, DepSection, Manifest, Publish};
pub use metadata::{CrateInfo, FastModel};
pub use source_measure::{MeasuredFile, SourceMeasure};
/// Re-export `toml_edit` so consumers can name [`toml_edit::Item`] and
/// friends at the exact version [`Manifest`] parses with.
pub use toml_edit;
pub use types::{Module, Target, TargetKind};

use std::path::PathBuf;

/// `path.canonicalize()`, falling back to the path itself when it doesn't
/// resolve — the set-comparison normalization every reach/orphan surface
/// shares (on macOS a `/tmp` fixture path and its `/private/tmp` real path
/// are the same file and must compare equal).
pub fn canonicalized(path: &std::path::Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub type Result<T> = std::result::Result<T, FastError>;

/// Errors produced while loading the fast-tier model.
///
/// Display texts mirror `syn_workspace::Error`'s equivalents so a consumer
/// switching between the two backends renders identical messages.
///
/// `#[non_exhaustive]`: new variants may be added in minor versions.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FastError {
    /// Failed to read or parse a `Cargo.toml`. The `source` chain holds
    /// the original I/O / parse error.
    #[error("manifest error in {}: {source}", path.display())]
    Manifest {
        path: PathBuf,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Failed to parse a Rust source file during the syntactic module-tree
    /// walk. The `source` is the original [`syn::Error`].
    #[error("parse error in {}: {source}", path.display())]
    Parse { path: PathBuf, source: syn::Error },
    /// Generic I/O error during the module-tree walk.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// `cargo metadata` itself failed (e.g. invalid workspace).
    #[error("cargo metadata: {0}")]
    Metadata(#[from] cargo_metadata::Error),
}

impl FastError {
    /// Convenience constructor for [`FastError::Manifest`]. Wraps the source
    /// error in a `Box`.
    pub(crate) fn manifest(
        path: impl Into<PathBuf>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Manifest {
            path: path.into(),
            source: Box::new(source),
        }
    }
}
