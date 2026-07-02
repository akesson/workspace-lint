//! The build-free fast tier's data layer.
//!
//! [`FastModel`] is the workspace view lints get *without compiling
//! anything*: the workspace shape from one `cargo metadata --no-deps` call
//! plus each member's parsed [`Manifest`]. It backs the pure-metadata
//! consumers (centralized-deps, crate-size, per-crate config leveling); the
//! syntactic module-tree walker joins this tier in a later migration PR.
//!
//! The manifest layer is salvaged from `syn-workspace` (copied, not moved —
//! the duplication is deliberate and disappears when syn-workspace retires).

mod manifest;
mod metadata;

pub use manifest::{DeclaredDep, DepLocation, DepSection, Manifest, Publish};
pub use metadata::{CrateInfo, FastModel};
/// Re-export `toml_edit` so consumers can name [`toml_edit::Item`] and
/// friends at the exact version [`Manifest`] parses with.
pub use toml_edit;

use std::path::PathBuf;

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
    /// `cargo metadata` itself failed (e.g. invalid workspace).
    #[error("cargo metadata: {0}")]
    Metadata(#[from] cargo_metadata::Error),
}

impl FastError {
    /// Convenience constructor for [`FastError::Manifest`]. Wraps the source
    /// error in a `Box`.
    pub fn manifest(
        path: impl Into<PathBuf>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Manifest {
            path: path.into(),
            source: Box::new(source),
        }
    }
}
