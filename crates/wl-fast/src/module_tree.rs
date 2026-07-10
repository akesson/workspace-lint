//! The lean syntactic module-tree walker behind [`FastModel`](super::FastModel).
//!
//! For each crate, starts at every target root (`lib.rs`/`main.rs` and each
//! cargo target's `src_path` — bin/example/test/bench/build-script) and walks
//! every `mod foo;` declaration to its backing file, resolved in the declaring
//! module's *owning directory*:
//!
//! - A **target root** owns its own *containing* directory regardless of its
//!   filename — it is a crate boundary, so its children are siblings
//!   (`foo.rs` / `foo/mod.rs`). (Callers pass this dir explicitly; computing it
//!   from the stem would wrongly resolve e.g. `tests/integration.rs`'s
//!   `mod common;` into `tests/integration/`.)
//! - A file reached *via* a `mod foo;` declaration owns `dir_owning_children`
//!   of itself: `mod.rs` owns its own dir; any other `bar.rs` owns a `bar/`
//!   subdirectory, so its children live under `bar/` (`bar/foo.rs` /
//!   `bar/foo/mod.rs`).
//! - A `#[path = "..."]` override is instead relative to the directory of the
//!   file that contains the `mod` statement — or, when the module sits inside an
//!   inline `mod { … }` block, that directory *plus the inline-module names as
//!   directories* (Rust's two-case rule; see `resolve_mod_file`).
//!
//! Produces a tree of [`Module`] values rooted at each target root, populated
//! with the *syntactic* facts the fast tier serves: per-module `cfg_features`,
//! the spliced `include!` file set, the macro-named file set, and the backing
//! file. Inline `mod foo { ... }` blocks become submodules backed by the same
//! `file` as their parent, and own a deeper `foo/` directory for any file
//! children declared inside them.
//!
//! The walk evaluates **no cfg**. Every `#[cfg_attr(<pred>, path = "…")]` arm
//! that names a file on disk is walked, so a platform-abstraction module
//! contributes its whole subtree regardless of the host. [`crate::reach`] turns
//! this into the "could rustc open this file?" query that `orphan-file` needs.
//!
//! `include!("…")` IS followed: the included file is spliced into the including
//! module, with its argument const-folded against a `CARGO_*`-seeded env (see
//! `include_resolve` — no build-script harvest in this tier, so `OUT_DIR`
//! includes stay unresolved). Spliced files land on [`Module::generated_files`];
//! `include!` outside item position, and `include_str!` / `include_bytes!`
//! anywhere, land on [`Module::named_files`] instead (named, never spliced).
//!
//! The walk logic is copied from syn-workspace's `resolve::module_tree` /
//! `walk.rs` with the resolver-feeding passes (use bindings, occurrences,
//! signature exposures, macro lowering, doc fences) stripped.
//!
//! Documented limitation: an `include!` whose argument can't be const-folded to
//! an existing path is left un-spliced (and unnamed).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use cargo_metadata::TargetKind as CargoTargetKind;

use super::doc_fences;
use super::include_resolve::{self, IncludeCtx};
use super::types::{Module, Target, TargetKind};
use super::{FastError, Result};

/// Submodules, `#[cfg(feature = "...")]` references, and spliced `include!`
/// files collected while walking a module. The `include!` splice merges into
/// these fields, see [`ModuleContents::absorb`].
#[derive(Default)]
struct ModuleContents {
    submodules: Vec<Module>,
    cfg_features: Vec<String>,
    /// Files spliced in via `include!(...)` while collecting this module (and any
    /// nested includes). Bubbled onto [`Module::generated_files`].
    generated_files: Vec<PathBuf>,
}

impl ModuleContents {
    /// Merge another module's collected contents into this one — the single merge
    /// point for the `include!` splice. The destructuring binding is load-bearing:
    /// a field added to [`ModuleContents`] later won't compile here until it's
    /// explicitly merged, so the splice path can't silently drop a generated
    /// file's data. `cfg_features` is re-deduped to preserve the set semantics it
    /// had as a `BTreeSet` before the final `Vec` collect.
    fn absorb(&mut self, other: ModuleContents) {
        let ModuleContents {
            submodules,
            cfg_features,
            generated_files,
        } = other;
        self.submodules.extend(submodules);
        self.cfg_features.extend(cfg_features);
        self.cfg_features.sort();
        self.cfg_features.dedup();
        self.generated_files.extend(generated_files);
    }
}

/// Read and parse one source file — the shared prelude of both
/// [`build_module_from_file`] and the `include!` splice (so the two paths can't
/// drift in how they read / parse a file). Returns the raw source too: the
/// doc-fence scan reads it (fences live in comments, invisible post-parse).
fn read_parse(path: &Path) -> Result<(String, syn::File)> {
    let source = std::fs::read_to_string(path)?;
    let parsed = syn::parse_file(&source).map_err(|e| FastError::Parse {
        path: path.to_path_buf(),
        source: e,
    })?;
    Ok((source, parsed))
}

/// Build the module tree rooted at one file.
///
/// `mod_dir` is the directory in which this file's `mod foo;` declarations are
/// resolved. For a **target/crate root** (the `src_path` of any cargo target)
/// it is the file's own directory — a root owns its containing directory
/// regardless of filename. For a file reached *via* a `mod foo;` declaration,
/// the caller passes [`dir_owning_children`] of that file (the
/// `foo.rs`-owns-`foo/` convention). Computing it from the file stem here
/// would be wrong for target roots like `tests/integration.rs`.
fn build_module_from_file(file_path: &Path, mod_dir: &Path, inc: IncludeCtx<'_>) -> Result<Module> {
    let (source, parsed) = read_parse(file_path)?;

    // A file's own items are at its top level — not inside any inline block of
    // *this* file, even when the file itself was reached via a `mod foo;`.
    let mut contents = collect_module_contents(&parsed.items, file_path, mod_dir, false, inc)?;

    // File-level inner attributes gate too: an integration test opening with
    // `#![cfg(feature = "…")]` is the canonical way to feature-gate a whole
    // test target, and it lives in `syn::File::attrs`, attached to no item —
    // dropping it made feature-drift report the feature "never gated".
    let mut file_gates = std::collections::BTreeSet::new();
    for attr in &parsed.attrs {
        extract_cfg_feature_names(attr, &mut file_gates);
    }
    for gate in file_gates {
        if !contents.cfg_features.contains(&gate) {
            contents.cfg_features.push(gate);
        }
    }

    Ok(Module {
        file: file_path.to_path_buf(),
        cfg_features: contents.cfg_features,
        // Scanned once per backing file, over the whole syntax tree rather than
        // its top-level items — an `include!` in expression position and an
        // `include_str!` in a `const` initializer are invisible to the item
        // walk, and calling their targets orphaned would advise deleting a file
        // rustc compiles in.
        named_files: named_macro_files(&parsed, file_path, inc),
        generated_files: contents.generated_files,
        // Scanned once per backing file; inline `mod {}` blocks share the
        // parent's source, so their fences are already covered here.
        doctest_crate_refs: doc_fences::doc_fence_crate_refs(&source),
        submodules: contents.submodules,
    })
}

/// Every existing file named by an `include!` / `include_str!` / `include_bytes!`
/// anywhere in `parsed` — including positions the item walk never reaches
/// (expression, `const`/`static` initializer, inside a function body).
///
/// Item-position `include!`s are found here too and are therefore recorded
/// twice: once as [`Module::generated_files`] (spliced source) and once here.
/// The duplication is harmless — both feed a reach *set* — and keeping the scan
/// position-blind is what makes it total.
///
/// Files that don't exist are skipped: an unresolvable `include!(concat!(env!(
/// "OUT_DIR"), …))` names nothing we could judge, and generated code under
/// `target/` is outside the `src/` tree the caller scans anyway.
fn named_macro_files(parsed: &syn::File, parent_file: &Path, inc: IncludeCtx<'_>) -> Vec<PathBuf> {
    use syn::visit::Visit;

    struct MacroScan<'a> {
        base_dir: &'a Path,
        env: &'a HashMap<String, String>,
        found: Vec<PathBuf>,
    }

    impl<'ast> Visit<'ast> for MacroScan<'_> {
        fn visit_macro(&mut self, mac: &'ast syn::Macro) {
            let names = ["include", "include_str", "include_bytes"];
            if names
                .iter()
                .any(|n| include_resolve::macro_is(&mac.path, n))
                && let Some(path) =
                    include_resolve::resolve_include_path(mac, self.base_dir, self.env)
            {
                self.found.push(path);
            }
            syn::visit::visit_macro(self, mac);
        }
    }

    let mut scan = MacroScan {
        base_dir: parent_file.parent().unwrap_or(Path::new(".")),
        env: inc.env,
        found: Vec::new(),
    };
    scan.visit_file(parsed);
    scan.found.sort();
    scan.found.dedup();
    scan.found
}

fn collect_module_contents(
    syn_items: &[syn::Item],
    parent_file: &Path,
    mod_dir: &Path,
    // Whether these items sit inside one or more inline `mod { … }` blocks of the
    // current file. Governs the `#[path]` base in `resolve_mod_file`: a nested
    // `#[path]` anchors at `mod_dir` (which already carries the inline names),
    // while a top-level one anchors at the declaring file's directory. Resets to
    // `false` when a `mod foo;` crosses into a new file (`build_module_from_file`).
    in_inline: bool,
    // Environment + nesting depth for `include!(...)` resolution. The env folds
    // `env!(...)` (seeded `CARGO_*`); the depth is the cyclic-include backstop.
    inc: IncludeCtx<'_>,
) -> Result<ModuleContents> {
    let mut submodules = Vec::new();
    let mut cfg_features: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Files pulled in by `include!(...)` at this module level, resolved to real
    // paths in the main loop and spliced in afterwards.
    let mut include_sites: Vec<PathBuf> = Vec::new();

    for syn_item in syn_items {
        for attr in crate::syn_util::item_attrs(syn_item) {
            extract_cfg_feature_names(attr, &mut cfg_features);
        }

        if let syn::Item::Macro(item_macro) = syn_item
            && let Some(path) = resolve_include_site(item_macro, parent_file, inc)
        {
            // Queue the resolved generated file for splicing after the main
            // loop. An argument we can't const-fold to an existing path yields
            // `None` and is left un-spliced.
            include_sites.push(path);
        }

        if let syn::Item::Mod(item_mod) = syn_item {
            let child_name = item_mod.ident.to_string();

            if let Some((_, inline_items)) = &item_mod.content {
                // An inline `mod a { … }` owns a deeper directory: any file
                // child declared inside it resolves in `<mod_dir>/a/`, not
                // `<mod_dir>/`.
                let inline = collect_module_contents(
                    inline_items,
                    parent_file,
                    &mod_dir.join(&child_name),
                    // Items here are inside this inline block — a nested
                    // `#[path]` anchors at the (now deeper) `mod_dir`.
                    true,
                    // An inline `mod { … }` doesn't deepen include nesting; pass
                    // the same context (env + depth) straight through.
                    inc,
                )?;
                // Inline `mod foo { ... }` shares the parent's `file`.
                submodules.push(Module {
                    file: parent_file.to_path_buf(),
                    cfg_features: inline.cfg_features,
                    // The parent's file-level scan already covered this block's
                    // macros (same syntax tree).
                    named_files: Vec::new(),
                    // An `include!` inside an inline `mod { … }` block splices its
                    // file into that inline module, so carry the inline recursion's
                    // generated files up.
                    generated_files: inline.generated_files,
                    // The parent's file-level scan already covered this block's
                    // doc fences (same source text).
                    doctest_crate_refs: std::collections::HashSet::new(),
                    submodules: inline.submodules,
                });
            } else {
                // Every existing file this `mod` could resolve to. Usually one;
                // two or more for the platform idiom
                // `#[cfg_attr(unix, path = "unix.rs")] #[cfg_attr(windows, …)]`,
                // where each arm names a real file and exactly one compiles.
                //
                // We walk *all* of them. The walker is cfg-agnostic by design,
                // and taking only the first arm would leave the other platform's
                // file — and its whole subtree — unreached.
                //
                // Zero candidates is not an error here: `mod foo;` with no
                // backing file is rustc's E0583, a hard compile error, and the
                // semantic tier only ever runs on a workspace that compiles.
                for child_file in resolve_mod_files(parent_file, mod_dir, item_mod, in_inline)? {
                    // A file reached via `mod foo;` owns `dir_owning_children` of
                    // itself: `foo.rs` owns `foo/`, `foo/mod.rs` owns `foo/`.
                    let child_mod_dir = dir_owning_children(&child_file);
                    submodules.push(build_module_from_file(&child_file, &child_mod_dir, inc)?);
                }
            }
        }
    }

    let mut contents = ModuleContents {
        submodules,
        cfg_features: cfg_features.into_iter().collect(),
        generated_files: Vec::new(),
    };

    // Splice every resolved `include!`d file into this module (see
    // `splice_includes`). Done last so the included files merge in through the
    // single `absorb` point.
    if let Some(spliced) = splice_includes(include_sites, inc, in_inline, mod_dir) {
        contents.absorb(spliced);
    }

    Ok(contents)
}

/// Resolve an item-position `include!(...)` to the generated file it pulls in, or
/// `None` if the macro isn't `include!` or its argument can't be const-folded to
/// an existing path. A relative path resolves against the directory of the
/// including file (`parent_file`).
fn resolve_include_site(
    item_macro: &syn::ItemMacro,
    parent_file: &Path,
    inc: IncludeCtx<'_>,
) -> Option<PathBuf> {
    if !include_resolve::macro_is(&item_macro.mac.path, "include") {
        return None;
    }
    let base_dir = parent_file.parent().unwrap_or(Path::new("."));
    include_resolve::resolve_include_path(&item_macro.mac, base_dir, inc.env)
}

/// Splice every resolved `include!`d file into the including module, returning the
/// merged contents (`None` when there is nothing to splice).
///
/// The included items belong to *this* module (an `include!` is not a
/// submodule), so they share `mod_dir`; `parent_file` is the generated file
/// itself, so a broken `mod` decl inside it is anchored there — the key the
/// diagnostic pipeline uses to recognize generated code.
///
/// Best-effort: an unreadable / unparsable generated file is skipped rather than
/// failing the load. Cyclic and *fan-out* cyclic includes are broken by the
/// `inc.ancestry` chain set (with `MAX_INCLUDE_DEPTH` as a backstop); a file
/// included twice into the same module is spliced once.
fn splice_includes(
    include_sites: Vec<PathBuf>,
    inc: IncludeCtx<'_>,
    in_inline: bool,
    mod_dir: &Path,
) -> Option<ModuleContents> {
    if include_sites.is_empty() {
        return None;
    }
    let mut merged = ModuleContents::default();
    // Files already spliced into *this* module — dedups `include!("g"); include!("g");`.
    let mut spliced_here: HashSet<PathBuf> = HashSet::new();
    for included_path in include_sites {
        let canon = included_path
            .canonicalize()
            .unwrap_or_else(|_| included_path.clone());
        // Skip a file already on the include chain (cycle / fan-out cycle) or
        // already spliced into this same module.
        if inc.ancestry.contains(&canon) || !spliced_here.insert(canon.clone()) {
            continue;
        }
        // Descend with `canon` appended to the chain; `None` at the depth cap.
        let mut child_ancestry = inc.ancestry.clone();
        child_ancestry.insert(canon);
        let Some(child) = inc.descend(&child_ancestry) else {
            continue;
        };
        let Ok((_, parsed)) = read_parse(&included_path) else {
            continue;
        };
        let Ok(spliced) =
            collect_module_contents(&parsed.items, &included_path, mod_dir, in_inline, child)
        else {
            continue;
        };
        merged.generated_files.push(included_path);
        merged.absorb(spliced);
    }
    Some(merged)
}

/// The directory in which a file's `mod foo;` children are resolved.
///
/// Rust's module-file convention: a crate root (`lib.rs`/`main.rs`) and a
/// `mod.rs` own the directory they sit in, so their children are siblings;
/// any other file `foo.rs` owns a `foo/` subdirectory, so *its* children live
/// under `foo/`.
pub(crate) fn dir_owning_children(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(Path::new("."));
    match file.file_stem().and_then(|s| s.to_str()) {
        Some("mod") | Some("lib") | Some("main") => parent.to_path_buf(),
        Some(stem) => parent.join(stem),
        None => parent.to_path_buf(),
    }
}

/// Every existing source file a `mod foo;` declaration could resolve to.
///
/// A plain `mod foo;` resolves in `mod_dir` (the declaring module's owning
/// directory — see `dir_owning_children`): `<mod_dir>/foo.rs` then
/// `<mod_dir>/foo/mod.rs`. A `#[path = "..."]` override follows Rust's two-case
/// rule, keyed on `in_inline`:
/// - **not inside an inline block** (top level of the file): relative to the
///   directory of the file that contains the `mod` statement
///   (`parent_file`'s directory).
/// - **inside an inline `mod { … }` block**: relative to the file's owning
///   directory *including the inline-module names as directories* — which is
///   exactly what `mod_dir` already accumulates (it is joined with each inline
///   name on the way down). This holds for both mod-rs files (`src/` + inline)
///   and non-mod-rs files (`dir/stem/` + inline), since `mod_dir` starts from
///   [`dir_owning_children`].
///
/// **Returns every candidate, not the one rustc would pick.** The platform
/// idiom
///
/// ```ignore
/// #[cfg_attr(unix,    path = "unix.rs")]
/// #[cfg_attr(windows, path = "windows.rs")]
/// mod imp;
/// ```
///
/// names two real files, of which exactly one compiles per target. This walker
/// evaluates no cfg, so it cannot know which — and must not have to. Returning
/// only the first would leave the other platform's file (and its whole subtree)
/// unreached, which is how `orphan-file` once advised deleting `windows.rs` on
/// a mac. Used by `memmap2`, `socket2`, and `tempfile`, among others.
fn resolve_mod_files(
    parent_file: &Path,
    mod_dir: &Path,
    item_mod: &syn::ItemMod,
    in_inline: bool,
) -> Result<Vec<PathBuf>> {
    let mod_name = item_mod.ident.to_string();

    let overrides = path_attributes(&item_mod.attrs);
    if !overrides.is_empty() {
        let base = if in_inline {
            mod_dir
        } else {
            parent_file.parent().unwrap_or(Path::new("."))
        };
        return Ok(overrides
            .into_iter()
            .map(|p| base.join(p))
            .filter(|c| c.exists())
            .collect());
    }

    // The two conventional forms are mutually exclusive in valid Rust (rustc
    // errors when both exist), so this yields at most one.
    let adjacent = mod_dir.join(format!("{mod_name}.rs"));
    if adjacent.exists() {
        return Ok(vec![adjacent]);
    }

    let nested = mod_dir.join(&mod_name).join("mod.rs");
    if nested.exists() {
        return Ok(vec![nested]);
    }

    Ok(Vec::new())
}

/// [`resolve_mod_files`] for callers outside the walk (the cfg-region scan):
/// file-level `mod` resolution, infallible, `#[path]` anchored at the
/// declaring file's directory, first candidate only — a cfg region lives in one
/// file, and the scan asks which file a `mod` leads into, not which files a
/// `mod` *could* lead into.
pub(crate) fn resolve_mod_file_simple(
    parent_file: &Path,
    mod_dir: &Path,
    item_mod: &syn::ItemMod,
) -> Option<PathBuf> {
    resolve_mod_files(parent_file, mod_dir, item_mod, false)
        .ok()?
        .into_iter()
        .next()
}

/// Every `path = "..."` value on a `mod` declaration, from both the bare
/// `#[path = "..."]` form and every `#[cfg_attr(<pred>, path = "...")]` arm.
///
/// Order is source order, and cfg predicates are *not* evaluated: the caller
/// keeps whichever candidates exist on disk. See [`resolve_mod_files`] for why
/// every arm matters.
fn path_attributes(attrs: &[syn::Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attrs {
        if attr.path().is_ident("path") {
            if let Some(value) = path_name_value(&attr.meta) {
                out.push(value);
            }
        } else if attr.path().is_ident("cfg_attr") {
            out.extend(cfg_attr_paths(attr));
        }
    }
    out
}

/// The `path = "..."` arms of one `#[cfg_attr(<pred>, <attr>, ...)]`.
///
/// `cfg_attr` is `(predicate, attr, attr, ...)`: the first element is the cfg
/// condition (which may itself be a list — `any(unix, target_os = "wasi")`) and
/// the rest are the attributes it would apply. We skip the predicate and read
/// any `path` name-value among the remainder.
fn cfg_attr_paths(attr: &syn::Attribute) -> Vec<String> {
    use syn::punctuated::Punctuated;

    let Ok(args) = attr.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
    else {
        return Vec::new();
    };
    args.iter()
        .skip(1) // the cfg predicate
        .filter(|meta| meta.path().is_ident("path"))
        .filter_map(path_name_value)
        .collect()
}

/// The string value of a `path = "..."` meta, if that is what this is.
fn path_name_value(meta: &syn::Meta) -> Option<String> {
    if let syn::Meta::NameValue(nv) = meta
        && nv.path.is_ident("path")
        && let syn::Expr::Lit(lit) = &nv.value
        && let syn::Lit::Str(s) = &lit.lit
    {
        return Some(s.value());
    }
    None
}

/// Scan an attribute for `feature = "name"` predicates inside `cfg(...)` or
/// `cfg_attr(<cfg>, ...)`. Predicates can be nested under `any(...)`,
/// `all(...)`, and `not(...)`; we recurse through the meta-list tree.
fn extract_cfg_feature_names(attr: &syn::Attribute, out: &mut std::collections::BTreeSet<String>) {
    let ident = match attr.path().get_ident() {
        Some(i) => i.to_string(),
        None => return,
    };
    if ident != "cfg" && ident != "cfg_attr" {
        return;
    }
    // Parse the inner meta. cfg(...) and cfg_attr(<cfg>, ...) both start
    // with a Meta::List whose nested predicate-tree we scan.
    if let syn::Meta::List(list) = &attr.meta {
        scan_cfg_tokens(list.tokens.clone(), out);
    }
}

fn scan_cfg_tokens(tokens: proc_macro2::TokenStream, out: &mut std::collections::BTreeSet<String>) {
    let iter: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    let mut i = 0;
    while i < iter.len() {
        if let proc_macro2::TokenTree::Ident(id) = &iter[i] {
            let name = id.to_string();
            if name == "feature"
                && let Some(proc_macro2::TokenTree::Punct(p)) = iter.get(i + 1)
                && p.as_char() == '='
                && let Some(proc_macro2::TokenTree::Literal(lit)) = iter.get(i + 2)
            {
                let s = lit.to_string();
                let trimmed = s.trim_matches('"');
                if !trimmed.is_empty() {
                    out.insert(trimmed.to_string());
                }
                i += 3;
                continue;
            }
        }
        if let proc_macro2::TokenTree::Group(g) = &iter[i] {
            scan_cfg_tokens(g.stream(), out);
        }
        i += 1;
    }
}

/// Materialize one workspace package's cargo targets (lib, bin, example, test,
/// bench, build script, proc-macro library) as [`Target`] entries with their
/// own module trees.
///
/// A parse failure on the primary lib/bin/proc-macro target propagates (the
/// crate's model would be silently hollow otherwise); auxiliary targets
/// (test/example/bench/build-script) are skipped on failure. syn-workspace
/// records a `LoadWarning` for the latter — the fast tier deliberately carries
/// no warnings channel, so the skip is silent.
pub(super) fn build_targets(
    pkg: &cargo_metadata::Package,
    manifest_dir: &Path,
) -> Result<Vec<Target>> {
    // Per-crate `include!` environment: `CARGO_*` vars seeded from metadata
    // (offline, always present). Drives `env!(...)` const-folding during the
    // module walk. No build-script harvest here — the fast tier is build-free,
    // so only literal / `CARGO_*` includes resolve (`OUT_DIR` output never
    // joins this tier's generated-file set).
    let mut include_env: HashMap<String, String> = HashMap::new();
    include_env.insert(
        "CARGO_MANIFEST_DIR".to_string(),
        manifest_dir.to_string_lossy().into_owned(),
    );
    include_env.insert("CARGO_PKG_NAME".to_string(), pkg.name.to_string());
    include_env.insert("CARGO_PKG_VERSION".to_string(), pkg.version.to_string());
    let inc = IncludeCtx::root(&include_env);

    let mut targets = Vec::new();
    for cargo_target in &pkg.targets {
        let Some(kind) = pick_target_kind(&cargo_target.kind) else {
            continue;
        };
        let src_path = cargo_target.src_path.as_std_path().to_path_buf();
        if !src_path.exists() {
            continue;
        }
        // A target root (lib/bin/example/test/bench/build-script) is a crate
        // boundary that owns its containing directory, regardless of its
        // filename — its `mod foo;` children resolve as siblings. (Passing
        // the file itself here would wrongly resolve e.g. `tests/it.rs`'s
        // `mod common;` into `tests/it/`.)
        let mod_dir = src_path.parent().unwrap_or(Path::new("."));
        let root = match build_module_from_file(&src_path, mod_dir, inc) {
            Ok(m) => m,
            Err(e) => {
                if matches!(
                    kind,
                    TargetKind::Lib | TargetKind::ProcMacro | TargetKind::Bin
                ) {
                    return Err(e);
                }
                continue;
            }
        };

        targets.push(Target {
            kind,
            name: cargo_target.name.clone(),
            src_path,
            root,
        });
    }
    Ok(targets)
}

/// Map cargo's per-target `kind: Vec<TargetKind>` (which may report
/// multiple crate-types for one target — e.g. `["lib", "cdylib"]`) onto
/// our coalesced [`TargetKind`]. `ProcMacro` outranks `Lib`; everything
/// else falls through in priority order. Unknown kinds yield `None`,
/// causing the target to be silently dropped.
fn pick_target_kind(kinds: &[CargoTargetKind]) -> Option<TargetKind> {
    if kinds
        .iter()
        .any(|k| matches!(k, CargoTargetKind::ProcMacro))
    {
        return Some(TargetKind::ProcMacro);
    }
    if kinds.iter().any(|k| {
        matches!(
            k,
            CargoTargetKind::Lib
                | CargoTargetKind::RLib
                | CargoTargetKind::DyLib
                | CargoTargetKind::CDyLib
                | CargoTargetKind::StaticLib
        )
    }) {
        return Some(TargetKind::Lib);
    }
    for k in kinds {
        match k {
            CargoTargetKind::Bin => return Some(TargetKind::Bin),
            CargoTargetKind::Example => return Some(TargetKind::Example),
            CargoTargetKind::Test => return Some(TargetKind::Test),
            CargoTargetKind::Bench => return Some(TargetKind::Bench),
            CargoTargetKind::CustomBuild => return Some(TargetKind::BuildScript),
            _ => {}
        }
    }
    None
}

/// Walk-driving helper shared by this module's tests and [`crate::reach`]'s
/// (which needs a real tree to derive a reach set from).
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Build a module tree from an in-tempdir `src/` layout with an empty
    /// include env (literal includes still resolve).
    pub(crate) fn build(src: &Path, root: &str) -> Module {
        static EMPTY_ENV: std::sync::LazyLock<HashMap<String, String>> =
            std::sync::LazyLock::new(HashMap::new);
        let root_file = src.join(root);
        let mod_dir = root_file.parent().unwrap().to_path_buf();
        build_module_from_file(&root_file, &mod_dir, IncludeCtx::root(&EMPTY_ENV)).expect("build")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::build;
    use super::*;

    /// Both file forms resolve; a `mod` with no backing file is simply not a
    /// module (it is rustc's E0583, and the semantic tier only runs on a
    /// workspace that compiles).
    #[test]
    fn walks_mod_decls_in_both_file_forms_and_skips_unresolvable_ones() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("b")).unwrap();
        std::fs::write(src.join("lib.rs"), "mod a;\nmod b;\nmod missing;\n").unwrap();
        std::fs::write(src.join("a.rs"), "").unwrap();
        std::fs::write(src.join("b/mod.rs"), "").unwrap();

        let root = build(&src, "lib.rs");
        let files: Vec<_> = root
            .walk()
            .map(|m| m.file.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(files, ["lib.rs", "a.rs", "mod.rs"]);
    }

    #[test]
    fn non_mod_rs_file_owns_its_stem_directory() {
        // `src/outer.rs` declaring `mod inner;` resolves into `src/outer/`,
        // not `src/` — the 2018-layout convention.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("outer")).unwrap();
        std::fs::write(src.join("lib.rs"), "mod outer;\n").unwrap();
        std::fs::write(src.join("outer.rs"), "mod inner;\n").unwrap();
        std::fs::write(src.join("outer/inner.rs"), "").unwrap();

        let root = build(&src, "lib.rs");
        assert!(root.walk().any(|m| m.file.ends_with("outer/inner.rs")));
    }

    #[test]
    fn cfg_features_are_collected_per_module_sorted_deduped() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "#[cfg(feature = \"b\")]\npub fn f() {}\n\
             #[cfg(any(feature = \"a\", feature = \"b\"))]\npub fn g() {}\n\
             #[cfg_attr(feature = \"c\", allow(dead_code))]\npub fn h() {}\n",
        )
        .unwrap();

        let root = build(&src, "lib.rs");
        assert_eq!(root.cfg_features, ["a", "b", "c"]);
    }

    #[test]
    fn inline_mod_shares_file_and_owns_deeper_dir() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("outer")).unwrap();
        std::fs::write(src.join("lib.rs"), "mod outer { mod file_child; }\n").unwrap();
        std::fs::write(src.join("outer/file_child.rs"), "").unwrap();

        let root = build(&src, "lib.rs");
        let inline = &root.submodules[0];
        assert!(inline.file.ends_with("lib.rs"), "inline shares the file");
        assert!(inline.submodules[0].file.ends_with("outer/file_child.rs"));
    }

    #[test]
    fn include_splices_generated_file_and_merges_its_facts() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "include!(\"generated.rs\");\ninclude!(\"generated.rs\");\n",
        )
        .unwrap();
        std::fs::write(
            src.join("generated.rs"),
            "#[cfg(feature = \"genfeat\")]\npub fn g() {}\n",
        )
        .unwrap();

        let root = build(&src, "lib.rs");
        // Spliced once despite the duplicate include; its cfg features land on
        // the including module.
        assert_eq!(root.generated_files.len(), 1);
        assert!(root.generated_files[0].ends_with("generated.rs"));
        assert_eq!(root.cfg_features, ["genfeat"]);
    }

    #[test]
    fn fan_out_cyclic_includes_terminate() {
        // lib → a; a → {b, c}; b → a and c → a. The `IncludeCtx::ancestry`
        // chain set breaks the fan-out cycle; reaching the assertion at all is
        // the real signal (without the guard this walk explodes).
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "include!(\"a.rs\");\n").unwrap();
        std::fs::write(
            src.join("a.rs"),
            "include!(\"b.rs\");\ninclude!(\"c.rs\");\n",
        )
        .unwrap();
        std::fs::write(src.join("b.rs"), "include!(\"a.rs\");\n").unwrap();
        std::fs::write(src.join("c.rs"), "include!(\"a.rs\");\n").unwrap();

        let root = build(&src, "lib.rs");
        let names: Vec<_> = root
            .generated_files
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, ["a.rs", "b.rs", "c.rs"]);
    }

    /// A `#[cfg_attr]` whose predicate is itself a list, and which carries a
    /// second attribute after the `path` — `tempfile`'s exact shape.
    #[test]
    fn cfg_attr_path_parses_list_predicates_and_trailing_attrs() {
        let attrs: syn::ItemMod = syn::parse_quote! {
            #[cfg_attr(any(unix, target_os = "wasi"), path = "unix.rs", allow(dead_code))]
            #[cfg_attr(windows, path = "windows.rs")]
            #[cfg(feature = "x")]
            mod imp;
        };
        assert_eq!(path_attributes(&attrs.attrs), ["unix.rs", "windows.rs"]);
    }

    /// A bare `#[path]` still wins, and a `cfg_attr` carrying no `path` at all
    /// contributes nothing.
    #[test]
    fn path_attributes_ignores_unrelated_cfg_attrs() {
        let item: syn::ItemMod = syn::parse_quote! {
            #[cfg_attr(test, allow(dead_code))]
            #[path = "custom.rs"]
            mod imp;
        };
        assert_eq!(path_attributes(&item.attrs), ["custom.rs"]);
    }

    /// A macro-named file is reached, but it is not *generated code*: nothing
    /// was spliced, so surgery's no-go set stays empty.
    #[test]
    fn macro_named_files_are_not_generated_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub const S: &str = include_str!(\"snippet.rs\");\n",
        )
        .unwrap();
        std::fs::write(src.join("snippet.rs"), "fn x() {}").unwrap();

        let root = build(&src, "lib.rs");
        assert!(root.generated_files.is_empty());
        assert_eq!(root.named_files.len(), 1);
        assert!(root.named_files[0].ends_with("snippet.rs"));
    }

    /// An `include!` naming a file that doesn't exist (an unresolved `OUT_DIR`
    /// path, say) names nothing judgeable and must not enter the reach set.
    #[test]
    fn named_files_skip_nonexistent_macro_targets() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub const S: &str = include_str!(\"absent.rs\");\n",
        )
        .unwrap();

        let root = build(&src, "lib.rs");
        assert!(root.named_files.is_empty());
    }
}
