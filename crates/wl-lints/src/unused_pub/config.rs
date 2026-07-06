use clap::ValueEnum;
use serde::Deserialize;

use crate::config::GlobPattern;

/// Item kinds the `unused-pub` lint can filter on. Shared by the `kinds`
/// config field and the CLI `--kinds` flag; an invalid value fails fast with
/// the valid set listed (so a typo like `method` — which isn't a modeled kind
/// — can't silently match nothing). `fn`/`mod` are accepted as aliases for
/// the keyword-minded.
#[derive(Deserialize, ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KindFilter {
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
    /// The rustc-IR backend's kind vocabulary (the extractor's stringified
    /// `DefKind`). `Module` is accepted for config compatibility but matches
    /// nothing — `mod` is a container, never an unused-pub candidate (same
    /// outcome as the syn backend's `is_definition` pre-filter).
    pub(crate) fn to_ir_kind(self) -> &'static str {
        match self {
            KindFilter::Function => "fn",
            KindFilter::Struct => "struct",
            KindFilter::Enum => "enum",
            KindFilter::Union => "union",
            KindFilter::Trait => "trait",
            KindFilter::Type => "type",
            KindFilter::Const => "const",
            KindFilter::Static => "static",
            KindFilter::Module => "mod",
            KindFilter::Macro => "macro",
        }
    }
}

#[derive(Deserialize, Default, Clone)]
pub struct UnusedPubConfig {
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
    /// "consider `pub`" suggestions.
    #[serde(default, rename = "suppress-intra-crate")]
    pub suppress_intra_crate: bool,
    /// When `true`, treat *every* crate as having an external public API:
    /// library-public items are exempt regardless of the crate's `publish`
    /// field (the conservative pre-publish-aware behavior). Default `false` —
    /// only crates that declare `publish = true` (or a registry list) are
    /// assumed to have out-of-workspace consumers; every other crate is treated
    /// as workspace-internal, so its `pub` items unused across the workspace are
    /// flagged. See the README's `unused-pub` section.
    #[serde(default, rename = "assume-all-public")]
    pub assume_all_public: bool,
    /// When a workspace-internal crate accumulates at least this many findings,
    /// emit one crate-level hint suggesting `publish = true` (in case the crate
    /// really is published). `Some(0)` disables the hint; `None` uses the
    /// built-in default (`DEFAULT_PUBLISH_HINT_THRESHOLD`).
    #[serde(default, rename = "publish-hint-threshold")]
    pub publish_hint_threshold: Option<usize>,
}

impl UnusedPubConfig {
    /// Build the config a `check unused-pub` CLI invocation resolves to.
    /// Shared by the lint construction and the `--fix-auto-delete` cascade
    /// (which needs the same config outside the boxed `Lint`).
    pub fn from_cli(
        exclude_crates: &[String],
        allowlist: &[String],
        kinds: &[KindFilter],
        exclude_paths: &[String],
        suppress_intra_crate: bool,
    ) -> Self {
        Self {
            exclude_crates: exclude_crates.to_vec(),
            allowlist: allowlist.iter().map(|p| GlobPattern::from_cli(p)).collect(),
            kinds: kinds.to_vec(),
            exclude_paths: exclude_paths
                .iter()
                .map(|p| GlobPattern::from_cli(p))
                .collect(),
            suppress_intra_crate,
            // Publish-awareness is config-only (no CLI flags): both live in the
            // project's config file. CLI single-lint runs keep the defaults.
            assume_all_public: false,
            publish_hint_threshold: None,
        }
    }
}
