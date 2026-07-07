//! Cfg-gated source regions: which byte ranges of a member's source exist
//! only under some `#[cfg(...)]` predicate.
//!
//! The deletion veto's data layer (and the coverage audit's): rustc can never
//! see inside a cfg it didn't compile — cfg-stripping happens before HIR — so
//! the only tier that can observe cfg'd-out code is this syntactic one. syn
//! parses the full text regardless of cfg; the scan records, for every
//! cfg-carrying node, its on-disk byte range plus the parsed predicate.
//!
//! Coverage is deliberately broad (a missed region means a missed veto — the
//! unsafe direction): items, impl/trait/foreign members, statements, common
//! expression heads, match arms, fields, and enum variants. A `#[cfg] mod x;`
//! that resolves to a file shadows the **entire child subtree** (per-OS module
//! files are the dominant real-world layout). Deliberate misses, all
//! documented: `include!`-spliced content, `#[path]` under `cfg_attr`, and
//! macro-generated cfgs.

use std::path::{Path, PathBuf};

use proc_macro2::TokenTree;
use syn::spanned::Spanned;

use crate::module_tree::{dir_owning_children, resolve_mod_file_simple};

/// One cfg atom: `key = "value"` (`target_arch = "wasm32"`,
/// `feature = "gpu"`) or a bare flag (`test`, `unix`, `windows`, `miri`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfgAtom {
    KeyValue { key: String, value: String },
    Flag(String),
}

/// A parsed `cfg(...)` predicate tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CfgPredicate {
    Atom(CfgAtom),
    All(Vec<CfgPredicate>),
    Any(Vec<CfgPredicate>),
    Not(Box<CfgPredicate>),
    /// Unparseable — evaluates to Unknown (shadowed, the safe direction).
    Unknown,
}

/// One cfg-gated region: `[lo, hi)` byte range of `file` (on-disk offsets —
/// syn parses the raw text) existing only under `predicate`. A region from a
/// `#[cfg] mod x;` file subtree spans the whole child file.
#[derive(Debug, Clone)]
pub struct CfgRegion {
    pub file: PathBuf,
    pub lo: usize,
    pub hi: usize,
    pub predicate: CfgPredicate,
}

/// Scan one target root file (and every module file it reaches) for cfg
/// regions. `out` accumulates; parse failures skip the file (the fast tier's
/// walker already reports broken trees — the veto degrades to fewer vetoes).
pub fn scan_target(root_file: &Path, out: &mut Vec<CfgRegion>) {
    scan_file(root_file, &dir_owning_children(root_file), out, 0);
}

fn scan_file(file: &Path, mod_dir: &Path, out: &mut Vec<CfgRegion>, depth: usize) {
    if depth > 32 {
        return; // cyclic `#[path]` backstop
    }
    let Ok(source) = std::fs::read_to_string(file) else {
        return;
    };
    let Ok(parsed) = syn::parse_file(&source) else {
        return;
    };
    let mut v = RegionVisitor {
        file,
        mod_dir: mod_dir.to_path_buf(),
        out,
        depth,
    };
    // File-level `#![cfg(...)]` gates the whole file.
    for attr in &parsed.attrs {
        if let Some(pred) = cfg_predicate(attr) {
            v.out.push(CfgRegion {
                file: file.to_path_buf(),
                lo: 0,
                hi: source.len(),
                predicate: pred,
            });
        }
    }
    for item in &parsed.items {
        v.visit_item(item);
    }
}

struct RegionVisitor<'a> {
    file: &'a Path,
    mod_dir: PathBuf,
    out: &'a mut Vec<CfgRegion>,
    depth: usize,
}

impl RegionVisitor<'_> {
    fn record(&mut self, attrs: &[syn::Attribute], span: proc_macro2::Span) {
        for attr in attrs {
            if let Some(pred) = cfg_predicate(attr) {
                let range = span.byte_range();
                self.out.push(CfgRegion {
                    file: self.file.to_path_buf(),
                    lo: range.start,
                    hi: range.end,
                    predicate: pred,
                });
            }
        }
    }

    fn visit_item(&mut self, item: &syn::Item) {
        self.record(crate::module_tree::item_attrs(item), item.span());
        match item {
            syn::Item::Mod(m) => self.visit_mod(m),
            syn::Item::Fn(f) => self.visit_block(&f.block),
            syn::Item::Impl(i) => {
                for member in &i.items {
                    if let syn::ImplItem::Fn(f) = member {
                        self.record(&f.attrs, member.span());
                        self.visit_block(&f.block);
                    } else {
                        self.record(impl_item_attrs(member), member.span());
                    }
                }
            }
            syn::Item::Trait(t) => {
                for member in &t.items {
                    self.record(trait_item_attrs(member), member.span());
                    if let syn::TraitItem::Fn(f) = member
                        && let Some(body) = &f.default
                    {
                        self.visit_block(body);
                    }
                }
            }
            syn::Item::ForeignMod(fm) => {
                for member in &fm.items {
                    self.record(foreign_item_attrs(member), member.span());
                }
            }
            syn::Item::Struct(s) => {
                for f in &s.fields {
                    self.record(&f.attrs, f.span());
                }
            }
            syn::Item::Enum(e) => {
                for variant in &e.variants {
                    self.record(&variant.attrs, variant.span());
                }
            }
            _ => {}
        }
    }

    /// A cfg'd `mod x;` shadows its whole backing file subtree; an inline
    /// `mod x { … }` is covered by its item span. Child files are scanned
    /// either way (their own inner cfgs stand alone).
    fn visit_mod(&mut self, m: &syn::ItemMod) {
        if let Some((_, items)) = &m.content {
            let inner_dir = self.mod_dir.join(m.ident.to_string());
            let mut inner = RegionVisitor {
                file: self.file,
                mod_dir: inner_dir,
                out: self.out,
                depth: self.depth,
            };
            for item in items {
                inner.visit_item(item);
            }
            return;
        }
        let Some(child) = resolve_mod_file_simple(self.file, &self.mod_dir, m) else {
            return;
        };
        let gated = m.attrs.iter().any(|a| cfg_predicate(a).is_some());
        if gated {
            // Whole-subtree regions, one per descendant file, all under this
            // decl's predicate(s).
            for attr in &m.attrs {
                if let Some(pred) = cfg_predicate(attr) {
                    let mut files = Vec::new();
                    collect_subtree_files(&child, &mut files, 0);
                    for f in files {
                        let hi = std::fs::metadata(&f).map(|m| m.len() as usize).unwrap_or(0);
                        self.out.push(CfgRegion {
                            file: f,
                            lo: 0,
                            hi,
                            predicate: pred.clone(),
                        });
                    }
                }
            }
        }
        scan_file(
            &child,
            &dir_owning_children(&child),
            self.out,
            self.depth + 1,
        );
    }

    fn visit_block(&mut self, block: &syn::Block) {
        for stmt in &block.stmts {
            match stmt {
                syn::Stmt::Local(l) => self.record(&l.attrs, stmt.span()),
                syn::Stmt::Macro(m) => self.record(&m.attrs, stmt.span()),
                syn::Stmt::Item(item) => self.visit_item(item),
                syn::Stmt::Expr(e, _) => self.visit_expr(e, stmt.span()),
            }
        }
    }

    /// Statement/expression-position cfg — the dominant real-world shape
    /// (`#[cfg(target_arch = "wasm32")] { total += … }` inside a live fn).
    /// Common heads only; an exotic cfg'd expression is a documented miss.
    fn visit_expr(&mut self, e: &syn::Expr, span: proc_macro2::Span) {
        match e {
            syn::Expr::Block(x) => {
                self.record(&x.attrs, span);
                self.visit_block(&x.block);
            }
            syn::Expr::If(x) => {
                self.record(&x.attrs, span);
                self.visit_block(&x.then_branch);
                if let Some((_, else_branch)) = &x.else_branch {
                    self.visit_expr(else_branch, else_branch.span());
                }
            }
            syn::Expr::Match(x) => {
                self.record(&x.attrs, span);
                for arm in &x.arms {
                    self.record(&arm.attrs, arm.span());
                    self.visit_expr(&arm.body, arm.body.span());
                }
            }
            syn::Expr::Unsafe(x) => {
                self.record(&x.attrs, span);
                self.visit_block(&x.block);
            }
            syn::Expr::Loop(x) => {
                self.record(&x.attrs, span);
                self.visit_block(&x.body);
            }
            syn::Expr::While(x) => {
                self.record(&x.attrs, span);
                self.visit_block(&x.body);
            }
            syn::Expr::ForLoop(x) => {
                self.record(&x.attrs, span);
                self.visit_block(&x.body);
            }
            other => {
                // Every Expr variant carries attrs; cover the cfg without
                // recursing (calls, method calls, macros, …).
                self.record(expr_attrs(other), span);
            }
        }
    }
}

fn collect_subtree_files(file: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 32 {
        return;
    }
    out.push(file.to_path_buf());
    let Ok(source) = std::fs::read_to_string(file) else {
        return;
    };
    let Ok(parsed) = syn::parse_file(&source) else {
        return;
    };
    let mod_dir = dir_owning_children(file);
    for item in &parsed.items {
        if let syn::Item::Mod(m) = item
            && m.content.is_none()
            && let Some(child) = resolve_mod_file_simple(file, &mod_dir, m)
        {
            collect_subtree_files(&child, out, depth + 1);
        }
    }
}

/// Parse an attribute's cfg predicate: `#[cfg(<pred>)]` or
/// `#[cfg_attr(<pred>, …)]` (the condition side). `None` for non-cfg attrs.
pub fn cfg_predicate(attr: &syn::Attribute) -> Option<CfgPredicate> {
    let ident = attr.path().get_ident()?.to_string();
    if ident != "cfg" && ident != "cfg_attr" {
        return None;
    }
    let syn::Meta::List(list) = &attr.meta else {
        return Some(CfgPredicate::Unknown);
    };
    let tokens: Vec<TokenTree> = list.tokens.clone().into_iter().collect();
    // cfg_attr(<pred>, ...): the predicate is everything before the first
    // top-level comma.
    let pred_tokens: Vec<TokenTree> = if ident == "cfg_attr" {
        tokens
            .into_iter()
            .take_while(|t| !matches!(t, TokenTree::Punct(p) if p.as_char() == ','))
            .collect()
    } else {
        tokens
    };
    Some(parse_predicate(&pred_tokens))
}

fn parse_predicate(tokens: &[TokenTree]) -> CfgPredicate {
    match tokens {
        // `key = "value"`
        [
            TokenTree::Ident(key),
            TokenTree::Punct(eq),
            TokenTree::Literal(lit),
        ] if eq.as_char() == '=' => CfgPredicate::Atom(CfgAtom::KeyValue {
            key: key.to_string(),
            value: lit.to_string().trim_matches('"').to_string(),
        }),
        // bare flag: `test`, `unix`, `windows`, custom
        [TokenTree::Ident(flag)] => CfgPredicate::Atom(CfgAtom::Flag(flag.to_string())),
        // combinator: `any(...)` / `all(...)` / `not(...)`
        [TokenTree::Ident(comb), TokenTree::Group(g)] => {
            let args = split_top_level_commas(g.stream());
            let parsed: Vec<CfgPredicate> = args.iter().map(|a| parse_predicate(a)).collect();
            match comb.to_string().as_str() {
                "all" => CfgPredicate::All(parsed),
                "any" => CfgPredicate::Any(parsed),
                "not" if parsed.len() == 1 => {
                    CfgPredicate::Not(Box::new(parsed.into_iter().next().expect("len 1")))
                }
                _ => CfgPredicate::Unknown,
            }
        }
        _ => CfgPredicate::Unknown,
    }
}

fn split_top_level_commas(stream: proc_macro2::TokenStream) -> Vec<Vec<TokenTree>> {
    let mut out: Vec<Vec<TokenTree>> = vec![Vec::new()];
    for t in stream {
        if matches!(&t, TokenTree::Punct(p) if p.as_char() == ',') {
            out.push(Vec::new());
        } else {
            out.last_mut().expect("non-empty").push(t);
        }
    }
    out.retain(|v| !v.is_empty());
    out
}

fn impl_item_attrs(item: &syn::ImplItem) -> &[syn::Attribute] {
    match item {
        syn::ImplItem::Const(i) => &i.attrs,
        syn::ImplItem::Fn(i) => &i.attrs,
        syn::ImplItem::Type(i) => &i.attrs,
        syn::ImplItem::Macro(i) => &i.attrs,
        _ => &[],
    }
}

fn trait_item_attrs(item: &syn::TraitItem) -> &[syn::Attribute] {
    match item {
        syn::TraitItem::Const(i) => &i.attrs,
        syn::TraitItem::Fn(i) => &i.attrs,
        syn::TraitItem::Type(i) => &i.attrs,
        syn::TraitItem::Macro(i) => &i.attrs,
        _ => &[],
    }
}

fn foreign_item_attrs(item: &syn::ForeignItem) -> &[syn::Attribute] {
    match item {
        syn::ForeignItem::Fn(i) => &i.attrs,
        syn::ForeignItem::Static(i) => &i.attrs,
        syn::ForeignItem::Type(i) => &i.attrs,
        syn::ForeignItem::Macro(i) => &i.attrs,
        _ => &[],
    }
}

fn expr_attrs(e: &syn::Expr) -> &[syn::Attribute] {
    use syn::Expr::*;
    match e {
        Array(x) => &x.attrs,
        Assign(x) => &x.attrs,
        Async(x) => &x.attrs,
        Await(x) => &x.attrs,
        Binary(x) => &x.attrs,
        Break(x) => &x.attrs,
        Call(x) => &x.attrs,
        Cast(x) => &x.attrs,
        Closure(x) => &x.attrs,
        Continue(x) => &x.attrs,
        Field(x) => &x.attrs,
        Group(x) => &x.attrs,
        Index(x) => &x.attrs,
        Let(x) => &x.attrs,
        Lit(x) => &x.attrs,
        Macro(x) => &x.attrs,
        MethodCall(x) => &x.attrs,
        Paren(x) => &x.attrs,
        Path(x) => &x.attrs,
        Range(x) => &x.attrs,
        Reference(x) => &x.attrs,
        Repeat(x) => &x.attrs,
        Return(x) => &x.attrs,
        Struct(x) => &x.attrs,
        Try(x) => &x.attrs,
        TryBlock(x) => &x.attrs,
        Tuple(x) => &x.attrs,
        Unary(x) => &x.attrs,
        Yield(x) => &x.attrs,
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_src(src: &str) -> Vec<CfgRegion> {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("lib.rs");
        std::fs::write(&file, src).unwrap();
        let mut out = Vec::new();
        scan_target(&file, &mut out);
        out
    }

    fn kv(key: &str, value: &str) -> CfgPredicate {
        CfgPredicate::Atom(CfgAtom::KeyValue {
            key: key.into(),
            value: value.into(),
        })
    }

    #[test]
    fn item_level_cfg_records_region_and_predicate() {
        let src = "#[cfg(target_arch = \"wasm32\")]\npub fn only_wasm() {}\n\npub fn host() {}\n";
        let regions = scan_src(src);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].predicate, kv("target_arch", "wasm32"));
        let text = &src[regions[0].lo..regions[0].hi];
        assert!(text.contains("only_wasm"), "region text: {text}");
        assert!(!text.contains("host"));
    }

    #[test]
    fn statement_position_cfg_inside_a_live_fn_is_seen() {
        // The LeaveDates `utc_offset` shape: the *caller* is live; only the
        // call statement is cfg-gated.
        let src = "pub fn run() -> i32 {\n    let mut total = 0;\n    \
                   #[cfg(target_arch = \"wasm32\")]\n    {\n        total += tz_offset();\n    }\n    \
                   total\n}\n";
        let regions = scan_src(src);
        assert_eq!(regions.len(), 1, "{regions:?}");
        let text = &src[regions[0].lo..regions[0].hi];
        assert!(text.contains("tz_offset"), "region text: {text}");
    }

    #[test]
    fn combinators_parse_structurally() {
        let src =
            "#[cfg(all(unix, not(any(feature = \"a\", target_os = \"macos\"))))]\nfn f() {}\n";
        let regions = scan_src(src);
        assert_eq!(
            regions[0].predicate,
            CfgPredicate::All(vec![
                CfgPredicate::Atom(CfgAtom::Flag("unix".into())),
                CfgPredicate::Not(Box::new(CfgPredicate::Any(vec![
                    kv("feature", "a"),
                    kv("target_os", "macos"),
                ]))),
            ])
        );
    }

    #[test]
    fn cfgd_mod_decl_shadows_the_whole_child_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("lib.rs");
        std::fs::write(&root, "#[cfg(windows)]\nmod win;\n").unwrap();
        let win = tmp.path().join("win.rs");
        std::fs::write(&win, "pub fn registry_key() {}\nmod deep;\n").unwrap();
        std::fs::create_dir(tmp.path().join("win")).unwrap();
        let deep = tmp.path().join("win/deep.rs");
        std::fs::write(&deep, "pub fn deeper() {}\n").unwrap();

        let mut regions = Vec::new();
        scan_target(&root, &mut regions);
        // The decl's own item region (in lib.rs) + one whole-file region per
        // descendant file.
        let whole_files: Vec<&CfgRegion> = regions
            .iter()
            .filter(|r| !r.file.ends_with("lib.rs"))
            .collect();
        assert_eq!(whole_files.len(), 2, "{regions:?}");
        assert!(whole_files.iter().any(|r| r.file.ends_with("win.rs")));
        assert!(whole_files.iter().any(|r| r.file.ends_with("deep.rs")));
        assert!(
            whole_files
                .iter()
                .all(|r| r.predicate == CfgPredicate::Atom(CfgAtom::Flag("windows".into())))
        );
    }

    #[test]
    fn match_arms_fields_and_variants_are_covered() {
        let src = "pub struct S {\n    #[cfg(feature = \"x\")]\n    pub gated_field: i32,\n}\n\
                   pub enum E {\n    #[cfg(feature = \"y\")]\n    Gated,\n    Plain,\n}\n\
                   pub fn m(e: E) -> i32 {\n    match e {\n        \
                   #[cfg(feature = \"y\")]\n        E::Gated => gated_helper(),\n        _ => 0,\n    }\n}\n";
        let regions = scan_src(src);
        let preds: Vec<&CfgPredicate> = regions.iter().map(|r| &r.predicate).collect();
        assert_eq!(preds.len(), 3, "{regions:?}");
    }

    #[test]
    fn unknown_predicates_stay_unknown_not_dropped() {
        let src = "#[cfg(version(\"1.90\"))]\nfn f() {}\n";
        let regions = scan_src(src);
        assert_eq!(regions[0].predicate, CfgPredicate::Unknown);
    }
}
