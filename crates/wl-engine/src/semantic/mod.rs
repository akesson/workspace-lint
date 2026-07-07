//! Phase 2: the semantic model — plain stable code over the extracted IR.
//!
//! Assembles per-config [`wl_ir::IrFragment`] sets into the workspace-global
//! view the semantic lints query: a cross-crate join on `DefPathHash` that is
//! **global across configs** (the config dirs share one cargo target dir, so a
//! hash names the same def everywhere — see the `join` module), then a
//! union on the `(crate, def_path)` identity to reduce the config matrix to a
//! verdict (SPIKE §7 — the hash is stable across configs, the identity across
//! crates, hence the two levels). Verdict-producing queries return data;
//! rendering belongs to the lints.

mod assembly;
mod clippy_guard;
mod collateral;
mod def_info;
mod deps;
mod imports;
mod join;
mod meta;
mod pub_usage;
mod removal;
mod store;
mod union;

pub use assembly::{Assembly, Category, DefInfo, Reach, ResolvedRef};
pub use clippy_guard::{DeletionUnmask, NarrowUnmask};
pub use collateral::PrivateOrphan;
pub use deps::{CrateDeps, DepUsage, DepsVerdict, NotJudged, UnusedDep};
pub use imports::{DanglingImport, ExcisionBlock};
pub use meta::{DepDecl, DepKind, WorkspaceMeta};
pub use pub_usage::{PubCandidate, PubUsage};
pub use removal::RemovalSet;
pub use union::{Lead, Retired, UnionVerdict};

use std::path::Path;

use crate::orchestrate::ExtractionRuns;
use store::FragmentBytes;

/// crate → index of the first config (declared order) whose lib/bin units
/// cover it: the crate's **home** config — the one entitled to judge its pub
/// API, define its display defs, and classify its external boundary. Under
/// the plain build+test matrix every lib's home is the primary config; a
/// crate only a `--target` config compiles (a wasm-only member) is judged by
/// that config instead of silently escaping judgment.
pub(crate) fn covering_configs(
    configs: &[(String, Assembly)],
) -> std::collections::BTreeMap<String, usize> {
    let mut covering = std::collections::BTreeMap::new();
    for (ci, (_, asm)) in configs.iter().enumerate() {
        for krate in asm.candidate_crates() {
            covering.entry(krate).or_insert(ci);
        }
    }
    covering
}

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
    pub fn load(runs: &ExtractionRuns) -> Result<Self, SemanticError> {
        let meta = WorkspaceMeta::from_metadata(&runs.metadata);
        let configs = crate::timing::phase("load_fragments[mmap all configs]", || {
            let mut configs = Vec::new();
            for run in &runs.runs {
                let fragments = load_fragments(&run.ir_dir)?;
                configs.push((run.id.clone(), fragments));
            }
            Ok::<_, SemanticError>(configs)
        })?;
        crate::timing::phase("assemble[join+per-config indexes]", || {
            Self::assemble_bytes(configs, meta)
        })
    }

    /// The assembly core over fragment byte stores, shared by production and
    /// tests so both see the one archived representation. `load` feeds mmap'd
    /// fragments straight in; the golden-fixture tests serialize native
    /// `IrFragment`s through `FragmentBytes::owned` and call this directly.
    fn assemble_bytes(
        configs: Vec<(String, Vec<FragmentBytes>)>,
        meta: WorkspaceMeta,
    ) -> Result<Self, SemanticError> {
        assert!(
            !configs.is_empty(),
            "SemanticModel::assemble needs at least the primary config"
        );
        // The global hash join: one index over ALL configs' defs, shared by
        // every per-config assembly so a `+test`/bench/integration edge can
        // resolve a target extracted only into another config's dir.
        let ids = join::IdentityIndex::build(&configs);
        // The per-config assemblies are independent (they share only the
        // read-only `IdentityIndex` via `Arc`), so build them concurrently —
        // one rayon task per `[engine] config`. `collect` into a `Vec`
        // preserves matrix order (primary first), which `primary()` relies on.
        use rayon::prelude::*;
        let configs: Vec<(String, Assembly)> = configs
            .into_par_iter()
            .map(|(id, frags)| (id, Assembly::build(frags, std::sync::Arc::clone(&ids))))
            .collect();
        Ok(Self { configs, meta })
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

    /// Every pub candidate with its cross-config usage classification, spans,
    /// and must-stay-`pub` guards — the query surface the ported `unused-pub`
    /// lint filters and renders. Candidates are restricted to the primary
    /// config's member crates; ordering is deterministic (by identity).
    pub fn pub_candidates(&self) -> Vec<PubCandidate> {
        pub_usage::compute(&self.configs)
    }

    /// [`pub_candidates`](Self::pub_candidates) recomputed as if every def in
    /// `removed` had been deleted — the unused-pub `--fix` cascade substrate.
    /// Deleting an item drops its outgoing edges, so a callee it solely reached
    /// re-classifies as `Unused` in the *same* pass. The cfg-matrix union is
    /// preserved (removal is simulated per config, then unioned exactly as the
    /// base verdict is), so an item still used under *any* configured
    /// `[engine] configs` entry stays alive. Returns candidates whose own
    /// identity is in `removed` too (they read `Unused`); the caller dedups.
    pub fn pub_candidates_excluding(&self, removed: &RemovalSet) -> Vec<PubCandidate> {
        if removed.is_empty() {
            return self.pub_candidates();
        }
        pub_usage::compute_excluding(&self.configs, removed)
    }

    /// The **private** defs left with no reaching edge once `removed`'s defs
    /// are gone — rustc `dead_code` would flag them on the fixed tree, so the
    /// unused-pub `--fix` cascade deletes them alongside the removal that
    /// caused it. Causality-gated and kind-limited; see [`PrivateOrphan`]
    /// and the `collateral` module docs.
    pub fn private_orphans(&self, removed: &RemovalSet) -> Vec<PrivateOrphan> {
        if removed.is_empty() {
            return Vec::new();
        }
        collateral::compute(&self.configs, removed)
    }

    /// Every reference out of `krate`'s primary-unit code under the
    /// **primary config** (architecture rules govern production layering;
    /// test cfg-variants and integration-test fragments are excluded), with
    /// canonical targets and module attribution.
    pub fn references_from(&self, krate: &str) -> Vec<ResolvedRef> {
        self.primary().references_from(krate)
    }

    /// The unused-deps verdict (declared deps vs the reference graph).
    pub fn deps_verdict(&self) -> DepsVerdict {
        DepsVerdict::compute(&self.configs, &self.meta)
    }

    /// The per-package exercised-crate sets — the primitive the ported
    /// `unused-deps` lint layers its manifest-driven judgement on.
    pub fn dep_usage(&self) -> DepUsage {
        DepUsage::compute(&self.configs, &self.meta)
    }
}

/// Mmap and header-validate every `*.wlir` fragment in one config's IR dir.
/// Header validation (magic + current schema + length) is the skew detection a
/// stale/foreign fragment must fail on — [`FragmentBytes::open`] rejects it so
/// the run errors instead of assembling alongside current-schema fragments.
fn load_fragments(dir: &Path) -> Result<Vec<FragmentBytes>, SemanticError> {
    let entries = std::fs::read_dir(dir).map_err(|source| SemanticError::IrDir {
        dir: dir.to_path_buf(),
        source,
    })?;
    let mut fragments = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wlir") {
            continue;
        }
        let frag = FragmentBytes::open(&path).map_err(|message| SemanticError::BadFragment {
            path: path.clone(),
            message,
        })?;
        fragments.push(frag);
    }
    if fragments.is_empty() {
        return Err(SemanticError::EmptyIrDir {
            dir: dir.to_path_buf(),
        });
    }
    // `crate_name` alone can tie — a package's bin may share the lib's crate
    // name — and read_dir order is OS-dependent, so break the tie on
    // target_kind to keep assembly deterministic. (Reading the archived header
    // fields is an O(1) cast per comparison.)
    fragments.sort_by(|a, b| {
        let (fa, fb) = (a.archived(), b.archived());
        (fa.crate_name.as_str(), fa.target_kind.as_str())
            .cmp(&(fb.crate_name.as_str(), fb.target_kind.as_str()))
    });
    Ok(fragments)
}

#[cfg(test)]
mod tests;
