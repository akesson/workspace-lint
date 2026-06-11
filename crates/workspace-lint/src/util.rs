//! Small cross-cutting helpers shared by more than one module.

/// A crate name with all `-`/`_` separators removed, for the FP-safe lib-name
/// fallback match (`md_5` collapses to `md5` to match a `md5` lib target).
///
/// Load-bearing in lockstep: `unused-deps` uses it to decide a dependency is
/// unused, and `deep::verify` uses the *same* normalization to decide whether
/// SCIP disproves that finding. If the two diverged, `--fix` could delete a dep
/// the verifier would have vouched for — so they share this single definition.
pub(crate) fn separator_stripped(name: &str) -> String {
    name.chars().filter(|c| *c != '-' && *c != '_').collect()
}
