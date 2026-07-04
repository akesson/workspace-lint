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
mod deps;
mod join;
mod meta;
mod pub_usage;
mod removal;
mod union;

pub use assembly::{Assembly, Category, DefInfo, Reach, ResolvedRef};
pub use deps::{CrateDeps, DepUsage, DepsVerdict, NotJudged, UnusedDep};
pub use meta::{DepDecl, DepKind, WorkspaceMeta};
pub use pub_usage::{PubCandidate, PubUsage};
pub use removal::RemovalSet;
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

/// One `use`-declaration leaf left dangling by a cascade deletion: its target
/// item is being removed, so the import must go too or the build breaks
/// (E0432). The unused-pub `--fix` import-surgery surface
/// ([`SemanticModel::dangling_imports`]).
#[derive(Debug, Clone)]
pub struct DanglingImport {
    /// The leaf item's own span (workspace-relative file, on-disk byte range).
    /// For a **standalone** `use a::b;` this is the whole statement — the
    /// delete surface. For a **brace-list** leaf rustc collapses it to the
    /// leaf, so it equals `elem`; that equality is the brace discriminator the
    /// lint keys on (`decl == elem` ⇒ excise in place, else delete statement).
    pub decl: wl_ir::Span,
    /// The leaf as written (`b`, `b::c`, or `B as C`) — the intra-brace
    /// excision surface. Covers the whole brace entry, nested path and rename
    /// included.
    pub elem: wl_ir::Span,
    /// A `pub use` re-export: excising it can break a downstream name
    /// (E0364/E0365). Surgery skips it — and the target should never be a
    /// deletion candidate anyway (the candidate filter guards re-export
    /// targets), so this is belt-and-braces.
    pub reexport: bool,
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
        // The global hash join: one index over ALL configs' defs, shared by
        // every per-config assembly so a `+test`/bench/integration edge can
        // resolve a target extracted only into another config's dir.
        let ids = join::IdentityIndex::build(&configs);
        Ok(Self {
            configs: configs
                .into_iter()
                .map(|(id, frags)| (id, Assembly::build(frags, std::sync::Arc::clone(&ids))))
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

    /// Identities that must NOT be auto-deleted because a `use` naming them
    /// can't be excised: the declaration lives in a **macro expansion**
    /// (`decl_span.from_expansion`), so deleting the item would dangle an
    /// import no edit can reach (E0432). The cascade excludes these from its
    /// removable seeds up front, avoiding a mid-run un-delete ripple. (A
    /// `pub use` re-export target is already guarded at the candidate filter.)
    pub fn import_excision_blocked(&self) -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        for (_, asm) in &self.configs {
            for frag in asm.fragments() {
                for e in &frag.references {
                    if !e.import {
                        continue;
                    }
                    let blocked = e.decl_span.as_ref().is_some_and(|d| d.from_expansion);
                    if blocked && let Some(id) = asm.target_identity(e) {
                        out.insert(id.to_string());
                    }
                }
            }
        }
        out
    }

    /// Every `use`-declaration leaf whose target is one of the `removed` defs —
    /// the imports a cascade deletion leaves dangling (E0432 if not also
    /// removed). Unioned across configs and deduped by (file, decl, elem); the
    /// unused-pub `--fix` import-surgery surface. Macro-generated and
    /// `pub use` leaves are surfaced but flagged for the lint to skip.
    pub fn dangling_imports(&self, removed: &RemovalSet) -> Vec<DanglingImport> {
        use std::collections::HashSet;
        let mut seen: HashSet<(String, u32, u32)> = HashSet::new();
        let mut out = Vec::new();
        for (_, asm) in &self.configs {
            for frag in asm.fragments() {
                for e in &frag.references {
                    if !e.import {
                        continue;
                    }
                    let (Some(decl), Some(elem)) = (&e.decl_span, &e.elem_span) else {
                        continue;
                    };
                    let Some(id) = asm.target_identity(e) else {
                        continue;
                    };
                    if !removed.contains_id(id) {
                        continue;
                    }
                    if !seen.insert((decl.file.clone(), decl.lo, elem.lo)) {
                        continue;
                    }
                    out.push(DanglingImport {
                        decl: decl.clone(),
                        elem: elem.clone(),
                        reexport: e.reexport,
                    });
                }
            }
        }
        out
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
    // `crate_name` alone can tie — a package's bin may share the lib's crate
    // name — and read_dir order is OS-dependent, so break the tie on
    // target_kind to keep assembly deterministic.
    fragments.sort_by(|a, b| (&a.crate_name, &a.target_kind).cmp(&(&b.crate_name, &b.target_kind)));
    Ok(fragments)
}

#[cfg(test)]
mod tests;
