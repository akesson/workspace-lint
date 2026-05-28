use serde::Deserialize;

#[derive(Deserialize, Default, Clone)]
pub(crate) struct UnusedPubConfig {
    #[serde(default, rename = "exclude-crates")]
    pub exclude_crates: Vec<String>,
    #[serde(default)]
    pub allowlist: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default, rename = "exclude-paths")]
    pub exclude_paths: Vec<String>,
    /// When `true`, suppress the "only used inside the crate" variant and
    /// only emit findings for items with zero references anywhere. Default
    /// `false` (both variants reported). Useful on noisy codebases where the
    /// `pub`-everywhere convention would otherwise flood the report with
    /// "consider `pub(crate)`" suggestions.
    #[serde(default, rename = "suppress-intra-crate")]
    pub suppress_intra_crate: bool,
    /// When `true`, the structural fix for `appears unused — consider
    /// removing` becomes *item deletion* instead of `pub(crate)` narrowing.
    /// Guarded by a git-tracked-clean check: if the containing file is
    /// untracked or has uncommitted changes, the suggestion is downgraded
    /// to `MaybeIncorrect` and `--fix` skips it. Default `false`
    /// (visibility-narrow only — safer when the user can't recover via
    /// `git checkout`).
    #[serde(default, rename = "auto-delete")]
    pub auto_delete: bool,
}
