//! Load a `rust-analyzer scip` index and flatten it into the occurrence rows
//! deep verification matches against. We compare by **symbol**, never by byte
//! range, so position-encoding is irrelevant here (unlike the oracle harness);
//! only the line is retained, for the disproof report.

use std::path::Path;

use protobuf::Message;
use scip::types::{Index, SymbolRole};

use super::normalize::{NormalizedSymbol, normalize_symbol};

/// One package-bearing SCIP occurrence, normalized.
#[derive(Debug, Clone)]
pub(crate) struct Occurrence {
    /// Document path, relative to the SCIP `project_root` (the workspace root) —
    /// the same base the resolver's crate-relative paths use, so it maps to a
    /// member by manifest-dir prefix.
    pub file: String,
    /// 0-based line of the occurrence (for the disproof report). `None` if the
    /// range was malformed.
    pub line: Option<u32>,
    /// Normalized symbol (package code-name + canonical segments).
    pub symbol: NormalizedSymbol,
    /// `true` for a definition occurrence (so callers can exclude an item's own
    /// definition from "is it referenced?" tests).
    pub is_definition: bool,
}

/// A loaded, flattened SCIP index: every package-bearing occurrence across all
/// documents. Local symbols (no package) are dropped at load time.
pub(crate) struct ScipIndex {
    pub occurrences: Vec<Occurrence>,
}

impl ScipIndex {
    /// Parse the protobuf index at `path` and flatten it. Errors (unreadable
    /// file, undecodable protobuf, zero documents) are returned as strings for
    /// the caller to surface — a zero-document index means rust-analyzer failed
    /// to load the workspace, which must not pass silently.
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes =
            std::fs::read(path).map_err(|e| format!("read SCIP index {}: {e}", path.display()))?;
        let index = Index::parse_from_bytes(&bytes)
            .map_err(|e| format!("decode SCIP index {}: {e}", path.display()))?;
        if index.documents.is_empty() {
            return Err(format!(
                "SCIP index {} has no documents — rust-analyzer failed to load the workspace",
                path.display()
            ));
        }
        let def_role = SymbolRole::Definition as i32;
        let mut occurrences = Vec::new();
        for doc in &index.documents {
            for occ in &doc.occurrences {
                let Some(symbol) = normalize_symbol(&occ.symbol) else {
                    continue; // local symbol — not reference evidence
                };
                occurrences.push(Occurrence {
                    file: doc.relative_path.clone(),
                    line: occ.range.first().map(|l| *l as u32),
                    symbol,
                    is_definition: occ.symbol_roles & def_role != 0,
                });
            }
        }
        Ok(Self { occurrences })
    }
}
