//! Phase 2: the semantic model — plain stable code over the extracted IR.
//!
//! Assembles per-config [`wl_ir::IrFragment`] sets into the workspace-global
//! view the semantic lints query: within each config a cross-crate join on
//! `DefPathHash`, across configs a union on the `(crate, def_path)` identity
//! (SPIKE §7 — neither key is stable on both axes, hence the two levels).
//! Verdict-producing queries return data; rendering belongs to the lints.

mod assembly;
mod deps;
mod meta;
mod union;

pub use assembly::{Assembly, Category, DefInfo, Reach};
pub use deps::{CrateDeps, DepsVerdict, NotJudged, UnusedDep};
pub use meta::{DepDecl, DepKind, WorkspaceMeta};
pub use union::{Lead, Retired, UnionVerdict};

use std::path::Path;

use wl_ir::IrFragment;

use crate::orchestrate::ExtractionRuns;

#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    #[error("no IR fragments found in {dir} — did extraction run?")]
    EmptyIrDir { dir: std::path::PathBuf },

    #[error("reading IR dir {dir}: {source}")]
    IrDir {
        dir: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("bad IR fragment {path}: {message}")]
    BadFragment {
        path: std::path::PathBuf,
        message: String,
    },

    #[error("reading cargo metadata for {dir}: {source}")]
    Metadata {
        dir: std::path::PathBuf,
        source: Box<cargo_metadata::Error>,
    },
}

/// The assembled semantic model: one [`Assembly`] per extracted config (first
/// = primary) plus the workspace metadata the verdicts classify against.
pub struct SemanticModel {
    configs: Vec<(String, Assembly)>,
    meta: WorkspaceMeta,
}

impl SemanticModel {
    /// Load every fragment dir of an extraction and assemble. The convenience
    /// entry point the binary uses: `Engine::extract(..)` → here.
    pub fn load(runs: &ExtractionRuns, workspace_root: &Path) -> Result<Self, SemanticError> {
        let meta = WorkspaceMeta::from_workspace(workspace_root)?;
        let mut configs = Vec::new();
        for run in &runs.runs {
            let fragments = load_fragments(&run.ir_dir)?;
            configs.push((run.id.clone(), fragments));
        }
        Self::assemble(configs, meta)
    }

    /// Assemble from in-memory fragments (the golden-fixture entry point).
    pub fn assemble(
        configs: Vec<(String, Vec<IrFragment>)>,
        meta: WorkspaceMeta,
    ) -> Result<Self, SemanticError> {
        assert!(
            !configs.is_empty(),
            "SemanticModel::assemble needs at least the primary config"
        );
        Ok(Self {
            configs: configs
                .into_iter()
                .map(|(id, frags)| (id, Assembly::build(frags)))
                .collect(),
            meta,
        })
    }

    /// The primary config's assembly (defines the candidate set).
    pub fn primary(&self) -> &Assembly {
        &self.configs[0].1
    }

    /// The config ids that ran, in matrix order — report these: silence reads
    /// as "all configs".
    pub fn config_ids(&self) -> impl Iterator<Item = &str> {
        self.configs.iter().map(|(id, _)| id.as_str())
    }

    pub fn meta(&self) -> &WorkspaceMeta {
        &self.meta
    }

    /// The cfg-matrix-unioned unused-pub verdict.
    pub fn union_verdict(&self) -> UnionVerdict {
        UnionVerdict::compute(&self.configs, Some(&self.meta))
    }

    /// The unused-deps verdict (declared deps vs the reference graph).
    pub fn deps_verdict(&self) -> DepsVerdict {
        DepsVerdict::compute(&self.configs, &self.meta)
    }
}

/// Load and schema-check every `*.json` fragment in one config's IR dir.
fn load_fragments(dir: &Path) -> Result<Vec<IrFragment>, SemanticError> {
    let entries = std::fs::read_dir(dir).map_err(|source| SemanticError::IrDir {
        dir: dir.to_path_buf(),
        source,
    })?;
    let mut fragments = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let bad = |message: String| SemanticError::BadFragment {
            path: path.clone(),
            message,
        };
        let text = std::fs::read_to_string(&path).map_err(|e| bad(e.to_string()))?;
        let frag: IrFragment = serde_json::from_str(&text).map_err(|e| bad(e.to_string()))?;
        // Skew detection: a stale or foreign-build fragment must fail the
        // run, not silently assemble alongside current-schema fragments.
        frag.check_schema().map_err(bad)?;
        fragments.push(frag);
    }
    if fragments.is_empty() {
        return Err(SemanticError::EmptyIrDir {
            dir: dir.to_path_buf(),
        });
    }
    fragments.sort_by(|a, b| a.crate_name.cmp(&b.crate_name));
    Ok(fragments)
}

#[cfg(test)]
mod tests;
