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
//! broken `mod` declarations, the spliced `include!` file set, and the backing
//! file. Inline `mod foo { ... }` blocks become submodules backed by the same
//! `file` as their parent, and own a deeper `foo/` directory for any file
//! children declared inside them.
//!
//! `include!("…")` IS followed: the included file is spliced into the including
//! module, with its argument const-folded against a `CARGO_*`-seeded env (see
//! `include_resolve` — no build-script harvest in this tier, so `OUT_DIR`
//! includes stay unresolved). Spliced files land on [`Module::generated_files`].
//!
//! The walk logic is copied from syn-workspace's `resolve::module_tree` /
//! `walk.rs` with the resolver-feeding passes (use bindings, occurrences,
//! signature exposures, macro lowering, doc fences) stripped — the module-file
//! resolution, cfg-feature, broken-decl, orphan, and splice behavior is
//! preserved verbatim.
//!
//! Documented limitations (inherited): `#[cfg_attr(cond, path = "...")]` is not
//! expanded, and an `include!` whose argument can't be const-folded to an
//! existing path is left un-spliced.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use cargo_metadata::TargetKind as CargoTargetKind;

use super::doc_fences;
use super::include_resolve::{self, IncludeCtx};
use super::types::{BrokenModDecl, Module, Target, TargetKind};
use super::{FastError, Result};

/// Submodules, broken `mod` declarations, `#[cfg(feature = "...")]` references,
/// and spliced `include!` files collected while walking a module. The
/// `include!` splice merges into these fields, see [`ModuleContents::absorb`].
#[derive(Default)]
struct ModuleContents {
    submodules: Vec<Module>,
    broken_mod_decls: Vec<BrokenModDecl>,
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
            broken_mod_decls,
            cfg_features,
            generated_files,
        } = other;
        self.submodules.extend(submodules);
        self.broken_mod_decls.extend(broken_mod_decls);
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
        broken_mod_decls: contents.broken_mod_decls,
        generated_files: contents.generated_files,
        // Scanned once per backing file; inline `mod {}` blocks share the
        // parent's source, so their fences are already covered here.
        doctest_crate_refs: doc_fences::doc_fence_crate_refs(&source),
        submodules: contents.submodules,
    })
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
    let mut broken_mod_decls = Vec::new();
    let mut cfg_features: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    // Files pulled in by `include!(...)` at this module level, resolved to real
    // paths in the main loop and spliced in afterwards.
    let mut include_sites: Vec<PathBuf> = Vec::new();

    for syn_item in syn_items {
        for attr in item_attrs(syn_item) {
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
                    broken_mod_decls: inline.broken_mod_decls,
                    // An `include!` inside an inline `mod { … }` block splices its
                    // file into that inline module, so carry the inline recursion's
                    // generated files up.
                    generated_files: inline.generated_files,
                    // The parent's file-level scan already covered this block's
                    // doc fences (same source text).
                    doctest_crate_refs: std::collections::HashSet::new(),
                    submodules: inline.submodules,
                });
            } else if let Some(child_file) =
                resolve_mod_file(parent_file, mod_dir, item_mod, in_inline)?
            {
                // A file reached via `mod foo;` owns `dir_owning_children` of
                // itself: `foo.rs` owns `foo/`, `foo/mod.rs` owns `foo/`.
                let child_mod_dir = dir_owning_children(&child_file);
                submodules.push(build_module_from_file(&child_file, &child_mod_dir, inc)?);
            } else {
                // `mod foo;` with neither inline body nor backing file —
                // record so consumers (e.g. module-tree integrity
                // checks) can flag the dangling declaration.
                broken_mod_decls.push(BrokenModDecl {
                    name: child_name,
                    declared_in: parent_file.to_path_buf(),
                    line: item_mod.mod_token.span.start().line as u32,
                });
            }
        }
    }

    let mut contents = ModuleContents {
        submodules,
        broken_mod_decls,
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
fn dir_owning_children(file: &Path) -> PathBuf {
    let parent = file.parent().unwrap_or(Path::new("."));
    match file.file_stem().and_then(|s| s.to_str()) {
        Some("mod") | Some("lib") | Some("main") => parent.to_path_buf(),
        Some(stem) => parent.join(stem),
        None => parent.to_path_buf(),
    }
}

/// Locate the source file backing a `mod foo;` declaration.
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
fn resolve_mod_file(
    parent_file: &Path,
    mod_dir: &Path,
    item_mod: &syn::ItemMod,
    in_inline: bool,
) -> Result<Option<PathBuf>> {
    let mod_name = item_mod.ident.to_string();

    if let Some(override_path) = path_attribute(&item_mod.attrs) {
        let base = if in_inline {
            mod_dir
        } else {
            parent_file.parent().unwrap_or(Path::new("."))
        };
        let candidate = base.join(&override_path);
        return Ok(candidate.exists().then_some(candidate));
    }

    let adjacent = mod_dir.join(format!("{mod_name}.rs"));
    if adjacent.exists() {
        return Ok(Some(adjacent));
    }

    let nested = mod_dir.join(&mod_name).join("mod.rs");
    if nested.exists() {
        return Ok(Some(nested));
    }

    Ok(None)
}

/// Read a `#[path = "..."]` value from a list of attributes, ignoring
/// `cfg_attr`-wrapped forms.
fn path_attribute(attrs: &[syn::Attribute]) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("path") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta
            && let syn::Expr::Lit(lit) = &nv.value
            && let syn::Lit::Str(s) = &lit.lit
        {
            return Some(s.value());
        }
    }
    None
}

/// Outer attributes of a syn item. Returned as a slice so the caller can
/// iterate without copying.
fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(i) => &i.attrs,
        syn::Item::Enum(i) => &i.attrs,
        syn::Item::ExternCrate(i) => &i.attrs,
        syn::Item::Fn(i) => &i.attrs,
        syn::Item::ForeignMod(i) => &i.attrs,
        syn::Item::Impl(i) => &i.attrs,
        syn::Item::Macro(i) => &i.attrs,
        syn::Item::Mod(i) => &i.attrs,
        syn::Item::Static(i) => &i.attrs,
        syn::Item::Struct(i) => &i.attrs,
        syn::Item::Trait(i) => &i.attrs,
        syn::Item::TraitAlias(i) => &i.attrs,
        syn::Item::Type(i) => &i.attrs,
        syn::Item::Union(i) => &i.attrs,
        syn::Item::Use(i) => &i.attrs,
        syn::Item::Verbatim(_) => &[],
        _ => &[],
    }
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

/// `.rs` files under `<manifest_dir>/src/` that aren't reached by any
/// target's module tree and aren't the `src_path` of any target.
pub(super) fn compute_orphans(manifest_dir: &Path, targets: &[Target]) -> Vec<PathBuf> {
    let src_dir = manifest_dir.join("src");
    if !src_dir.is_dir() {
        return Vec::new();
    }

    // Files reached by any target's module tree, plus each target's
    // top-level src_path. Canonicalize so symlinks compare equal.
    let mut reached: HashSet<PathBuf> = HashSet::new();
    for target in targets {
        if let Ok(canon) = target.src_path.canonicalize() {
            reached.insert(canon);
        } else {
            reached.insert(target.src_path.clone());
        }
        for module in target.all_modules() {
            if let Ok(canon) = module.file.canonicalize() {
                reached.insert(canon);
            } else {
                reached.insert(module.file.clone());
            }
            // A file spliced in via `include!(...)` is reached even though it is
            // no module's own `file` (its items live in the including module).
            for gen_file in &module.generated_files {
                if let Ok(canon) = gen_file.canonicalize() {
                    reached.insert(canon);
                } else {
                    reached.insert(gen_file.clone());
                }
            }
        }
    }

    let mut orphans = Vec::new();
    for path in rs_files_under(&src_dir) {
        let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !reached.contains(&canon) && !reached.contains(&path) {
            orphans.push(path);
        }
    }
    orphans
}

/// Recursively list `.rs` files under `dir`, excluding `target/` and
/// hidden directories.
fn rs_files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let read = match std::fs::read_dir(&current) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if name.starts_with('.') || name == "target" {
                continue;
            }
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a module tree from an in-tempdir `src/` layout with an empty
    /// include env (literal includes still resolve).
    fn build(src: &Path, root: &str) -> Module {
        static EMPTY_ENV: std::sync::LazyLock<HashMap<String, String>> =
            std::sync::LazyLock::new(HashMap::new);
        let root_file = src.join(root);
        let mod_dir = root_file.parent().unwrap().to_path_buf();
        build_module_from_file(&root_file, &mod_dir, IncludeCtx::root(&EMPTY_ENV)).expect("build")
    }

    #[test]
    fn walks_mod_decls_in_both_file_forms_and_records_broken_ones() {
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
        assert_eq!(root.broken_mod_decls.len(), 1);
        assert_eq!(root.broken_mod_decls[0].name, "missing");
        assert_eq!(root.broken_mod_decls[0].line, 3);
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
            "#[cfg(feature = \"genfeat\")]\npub fn g() {}\nmod dangling;\n",
        )
        .unwrap();

        let root = build(&src, "lib.rs");
        // Spliced once despite the duplicate include; its cfg features and
        // broken decls land on the including module, anchored in the
        // generated file.
        assert_eq!(root.generated_files.len(), 1);
        assert!(root.generated_files[0].ends_with("generated.rs"));
        assert_eq!(root.cfg_features, ["genfeat"]);
        assert_eq!(root.broken_mod_decls.len(), 1);
        assert!(
            root.broken_mod_decls[0]
                .declared_in
                .ends_with("generated.rs")
        );
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

    #[test]
    fn compute_orphans_flags_unreached_src_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("lib.rs"), "mod reached;\n").unwrap();
        std::fs::write(src.join("reached.rs"), "").unwrap();
        std::fs::write(src.join("stale.rs"), "").unwrap();

        let root = build(&src, "lib.rs");
        let targets = vec![Target {
            kind: TargetKind::Lib,
            name: "demo".into(),
            src_path: src.join("lib.rs"),
            root,
        }];
        let orphans = compute_orphans(dir.path(), &targets);
        assert_eq!(orphans.len(), 1);
        assert!(orphans[0].ends_with("stale.rs"));
    }
}
