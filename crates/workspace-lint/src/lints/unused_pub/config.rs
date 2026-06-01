use clap::ValueEnum;
use serde::Deserialize;
use syn_workspace::ItemKind;

use crate::config::GlobPattern;

/// Item kinds the `unused-pub` lint can filter on, mapping to
/// [`syn_workspace::ItemKind`]. Shared by the `kinds` config field and the CLI
/// `--kinds` flag; an invalid value fails fast with the valid set listed (so a
/// typo like `method` — which isn't a modeled kind — can't silently match
/// nothing). `fn`/`mod` are accepted as aliases for the keyword-minded.
#[derive(Deserialize, ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum KindFilter {
    #[serde(alias = "fn")]
    #[value(alias = "fn")]
    Function,
    Struct,
    Enum,
    Union,
    Trait,
    Type,
    Const,
    Static,
    #[serde(alias = "mod")]
    #[value(alias = "mod")]
    Module,
    Macro,
}

impl KindFilter {
    pub fn to_item_kind(self) -> ItemKind {
        match self {
            KindFilter::Function => ItemKind::Fn,
            KindFilter::Struct => ItemKind::Struct,
            KindFilter::Enum => ItemKind::Enum,
            KindFilter::Union => ItemKind::Union,
            KindFilter::Trait => ItemKind::Trait,
            KindFilter::Type => ItemKind::TypeAlias,
            KindFilter::Const => ItemKind::Const,
            KindFilter::Static => ItemKind::Static,
            KindFilter::Module => ItemKind::Module,
            KindFilter::Macro => ItemKind::Macro,
        }
    }
}

#[derive(Deserialize, Default, Clone)]
pub(crate) struct UnusedPubConfig {
    #[serde(default, rename = "exclude-crates")]
    pub exclude_crates: Vec<String>,
    #[serde(default)]
    pub allowlist: Vec<GlobPattern>,
    #[serde(default)]
    pub kinds: Vec<KindFilter>,
    #[serde(default, rename = "exclude-paths")]
    pub exclude_paths: Vec<GlobPattern>,
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
