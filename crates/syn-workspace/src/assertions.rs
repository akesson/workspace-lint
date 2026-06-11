//! Tier-H usage assertions (`DESIGN-ir-pipeline.md` §13): built-in rules that,
//! when a syntactic *trigger* appears in source, emit reference occurrences
//! tagged [`Origin::Asserted`]. An assertion encodes a *declared upstream
//! contract* — "a `#[derive(EnumString)]` expands to code that references
//! `strum`" — evidence the resolver cannot reach by parsing, because the
//! referencing code only exists after macro expansion (and the trigger itself
//! often arrives through an external-crate glob like `use wasm_bindgen_test::*;`
//! the resolver can't see into). So triggers are matched **syntactically**.
//!
//! Three ownership levels share one concept (the user-facing `[[macros.external]]`
//! config and in-source `expansion_uses!` are the other two); this module is the
//! built-in tier. Each rule cites the upstream contract it asserts — a rule that
//! can't is config, not a built-in.
//!
//! ## The FP-safe contract
//!
//! Asserted refs flow only into the crate-level reference sets
//! ([`crate::Workspace::references_from_crate`] / `referring_crates`), where they
//! *suppress* `unused-deps` / `unused-pub` false positives. They can never
//! create a finding: over-firing (e.g. crediting `strum` for a derive that
//! wasn't actually strum's) at worst fails to flag a genuinely unused dep — the
//! same direction the over-linking Phase B passes already commit to. Asserted
//! occurrences are excluded from the SCIP projection and from
//! [`crate::Module::references`], so the precision gate measures parsed evidence
//! only.

use std::path::Path;

use proc_macro2::{Literal, TokenTree};
use syn::punctuated::Punctuated;
use syn::visit::Visit;

use crate::resolve::module_tree::span_to_source_span;
use crate::resolve::{Occurrence, Origin, ResolvedPath, SourceSpan};

/// One built-in Tier-H rule. Every field is `'static`: built-ins ship as a
/// static data table (no code per rule), which is also what keeps [`Origin`]
/// `Copy`.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct UsageAssertion {
    /// Stable, kebab-case rule id — embedded in `Origin::Asserted { rule }` for
    /// provenance, and the name of the rule's guarding fixture
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

/// The built-in Tier-H rule table. See `DESIGN-ir-pipeline.md` §13.
pub fn builtin_assertions() -> &'static [UsageAssertion] {
    BUILTIN
}

static BUILTIN: &[UsageAssertion] = &[
    // H1 — a strum derive expands to code that references the `strum` runtime
    // crate. `strum_macros` (the proc-macro crate) is credited separately by the
    // `use strum_macros::…` / `#[derive(strum_macros::…)]` that names it; only
    // `strum` is invisible to parsing. Distinctive idents only — `Display` and
    // `ToString` are deliberately omitted (a bare `#[derive(Display)]` is
    // ambiguous with `derive_more::Display`), so a crate that derives *only*
    // strum's `Display` unqualified won't be covered; the qualified
    // `#[derive(strum::Display)]` form still fires via `crates`.
    UsageAssertion {
        id: "strum-derive",
        trigger: Trigger::DeriveIdent {
            idents: &[
                "AsRefStr",
                "EnumCount",
                "EnumDiscriminants",
                "EnumIs",
                "EnumIter",
                "EnumMessage",
                "EnumProperty",
                "EnumString",
                "EnumTryAs",
                "EnumVariantNames", // legacy (pre-0.26) name of VariantNames.
                "FromRepr",
                "IntoStaticStr",
                "VariantArray",
                "VariantNames",
            ],
            crates: &["strum", "strum_macros"],
        },
        implies: &["strum"],
        // "This crate only contains derive macros for use with the strum crate."
        citation: "https://docs.rs/strum_macros/latest/strum_macros/",
    },
    // H1 — `#[wasm_bindgen_test]` expands to code requiring the `wasm-bindgen`
    // runtime. The attribute usually arrives via `use wasm_bindgen_test::*;`.
    UsageAssertion {
        id: "wasm-bindgen-test",
        trigger: Trigger::AttrPath {
            idents: &["wasm_bindgen_test"],
        },
        implies: &["wasm_bindgen"],
        citation: "https://rustwasm.github.io/wasm-bindgen/wasm-bindgen-test/usage.html",
    },
    // H2 — `#[serde(with = "mod")]` / `#[serde(crate = "mod")]` name a path in a
    // string literal that no scan would otherwise see. serde's `with` contract
    // requires the named module to expose `serialize` / `deserialize`, so credit
    // those children too (so `unused-pub` doesn't flag the helper fns).
    UsageAssertion {
        id: "serde-with",
        trigger: Trigger::AttrStringValue {
            attr: "serde",
            keys: &["with", "crate"],
            children: &["serialize", "deserialize"],
        },
        implies: &[],
        citation: "https://serde.rs/field-attrs.html#with",
    },
];

/// Scan one top-level item's full attribute subtree (item attrs, field/variant
/// attrs, fn-body items) for assertion triggers, appending `Origin::Asserted`
/// occurrences. `syn::Item::Mod` is skipped — inline modules are scanned by
/// their own `collect_module_contents` pass, so recursing here would
/// double-count.
pub(crate) fn scan_item(item: &syn::Item, file: &Path, out: &mut Vec<Occurrence>) {
    if matches!(item, syn::Item::Mod(_)) {
        return;
    }
    let mut scan = AssertionScan { file, out };
    scan.visit_item(item);
}

struct AssertionScan<'a> {
    file: &'a Path,
    out: &'a mut Vec<Occurrence>,
}

impl<'ast> Visit<'ast> for AssertionScan<'_> {
    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        match_attribute(attr, self.file, self.out);
    }

    fn visit_item_mod(&mut self, _: &'ast syn::ItemMod) {
        // Inline modules are scanned by their own pass; don't descend.
    }
}

/// Match one attribute against every built-in rule, emitting asserted
/// occurrences for each that fires.
fn match_attribute(attr: &syn::Attribute, file: &Path, out: &mut Vec<Occurrence>) {
    for rule in builtin_assertions() {
        match &rule.trigger {
            Trigger::DeriveIdent { idents, crates } => {
                if attr.path().is_ident("derive") && derive_list_matches(attr, idents, crates) {
                    emit_implies(rule.implies, attr_anchor(attr, file), rule.id, out);
                }
            }
            Trigger::AttrPath { idents } => {
                if attr_last_segment_in(attr, idents) {
                    emit_implies(rule.implies, attr_anchor(attr, file), rule.id, out);
                }
            }
            Trigger::AttrStringValue {
                attr: name,
                keys,
                children,
            } => {
                if attr.path().is_ident(name) {
                    let anchor = attr_anchor(attr, file);
                    for value in attr_string_paths(attr, keys) {
                        emit_string_path(&value, children, anchor.clone(), rule.id, out);
                    }
                }
            }
        }
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

/// Emit one asserted occurrence per implied canonical path. Implies are
/// declared-canonical, so the resolved `path` is pre-set (absolute) and the
/// Phase B resolver leaves it untouched.
fn emit_implies(
    implies: &[&str],
    span: Option<SourceSpan>,
    rule: &'static str,
    out: &mut Vec<Occurrence>,
) {
    for imp in implies {
        let path = ResolvedPath::from_user_str(imp);
        out.push(Occurrence {
            segments: path.segments().to_vec(),
            path: Some(path),
            span: span.clone(),
            origin: Origin::Asserted { rule },
        });
    }
}

/// Emit the asserted value path plus one `path::<child>` per child. A path
/// written absolute (`::serde_with::…`) is pre-resolved here; a relative one
/// (`with = "routes"`) keeps `path = None` and is resolved against the module's
/// scope in Phase B (`resolve_occurrence`'s `Asserted` arm).
fn emit_string_path(
    value: &syn::Path,
    children: &[&str],
    span: Option<SourceSpan>,
    rule: &'static str,
    out: &mut Vec<Occurrence>,
) {
    let segments: Vec<String> = value.segments.iter().map(|s| s.ident.to_string()).collect();
    if segments.is_empty() {
        return;
    }
    let absolute = value.leading_colon.is_some();
    push_asserted(segments.clone(), absolute, span.clone(), rule, out);
    for child in children {
        let mut child_segs = segments.clone();
        child_segs.push((*child).to_string());
        push_asserted(child_segs, absolute, span.clone(), rule, out);
    }
}

fn push_asserted(
    segments: Vec<String>,
    absolute: bool,
    span: Option<SourceSpan>,
    rule: &'static str,
    out: &mut Vec<Occurrence>,
) {
    let path = absolute.then(|| ResolvedPath::new(segments.clone()));
    out.push(Occurrence {
        segments,
        path,
        span,
        origin: Origin::Asserted { rule },
    });
}

/// Span to anchor an asserted occurrence at: the attribute path's first ident.
fn attr_anchor(attr: &syn::Attribute, file: &Path) -> Option<SourceSpan> {
    attr.path()
        .segments
        .first()
        .map(|s| span_to_source_span(file, s.ident.span()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scan a single parsed item's source, returning the asserted occurrences.
    fn scan(src: &str) -> Vec<Occurrence> {
        let item: syn::Item = syn::parse_str(src).expect("valid item");
        let mut out = Vec::new();
        scan_item(&item, Path::new("src/lib.rs"), &mut out);
        out
    }

    /// The rules an occurrence set fired, by id (deduped, sorted).
    fn rules(occs: &[Occurrence]) -> Vec<&'static str> {
        let mut ids: Vec<&'static str> = occs
            .iter()
            .map(|o| match o.origin {
                Origin::Asserted { rule } => rule,
                _ => panic!("non-asserted occurrence emitted"),
            })
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn segments_of(occs: &[Occurrence]) -> Vec<Vec<String>> {
        occs.iter().map(|o| o.segments.clone()).collect()
    }

    #[test]
    fn strum_bare_derive_ident_fires() {
        let occs = scan("#[derive(EnumString)]\npub enum E { A }");
        assert_eq!(rules(&occs), ["strum-derive"]);
        assert_eq!(segments_of(&occs), [vec!["strum".to_string()]]);
        // Implies are pre-resolved (absolute).
        assert!(occs[0].path.is_some());
    }

    #[test]
    fn strum_qualified_derive_fires_on_any_ident() {
        // `strum::Display` is excluded as a bare ident, but the qualifier proves
        // the contract, so the `crates` arm fires.
        let occs = scan("#[derive(Debug, strum::Display)]\npub enum E { A }");
        assert_eq!(rules(&occs), ["strum-derive"]);
    }

    #[test]
    fn bare_display_does_not_fire() {
        // Ambiguous with derive_more::Display — deliberately not covered.
        let occs = scan("#[derive(Debug, Display, Clone)]\npub enum E { A }");
        assert!(occs.is_empty());
    }

    #[test]
    fn wasm_bindgen_test_attr_fires() {
        let occs = scan("#[wasm_bindgen_test]\nfn smoke() {}");
        assert_eq!(rules(&occs), ["wasm-bindgen-test"]);
        assert_eq!(segments_of(&occs), [vec!["wasm_bindgen".to_string()]]);
    }

    #[test]
    fn serde_with_relative_emits_module_and_children_unresolved() {
        let occs = scan("pub struct S {\n    #[serde(with = \"routes\")]\n    pub r: u8,\n}");
        assert_eq!(rules(&occs), ["serde-with"]);
        // module + two children, all relative (path unresolved until Phase B).
        assert_eq!(
            segments_of(&occs),
            [
                vec!["routes".to_string()],
                vec!["routes".to_string(), "serialize".to_string()],
                vec!["routes".to_string(), "deserialize".to_string()],
            ]
        );
        assert!(occs.iter().all(|o| o.path.is_none()));
    }

    #[test]
    fn serde_with_absolute_path_is_preresolved() {
        let occs = scan(
            "pub struct S {\n    #[serde(with = \"::serde_with::rust::display_fromstr\")]\n    pub r: u8,\n}",
        );
        assert_eq!(rules(&occs), ["serde-with"]);
        // Leading `::` → absolute → path pre-set, crate name credits `serde_with`.
        assert!(occs.iter().all(|o| o.path.is_some()));
        assert_eq!(
            occs[0].path.as_ref().unwrap().crate_name(),
            Some("serde_with")
        );
    }

    #[test]
    fn serde_crate_key_also_fires() {
        let occs = scan("#[serde(crate = \"my_serde\")]\npub struct S { pub a: u8 }");
        assert_eq!(rules(&occs), ["serde-with"]);
        assert_eq!(occs[0].segments, vec!["my_serde".to_string()]);
    }

    #[test]
    fn serde_unrelated_keys_do_not_fire() {
        let occs = scan("#[serde(rename_all = \"camelCase\")]\npub struct S { pub a: u8 }");
        assert!(occs.is_empty());
    }

    #[test]
    fn serde_with_among_other_metas_is_found() {
        let occs = scan(
            "pub struct S {\n    #[serde(default, with = \"routes\", skip_serializing_if = \"Option::is_none\")]\n    pub r: u8,\n}",
        );
        assert_eq!(rules(&occs), ["serde-with"]);
        assert!(segments_of(&occs).contains(&vec!["routes".to_string()]));
    }

    #[test]
    fn unparsable_value_is_silently_skipped() {
        let occs = scan("pub struct S {\n    #[serde(with = \"not a path!!\")]\n    pub r: u8,\n}");
        assert!(occs.is_empty());
    }

    #[test]
    fn inline_module_is_not_scanned_here() {
        // scan_item skips Item::Mod — the inner module is scanned by its own pass.
        let occs = scan("pub mod inner {\n    #[wasm_bindgen_test]\n    fn t() {}\n}");
        assert!(occs.is_empty());
    }

    #[test]
    fn field_attribute_on_enum_variant_is_seen() {
        let occs = scan(
            "pub enum E {\n    A {\n        #[serde(with = \"routes\")]\n        r: u8,\n    },\n}",
        );
        assert_eq!(rules(&occs), ["serde-with"]);
    }

    #[test]
    fn every_rule_has_kebab_id_and_http_citation() {
        let mut ids = std::collections::HashSet::new();
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
