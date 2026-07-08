//! Lazy per-file syntactic metadata for the refactoring classifier.
//!
//! The classifier needs to name the fix a clone group calls for, which takes
//! more than the group's normalized tokens: whether the instances are free
//! fns, inherent methods, or trait-impl methods; each fn's name and its first
//! parameter's base type; which macros a region invokes; and every type the
//! workspace defines. None of that feeds the fingerprint — recovering it here,
//! *after* grouping, keeps the hot [`super::Collect`] walker untouched (the
//! same discipline [`super::capture`] follows for concrete literals).
//!
//! One walk per file that actually holds a reported instance, cached. A fn is
//! keyed by its signature-start `(line, column)` — exactly the anchor
//! [`super::Collect::candidate_fn`] records as a `Fn` region's
//! `(line_start, col_start)` — so [`MetaResolver::fn_meta`] is a direct lookup.
//! [`FnMeta::byte_offset`] is the on-disk byte the semantic tier's
//! `enclosing_fn` resolves against (proc-macro2's `byte_range` and the IR's
//! spans are both on-disk offsets — see [`wl_ir::Span`]).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use proc_macro2::LineColumn;
use syn::spanned::Spanned;
use syn::visit::{self, Visit};

use super::{Region, ScanFile};

/// Where a fn lives — the shape that decides which refactoring names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FnOwner {
    /// A free fn (module-level or fn-local).
    Free,
    /// An inherent-impl method (`impl Ty { fn … }`).
    Inherent { self_ty: String },
    /// A trait-impl method (`impl Tr for Ty { fn … }`).
    TraitImpl { self_ty: String, trait_path: String },
    /// A trait-declaration method with a default body (`trait Tr { fn …{} }`).
    TraitDecl { trait_path: String },
}

/// A fn's first parameter, normalized to what the classifier judges: a
/// `self` receiver, or a by-path type reduced to its base name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FirstParam {
    /// `self` / `&self` / `&mut self`.
    Receiver,
    /// A typed parameter; `base` is the type's final path segment with `&`/
    /// `&mut` peeled and generics dropped (`&mut Foo<u32>` ⇒ `Foo`). A
    /// non-path shape (tuple, slice, …) yields `""`.
    Typed { base: String },
}

/// The syntactic facts about one fn a clone instance resolves to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FnMeta {
    pub fn_name: String,
    pub owner: FnOwner,
    pub first_param: Option<FirstParam>,
    /// On-disk byte offset of the signature start — the `enclosing_fn` key.
    pub byte_offset: u32,
}

/// Lazily-resolved per-file syntax, built once per file on first use (mirrors
/// [`super::capture::LiteralTables`]).
pub struct MetaResolver<'a> {
    asts: HashMap<&'a Path, &'a syn::File>,
    fns: HashMap<PathBuf, HashMap<(u32, u32), FnMeta>>,
    macros: HashMap<PathBuf, Vec<(String, LineColumn)>>,
    type_names: Option<BTreeSet<String>>,
}

impl<'a> MetaResolver<'a> {
    pub fn new(files: &'a [ScanFile]) -> Self {
        Self {
            asts: files
                .iter()
                .map(|f| (f.rel_path.as_path(), &f.ast))
                .collect(),
            fns: HashMap::new(),
            macros: HashMap::new(),
            type_names: None,
        }
    }

    /// The fn a `Fn`-kind region anchors on, by its `(line_start, col_start)`
    /// signature anchor. `None` for a non-fn region, or a file not scanned.
    pub fn fn_meta(&mut self, region: &Region) -> Option<FnMeta> {
        self.ensure_fns(&region.file);
        self.fns
            .get(&region.file)?
            .get(&(region.line_start, region.col_start))
            .cloned()
    }

    /// The distinct macro names invoked inside `region`. A group's instances
    /// are structurally identical, so any one instance's set represents the
    /// group — the classifier passes the anchor.
    pub fn macros_in(&mut self, region: &Region) -> BTreeSet<String> {
        self.ensure_macros(&region.file);
        let Some(table) = self.macros.get(&region.file) else {
            return BTreeSet::new();
        };
        let (start, end) = region.bounds();
        table
            .iter()
            .filter(|(_, pos)| *pos >= start && *pos < end)
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Every type name the scanned workspace defines (struct / enum / union /
    /// type alias) — the workspace-local test for `MethodOnReceiverType`.
    /// Built once across all files.
    pub fn defined_type_names(&mut self) -> &BTreeSet<String> {
        self.type_names.get_or_insert_with(|| {
            let mut w = TypeWalker {
                names: BTreeSet::new(),
            };
            for ast in self.asts.values() {
                w.visit_file(ast);
            }
            w.names
        })
    }

    fn ensure_fns(&mut self, file: &Path) {
        if self.fns.contains_key(file) {
            return;
        }
        let table = match self.asts.get(file) {
            Some(ast) => {
                let mut w = FnWalker {
                    ctx: Vec::new(),
                    out: HashMap::new(),
                };
                w.visit_file(ast);
                w.out
            }
            None => HashMap::new(),
        };
        self.fns.insert(file.to_path_buf(), table);
    }

    fn ensure_macros(&mut self, file: &Path) {
        if self.macros.contains_key(file) {
            return;
        }
        let table = match self.asts.get(file) {
            Some(ast) => {
                let mut w = MacroWalker { out: Vec::new() };
                w.visit_file(ast);
                w.out
            }
            None => Vec::new(),
        };
        self.macros.insert(file.to_path_buf(), table);
    }
}

/// The enclosing impl/trait as a fn is visited — the `FnOwner` context stack.
enum OwnerCtx {
    Impl {
        self_ty: String,
        trait_path: Option<String>,
    },
    Trait {
        trait_path: String,
    },
}

struct FnWalker {
    ctx: Vec<OwnerCtx>,
    out: HashMap<(u32, u32), FnMeta>,
}

impl FnWalker {
    fn record(&mut self, sig: &syn::Signature, owner: FnOwner) {
        let start = sig.span().start();
        let key = (start.line as u32, start.column as u32);
        self.out.insert(
            key,
            FnMeta {
                fn_name: sig.ident.to_string(),
                owner,
                first_param: first_param(sig),
                byte_offset: sig.span().byte_range().start as u32,
            },
        );
    }
}

impl<'ast> Visit<'ast> for FnWalker {
    fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
        self.record(&f.sig, FnOwner::Free);
        visit::visit_item_fn(self, f);
    }

    fn visit_item_impl(&mut self, i: &'ast syn::ItemImpl) {
        self.ctx.push(OwnerCtx::Impl {
            self_ty: type_base(&i.self_ty),
            trait_path: i.trait_.as_ref().map(|(_, path, _)| path_string(path)),
        });
        visit::visit_item_impl(self, i);
        self.ctx.pop();
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let owner = match self.ctx.last() {
            Some(OwnerCtx::Impl {
                self_ty,
                trait_path: None,
            }) => FnOwner::Inherent {
                self_ty: self_ty.clone(),
            },
            Some(OwnerCtx::Impl {
                self_ty,
                trait_path: Some(tp),
            }) => FnOwner::TraitImpl {
                self_ty: self_ty.clone(),
                trait_path: tp.clone(),
            },
            _ => FnOwner::Free,
        };
        self.record(&f.sig, owner);
        visit::visit_impl_item_fn(self, f);
    }

    fn visit_item_trait(&mut self, t: &'ast syn::ItemTrait) {
        self.ctx.push(OwnerCtx::Trait {
            trait_path: t.ident.to_string(),
        });
        visit::visit_item_trait(self, t);
        self.ctx.pop();
    }

    fn visit_trait_item_fn(&mut self, f: &'ast syn::TraitItemFn) {
        let trait_path = match self.ctx.last() {
            Some(OwnerCtx::Trait { trait_path }) => trait_path.clone(),
            _ => String::new(),
        };
        self.record(&f.sig, FnOwner::TraitDecl { trait_path });
        visit::visit_trait_item_fn(self, f);
    }
}

struct MacroWalker {
    out: Vec<(String, LineColumn)>,
}

impl<'ast> Visit<'ast> for MacroWalker {
    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        if let Some(seg) = m.path.segments.last() {
            self.out
                .push((seg.ident.to_string(), m.path.span().start()));
        }
        visit::visit_macro(self, m);
    }
}

struct TypeWalker {
    names: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for TypeWalker {
    fn visit_item_struct(&mut self, i: &'ast syn::ItemStruct) {
        self.names.insert(i.ident.to_string());
        visit::visit_item_struct(self, i);
    }

    fn visit_item_enum(&mut self, i: &'ast syn::ItemEnum) {
        self.names.insert(i.ident.to_string());
        visit::visit_item_enum(self, i);
    }

    fn visit_item_union(&mut self, i: &'ast syn::ItemUnion) {
        self.names.insert(i.ident.to_string());
        visit::visit_item_union(self, i);
    }

    fn visit_item_type(&mut self, i: &'ast syn::ItemType) {
        self.names.insert(i.ident.to_string());
        visit::visit_item_type(self, i);
    }
}

/// A fn's first parameter, normalized (see [`FirstParam`]).
fn first_param(sig: &syn::Signature) -> Option<FirstParam> {
    match sig.inputs.first()? {
        syn::FnArg::Receiver(_) => Some(FirstParam::Receiver),
        syn::FnArg::Typed(pt) => Some(FirstParam::Typed {
            base: type_base(&pt.ty),
        }),
    }
}

/// A type reduced to its base name: `&`/`&mut`/parens peeled, then the final
/// path segment's ident (generics dropped). `""` for non-path shapes.
fn type_base(ty: &syn::Type) -> String {
    match ty {
        syn::Type::Reference(r) => type_base(&r.elem),
        syn::Type::Paren(p) => type_base(&p.elem),
        syn::Type::Group(g) => type_base(&g.elem),
        syn::Type::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// A path rendered `A::B::C` (segment idents only — generics dropped).
fn path_string(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

#[cfg(test)]
mod tests {
    use super::super::{CandidateKind, Options, find_clones};
    use super::*;

    fn scan(src: &str) -> ScanFile {
        scan_at("src/lib.rs", src)
    }

    fn scan_at(path: &str, src: &str) -> ScanFile {
        ScanFile {
            rel_path: PathBuf::from(path),
            krate: "demo".into(),
            ast: syn::parse_file(src).expect("valid source"),
        }
    }

    /// The one owner entry a single-fn file resolves to.
    fn sole_owner(src: &str) -> FnOwner {
        let file = syn::parse_file(src).unwrap();
        let mut w = FnWalker {
            ctx: Vec::new(),
            out: HashMap::new(),
        };
        w.visit_file(&file);
        assert_eq!(w.out.len(), 1, "expected exactly one fn in {src:?}");
        w.out.into_values().next().unwrap().owner
    }

    fn param_of(src: &str) -> Option<FirstParam> {
        let f: syn::ItemFn = syn::parse_str(src).unwrap();
        first_param(&f.sig)
    }

    #[test]
    fn owner_shapes() {
        assert_eq!(sole_owner("fn f() {}"), FnOwner::Free);
        assert_eq!(
            sole_owner("impl Foo { fn m(&self) {} }"),
            FnOwner::Inherent {
                self_ty: "Foo".into()
            }
        );
        // Generic + qualified self type reduce to the base name.
        assert_eq!(
            sole_owner("impl<T> path::to::Foo<T> { fn m() {} }"),
            FnOwner::Inherent {
                self_ty: "Foo".into()
            }
        );
        assert_eq!(
            sole_owner("impl some::Trait for Foo { fn m(&self) {} }"),
            FnOwner::TraitImpl {
                self_ty: "Foo".into(),
                trait_path: "some::Trait".into()
            }
        );
        assert_eq!(
            sole_owner("trait Tr { fn m(&self) {} }"),
            FnOwner::TraitDecl {
                trait_path: "Tr".into()
            }
        );
    }

    #[test]
    fn first_param_normalizes() {
        assert_eq!(param_of("fn f() {}"), None);
        assert_eq!(param_of("fn f(&self) {}"), Some(FirstParam::Receiver));
        assert_eq!(param_of("fn f(self) {}"), Some(FirstParam::Receiver));
        assert_eq!(
            param_of("fn f(c: Config) {}"),
            Some(FirstParam::Typed {
                base: "Config".into()
            })
        );
        assert_eq!(
            param_of("fn f(c: &Config) {}"),
            Some(FirstParam::Typed {
                base: "Config".into()
            })
        );
        assert_eq!(
            param_of("fn f(c: &mut path::Foo<u32>) {}"),
            Some(FirstParam::Typed { base: "Foo".into() })
        );
        // A non-path shape yields the empty base (never matches a type name).
        assert_eq!(
            param_of("fn f(c: (u8, u8)) {}"),
            Some(FirstParam::Typed {
                base: String::new()
            })
        );
    }

    #[test]
    fn defined_type_names_spans_kinds_and_files() {
        let files = vec![
            scan_at("src/a.rs", "struct S; enum E { A } type Alias = u8;"),
            scan_at("src/b.rs", "union U { a: u8 }"),
        ];
        let mut r = MetaResolver::new(&files);
        let names = r.defined_type_names();
        assert!(names.contains("S") && names.contains("E"));
        assert!(names.contains("Alias") && names.contains("U"));
    }

    #[test]
    fn fn_meta_and_macros_resolve_group_instances() {
        // Two structurally identical fns with different names and one rsx!
        // body each — a Fn group of two.
        let src = r#"
            fn alpha() {
                let x = compute(1);
                rsx! { div { "hi" } }
            }
            fn beta() {
                let y = compute(2);
                rsx! { div { "hi" } }
            }
        "#;
        let files = vec![scan(src)];
        let opts = Options {
            min_lines: 1,
            min_tokens: 1,
            min_instances: 2,
            ignore_literals: true,
            ignore_test_code: false,
            cross_crate_only: false,
            min_distinct_anchors: 0,
            min_non_repeating_ratio: 0.0,
        };
        let groups = find_clones(&files, &opts);
        let group = groups
            .iter()
            .find(|g| g.instances[0].kind == CandidateKind::Fn)
            .expect("a fn group");
        assert_eq!(group.instances.len(), 2);

        let mut r = MetaResolver::new(&files);
        let names: Vec<String> = group
            .instances
            .iter()
            .map(|inst| r.fn_meta(inst).expect("fn resolves").fn_name)
            .collect();
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
        // Each instance's rsx! invocation is seen inside its region.
        assert!(r.macros_in(&group.instances[0]).contains("rsx"));
    }
}
