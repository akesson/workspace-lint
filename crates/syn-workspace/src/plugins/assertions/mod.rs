//! Tier-H usage assertions (`DESIGN-ir-pipeline.md` §13): built-in rules that,
//! when a syntactic *trigger* appears in source, contribute a [`Fact::Reference`].
//! An assertion encodes a *declared upstream contract* — "a `#[derive(EnumString)]`
//! expands to code that references `strum`" — evidence the resolver cannot reach by
//! parsing, because the referencing code only exists after macro expansion (and the
//! trigger itself often arrives through an external-crate glob like
//! `use wasm_bindgen_test::*;` the resolver can't see into). So triggers are matched
//! **syntactically**.
//!
//! Three ownership levels share one concept (the user-facing `[[macros.external]]`
//! config and in-source `expansion_uses!` are the other two); this module is the
//! built-in tier. Each rule cites the upstream contract it asserts — a rule that
//! can't is config, not a built-in.
//!
//! ## Shape: a shared engine, one plugin per crate
//!
//! [`UsageAssertion`] / [`Trigger`] are a small data table + a syntactic matcher
//! ([`scan`]). Each *crate* whose contract we encode gets its own one-file
//! [`ResolverPlugin`](crate::plugins::ResolverPlugin): [`strum::StrumPlugin`],
//! [`serde::SerdeWithPlugin`], [`wasm_bindgen::WasmBindgenTestPlugin`] — each holds its
//! rule and delegates matching to [`scan`]. [`builtin_assertions`] re-collects the rules
//! for introspection (a guarding-fixture test enumerates them).
//!
//! ## The FP-safe contract
//!
//! Each contributed [`Fact::Reference`] flows only into the crate-level reference sets
//! ([`crate::Workspace::references_from_crate`] / `referring_crates`), via the module's
//! `fact_references`, where it *suppresses* `unused-deps` / `unused-pub` false positives.
//! It can never create a finding: over-firing (e.g. crediting `strum` for a derive that
//! wasn't actually strum's) at worst fails to flag a genuinely unused dep — the same
//! direction the over-linking Phase B passes already commit to. Because these refs live
//! on `fact_references`, not in `occurrences`, they're absent from the SCIP projection
//! and [`crate::Module::references`], so the precision gate measures parsed evidence only.

use proc_macro2::{Literal, TokenTree};
use syn::punctuated::Punctuated;
use syn::visit::Visit;

use super::{ContributedRef, Fact, LocalFactCtx, Provenance};
use crate::resolve::ResolvedPath;
use crate::resolve::module_tree::signature::resolve_reference_path;

pub(crate) mod serde;
pub(crate) mod strum;
pub(crate) mod wasm_bindgen;

/// One built-in Tier-H rule. Every field is `'static`: built-ins ship as static
/// data (no code per rule). Lives with its owning crate's plugin (e.g. the `strum`
/// module's `STRUM`); [`builtin_assertions`] re-collects them.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct UsageAssertion {
    /// Stable, kebab-case rule id — embedded in each fact's provenance `rule` for a
    /// future `--explain`, and the
    /// name of the rule's guarding fixture
    /// (`tests/cases/unused-deps/true_negatives/asserted_<id_with_underscores>/`).
    pub id: &'static str,
    /// What in the source makes this rule fire.
    pub trigger: Trigger,
    /// Canonical paths (in-code form, `::`-separated) the trigger implies are
    /// referenced. Empty for [`Trigger::AttrStringValue`], whose implied refs
    /// come from the parsed string value instead.
    pub implies: &'static [&'static str],
    /// URL of the upstream documented contract this rule asserts.
    pub citation: &'static str,
}

/// How a [`UsageAssertion`] is triggered. All matching is syntactic.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum Trigger {
    /// Fires on `#[derive(X)]` when bare `X` is in `idents`, or on any
    /// path-qualified `c::X` whose crate `c` is in `crates` (any `X` — the
    /// qualifier alone proves the contract).
    DeriveIdent {
        idents: &'static [&'static str],
        crates: &'static [&'static str],
    },
    /// Fires on an attribute whose path's *last* segment is in `idents` (covers
    /// both `#[wasm_bindgen_test]` and a fully-qualified form).
    AttrPath { idents: &'static [&'static str] },
    /// Fires on `#[<attr>(<key> = "PATH", …)]` for any `key` in `keys`. The
    /// string value is parsed as a path and emitted as a reference, plus one
    /// `PATH::<child>` ref per entry in `children` (serde's `with` contract: the
    /// named module must expose `serialize` / `deserialize`).
    AttrStringValue {
        attr: &'static str,
        keys: &'static [&'static str],
        children: &'static [&'static str],
    },
}

/// The built-in Tier-H rule table, re-collected from the per-crate plugins for
/// introspection. See `DESIGN-ir-pipeline.md` §13.
pub fn builtin_assertions() -> &'static [UsageAssertion] {
    BUILTIN
}

static BUILTIN: &[UsageAssertion] = &[
    strum::STRUM,
    wasm_bindgen::WASM_BINDGEN_TEST,
    serde::SERDE_WITH,
];

/// Scan one top-level item's full attribute subtree (item attrs, field/variant
/// attrs, fn-body items) for `rule`'s trigger, returning a [`Fact::Reference`] per
/// implied / parsed path. `plugin` is the owning crate's id, carried into each
/// fact's [`Provenance`]. `syn::Item::Mod` is skipped — inline modules are scanned
/// by their own `collect_module_contents` pass, so recursing here would double-count.
pub(crate) fn scan(
    rule: &UsageAssertion,
    plugin: &'static str,
    item: &syn::Item,
    cx: &LocalFactCtx,
) -> Vec<Fact> {
    if matches!(item, syn::Item::Mod(_)) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut scanner = AssertionScan {
        rule,
        plugin,
        cx,
        out: &mut out,
    };
    scanner.visit_item(item);
    out
}

struct AssertionScan<'a, 'ctx> {
    rule: &'a UsageAssertion,
    plugin: &'static str,
    cx: &'a LocalFactCtx<'ctx>,
    out: &'a mut Vec<Fact>,
}

impl<'ast> Visit<'ast> for AssertionScan<'_, '_> {
    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        match_attribute(self.rule, self.plugin, attr, self.cx, self.out);
    }

    fn visit_item_mod(&mut self, _: &'ast syn::ItemMod) {
        // Inline modules are scanned by their own pass; don't descend.
    }
}

/// Match one attribute against `rule`, pushing reference facts for a fire.
fn match_attribute(
    rule: &UsageAssertion,
    plugin: &'static str,
    attr: &syn::Attribute,
    cx: &LocalFactCtx,
    out: &mut Vec<Fact>,
) {
    match &rule.trigger {
        Trigger::DeriveIdent { idents, crates } => {
            if attr.path().is_ident("derive") && derive_list_matches(attr, idents, crates) {
                emit_implies(rule.implies, &provenance(plugin, rule, attr, cx), cx, out);
            }
        }
        Trigger::AttrPath { idents } => {
            if attr_last_segment_in(attr, idents) {
                emit_implies(rule.implies, &provenance(plugin, rule, attr, cx), cx, out);
            }
        }
        Trigger::AttrStringValue {
            attr: name,
            keys,
            children,
        } => {
            if attr.path().is_ident(name) {
                let by = provenance(plugin, rule, attr, cx);
                for value in attr_string_paths(attr, keys) {
                    emit_string_path(&value, children, &by, cx, out);
                }
            }
        }
    }
}

/// This firing's [`Provenance`]: the owning crate, the rule id, and the trigger's
/// span (the attribute path's first ident).
fn provenance(
    plugin: &'static str,
    rule: &UsageAssertion,
    attr: &syn::Attribute,
    cx: &LocalFactCtx,
) -> Provenance {
    Provenance {
        plugin,
        rule: rule.id,
        trigger: attr
            .path()
            .segments
            .first()
            .and_then(|s| cx.span(s.ident.span())),
    }
}

/// True when the `#[derive(...)]` list contains a bare ident in `idents` or any
/// path-qualified `c::X` whose crate `c` is in `crates`.
fn derive_list_matches(attr: &syn::Attribute, idents: &[&str], crates: &[&str]) -> bool {
    let Ok(paths) = attr.parse_args_with(Punctuated::<syn::Path, syn::Token![,]>::parse_terminated)
    else {
        return false;
    };
    paths.iter().any(|p| match p.get_ident() {
        Some(id) => idents.contains(&id.to_string().as_str()),
        None => p
            .segments
            .first()
            .is_some_and(|s| crates.contains(&s.ident.to_string().as_str())),
    })
}

/// True when the attribute path's last segment is one of `idents`.
fn attr_last_segment_in(attr: &syn::Attribute, idents: &[&str]) -> bool {
    attr.path()
        .segments
        .last()
        .is_some_and(|s| idents.contains(&s.ident.to_string().as_str()))
}

/// Parse `<key> = "<path>"` entries (for any `key` in `keys`) out of an
/// attribute's `(...)` body, returning each value parsed as a [`syn::Path`].
/// A flat top-level token scan: nested groups (`bound(serialize = "…")`) are a
/// single `Group` token and never match the `ident = literal` window, so they're
/// skipped. Unparsable values are silently dropped — assertions never error.
fn attr_string_paths(attr: &syn::Attribute, keys: &[&str]) -> Vec<syn::Path> {
    let syn::Meta::List(list) = &attr.meta else {
        return Vec::new();
    };
    let tokens: Vec<TokenTree> = list.tokens.clone().into_iter().collect();
    let mut paths = Vec::new();
    for window in tokens.windows(3) {
        if let [
            TokenTree::Ident(key),
            TokenTree::Punct(eq),
            TokenTree::Literal(lit),
        ] = window
            && eq.as_char() == '='
            && keys.contains(&key.to_string().as_str())
            && let Some(value) = lit_str_value(lit)
            && let Ok(path) = syn::parse_str::<syn::Path>(&value)
        {
            paths.push(path);
        }
    }
    paths
}

/// The string a `Literal` token holds, if it is a string literal (`"routes"`
/// → `routes`); `None` for numeric / char / non-string literals.
fn lit_str_value(lit: &Literal) -> Option<String> {
    syn::parse_str::<syn::LitStr>(&lit.to_string())
        .ok()
        .map(|l| l.value())
}

/// Emit one reference fact per implied canonical path. Implies are
/// declared-canonical (absolute), so the target is built directly.
fn emit_implies(implies: &[&str], by: &Provenance, cx: &LocalFactCtx, out: &mut Vec<Fact>) {
    let from = crate_from(cx);
    for imp in implies {
        out.push(reference(
            from.clone(),
            ResolvedPath::from_user_str(imp),
            by.clone(),
        ));
    }
}

/// Emit the parsed value path plus one `path::<child>` per child. A path written
/// absolute (`::serde_with::…`) is the target verbatim; a relative one
/// (`with = "routes"`) is resolved against the module scope through the exact logic
/// ordinary references use, falling back to its raw segments.
fn emit_string_path(
    value: &syn::Path,
    children: &[&str],
    by: &Provenance,
    cx: &LocalFactCtx,
    out: &mut Vec<Fact>,
) {
    let segments: Vec<String> = value.segments.iter().map(|s| s.ident.to_string()).collect();
    if segments.is_empty() {
        return;
    }
    let absolute = value.leading_colon.is_some();
    let from = crate_from(cx);
    push_ref(segments.clone(), absolute, &from, by, cx, out);
    for child in children {
        let mut child_segs = segments.clone();
        child_segs.push((*child).to_string());
        push_ref(child_segs, absolute, &from, by, cx, out);
    }
}

fn push_ref(
    segments: Vec<String>,
    absolute: bool,
    from: &str,
    by: &Provenance,
    cx: &LocalFactCtx,
    out: &mut Vec<Fact>,
) {
    let to = if absolute {
        ResolvedPath::new(segments)
    } else {
        resolve_reference_path(segments, cx)
    };
    out.push(reference(from.to_string(), to, by.clone()));
}

fn reference(from: String, to: ResolvedPath, by: Provenance) -> Fact {
    Fact::Reference {
        edge: ContributedRef {
            from,
            to,
            // Asserted refs go to the package's own crate set, never the
            // sibling-target set (matches the pre-fold behavior).
            via_sibling_target: false,
        },
        by,
    }
}

/// The referencing crate's code-form name — the first segment of the enclosing
/// module's (crate-rooted) canonical path. Cosmetic for routing (the per-module
/// drain keys by the crate), but kept correct for the fact record.
fn crate_from(cx: &LocalFactCtx) -> String {
    cx.parent_canonical
        .segments()
        .first()
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;

    use super::*;
    use crate::resolve::use_tree::{Scope, UseBinding};

    /// Run `rule`'s scan over a single parsed item with a minimal (empty-scope)
    /// resolution context, returning the contributed facts. Relative paths resolve
    /// to their raw-segment fallback (no bindings/siblings to bind against) — the
    /// same `resolve_code_path` outcome the real walk produces for an unresolvable
    /// name.
    fn scan_src(rule: &UsageAssertion, plugin: &'static str, src: &str) -> Vec<Fact> {
        let item: syn::Item = syn::parse_str(src).expect("valid item");
        let scope = Scope {
            crate_name: "demo".to_string(),
            module_path: Vec::new(),
        };
        let siblings: HashSet<String> = HashSet::new();
        let use_bindings: Vec<UseBinding> = Vec::new();
        let parent = ResolvedPath::new(["demo".to_string()]);
        let cx = LocalFactCtx {
            scope: &scope,
            siblings: &siblings,
            use_bindings: &use_bindings,
            parent_canonical: &parent,
            file: Path::new("src/lib.rs"),
        };
        scan(rule, plugin, &item, &cx)
    }

    /// The rules a fact set fired, by id (deduped, sorted).
    fn rules(facts: &[Fact]) -> Vec<&'static str> {
        let mut ids: Vec<&'static str> = facts
            .iter()
            .map(|f| match f {
                Fact::Reference { by, .. } | Fact::Exposure { by, .. } => by.rule,
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// The target canonical segments of each contributed reference, in order.
    fn ref_targets(facts: &[Fact]) -> Vec<Vec<String>> {
        facts
            .iter()
            .map(|f| match f {
                Fact::Reference { edge, .. } => edge.to.segments().to_vec(),
                Fact::Exposure { .. } => panic!("non-reference fact emitted"),
            })
            .collect()
    }

    #[test]
    fn strum_bare_derive_ident_fires() {
        let facts = scan_src(
            &strum::STRUM,
            "strum",
            "#[derive(EnumString)]\npub enum E { A }",
        );
        assert_eq!(rules(&facts), ["strum-derive"]);
        assert_eq!(ref_targets(&facts), [vec!["strum".to_string()]]);
    }

    #[test]
    fn strum_qualified_derive_fires_on_any_ident() {
        // `strum::Display` is excluded as a bare ident, but the qualifier proves
        // the contract, so the `crates` arm fires.
        let facts = scan_src(
            &strum::STRUM,
            "strum",
            "#[derive(Debug, strum::Display)]\npub enum E { A }",
        );
        assert_eq!(rules(&facts), ["strum-derive"]);
    }

    #[test]
    fn bare_display_does_not_fire() {
        // Ambiguous with derive_more::Display — deliberately not covered.
        let facts = scan_src(
            &strum::STRUM,
            "strum",
            "#[derive(Debug, Display, Clone)]\npub enum E { A }",
        );
        assert!(facts.is_empty());
    }

    #[test]
    fn wasm_bindgen_test_attr_fires() {
        let facts = scan_src(
            &wasm_bindgen::WASM_BINDGEN_TEST,
            "wasm_bindgen",
            "#[wasm_bindgen_test]\nfn smoke() {}",
        );
        assert_eq!(rules(&facts), ["wasm-bindgen-test"]);
        assert_eq!(ref_targets(&facts), [vec!["wasm_bindgen".to_string()]]);
    }

    #[test]
    fn serde_with_relative_resolves_to_fallback() {
        let facts = scan_src(
            &serde::SERDE_WITH,
            "serde",
            "pub struct S {\n    #[serde(with = \"routes\")]\n    pub r: u8,\n}",
        );
        assert_eq!(rules(&facts), ["serde-with"]);
        // module + two children; with an empty scope the relative path resolves to
        // its raw-segment fallback (the same path the real walk would credit).
        assert_eq!(
            ref_targets(&facts),
            [
                vec!["routes".to_string()],
                vec!["routes".to_string(), "serialize".to_string()],
                vec!["routes".to_string(), "deserialize".to_string()],
            ]
        );
    }

    #[test]
    fn serde_with_absolute_path_credits_named_crate() {
        let facts = scan_src(
            &serde::SERDE_WITH,
            "serde",
            "pub struct S {\n    #[serde(with = \"::serde_with::rust::display_fromstr\")]\n    pub r: u8,\n}",
        );
        assert_eq!(rules(&facts), ["serde-with"]);
        // Leading `::` → absolute → target verbatim, crate name credits `serde_with`.
        assert_eq!(ref_targets(&facts)[0][0], "serde_with");
    }

    #[test]
    fn serde_crate_key_also_fires() {
        let facts = scan_src(
            &serde::SERDE_WITH,
            "serde",
            "#[serde(crate = \"my_serde\")]\npub struct S { pub a: u8 }",
        );
        assert_eq!(rules(&facts), ["serde-with"]);
        assert_eq!(ref_targets(&facts)[0], vec!["my_serde".to_string()]);
    }

    #[test]
    fn serde_unrelated_keys_do_not_fire() {
        let facts = scan_src(
            &serde::SERDE_WITH,
            "serde",
            "#[serde(rename_all = \"camelCase\")]\npub struct S { pub a: u8 }",
        );
        assert!(facts.is_empty());
    }

    #[test]
    fn serde_with_among_other_metas_is_found() {
        let facts = scan_src(
            &serde::SERDE_WITH,
            "serde",
            "pub struct S {\n    #[serde(default, with = \"routes\", skip_serializing_if = \"Option::is_none\")]\n    pub r: u8,\n}",
        );
        assert_eq!(rules(&facts), ["serde-with"]);
        assert!(ref_targets(&facts).contains(&vec!["routes".to_string()]));
    }

    #[test]
    fn unparsable_value_is_silently_skipped() {
        let facts = scan_src(
            &serde::SERDE_WITH,
            "serde",
            "pub struct S {\n    #[serde(with = \"not a path!!\")]\n    pub r: u8,\n}",
        );
        assert!(facts.is_empty());
    }

    #[test]
    fn inline_module_is_not_scanned_here() {
        // scan skips Item::Mod — the inner module is scanned by its own pass.
        let facts = scan_src(
            &wasm_bindgen::WASM_BINDGEN_TEST,
            "wasm_bindgen",
            "pub mod inner {\n    #[wasm_bindgen_test]\n    fn t() {}\n}",
        );
        assert!(facts.is_empty());
    }

    #[test]
    fn field_attribute_on_enum_variant_is_seen() {
        let facts = scan_src(
            &serde::SERDE_WITH,
            "serde",
            "pub enum E {\n    A {\n        #[serde(with = \"routes\")]\n        r: u8,\n    },\n}",
        );
        assert_eq!(rules(&facts), ["serde-with"]);
    }

    #[test]
    fn provenance_carries_plugin_and_rule() {
        let facts = scan_src(
            &strum::STRUM,
            "strum",
            "#[derive(EnumString)]\npub enum E { A }",
        );
        match &facts[0] {
            Fact::Reference { by, .. } => {
                assert_eq!(by.plugin, "strum");
                assert_eq!(by.rule, "strum-derive");
                assert!(by.trigger.is_some());
            }
            Fact::Exposure { .. } => panic!("expected a reference fact"),
        }
    }

    #[test]
    fn every_rule_has_kebab_id_and_http_citation() {
        let mut ids = HashSet::new();
        for rule in builtin_assertions() {
            assert!(ids.insert(rule.id), "duplicate rule id {}", rule.id);
            assert!(
                rule.id.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "rule id {} is not kebab-case",
                rule.id
            );
            assert!(
                rule.citation.starts_with("http"),
                "rule {} has no upstream citation",
                rule.id
            );
        }
    }
}
