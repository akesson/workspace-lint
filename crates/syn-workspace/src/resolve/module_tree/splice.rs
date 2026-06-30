//! Helpers for the `include!` splice in the module-tree walk (`mod.rs`), factored
//! out to keep that file focused: the ambient lexical scope threaded into a
//! spliced file, the shared read→parse→seed file prelude, the item-position macro
//! lowering dispatch, and the splice itself ([`splice_includes`] /
//! [`ModuleContents::absorb`]).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::macros::annotation::comment_expansion_uses_occurrences;
use crate::macros::autodetect::extract_macro_paths;
use crate::plugins;
use crate::resolve::use_tree::UseBinding;
use crate::resolve::{Error, Occurrence, ResolvedPath, Result};

use super::{IncludeCtx, ModuleContents, collect_module_contents, include_resolve};

/// Lexical scope an `include!`d file must resolve against, inherited from the
/// module that contains the `include!`. Because `include!` pastes its tokens into
/// the including module (it is *not* a fresh submodule), generated code sees that
/// module's sibling items and `use` imports; threading them in lets a reference
/// *from* generated code to a handwritten sibling or a parent import resolve —
/// otherwise it leaks as an `unused-deps` / `unused-pub` false positive on the
/// handwritten code, which the generated-file drop can't catch (the finding is
/// anchored on the handwritten file). These widen *resolution only*: ambient `use`
/// bindings are deliberately kept out of the module's persisted `use_bindings`
/// (which feeds the re-export / SCIP / referrer passes) so an included file never
/// duplicates its parent's imports into the model.
///
/// [`AmbientScope::empty`] is the no-inheritance case used by every entry point
/// that is *not* an `include!` splice — a file root and an inline `mod { … }`
/// each open their own scope.
#[derive(Clone, Copy)]
pub(super) struct AmbientScope<'a> {
    pub(super) siblings: &'a HashSet<String>,
    pub(super) use_bindings: &'a [UseBinding],
    pub(super) glob: bool,
}

impl AmbientScope<'static> {
    /// The empty ambient scope: no inherited siblings, bindings, or glob. Keeps
    /// every non-`include!` caller's behavior unchanged.
    pub(super) fn empty() -> Self {
        static EMPTY: std::sync::LazyLock<HashSet<String>> = std::sync::LazyLock::new(HashSet::new);
        AmbientScope {
            siblings: &EMPTY,
            use_bindings: &[],
            glob: false,
        }
    }
}

/// Read, parse, and recover comment-directive seed occurrences for one source
/// file — the shared prelude of both [`build_module_from_file`](super::build_module_from_file)
/// and the `include!` splice (so the two paths can't drift in how they read /
/// parse / seed a file). Returns the file text (callers may still need it, e.g.
/// for doc-fence scanning), the parsed AST, and the seed occurrences recovered
/// from the dependency-free `// workspace-syn: expansion-uses(...)` comment form.
pub(super) fn read_parse_seed(path: &Path) -> Result<(String, syn::File, Vec<Occurrence>)> {
    let source = std::fs::read_to_string(path)?;
    let parsed = syn::parse_file(&source).map_err(|e| Error::Parse {
        path: path.to_path_buf(),
        source: e,
    })?;
    let seed = comment_expansion_uses_occurrences(&source, path);
    Ok((source, parsed, seed))
}

impl ModuleContents {
    /// Merge another module's collected contents into this one — the single merge
    /// point for the `include!` splice. The destructuring binding is load-bearing:
    /// a field added to [`ModuleContents`] later won't compile here until it's
    /// explicitly merged, so the splice path can't silently drop a generated
    /// file's data. `cfg_features` is re-deduped to preserve the set semantics it
    /// had as a `BTreeSet` before the final `Vec` collect.
    pub(super) fn absorb(&mut self, other: ModuleContents) {
        let ModuleContents {
            items,
            submodules,
            use_bindings,
            broken_mod_decls,
            cfg_features,
            occurrences,
            glob_reexports,
            signature_exposures,
            fact_references,
            fact_provenance,
            generated_files,
        } = other;
        self.items.extend(items);
        self.submodules.extend(submodules);
        self.use_bindings.extend(use_bindings);
        self.broken_mod_decls.extend(broken_mod_decls);
        self.cfg_features.extend(cfg_features);
        self.cfg_features.sort();
        self.cfg_features.dedup();
        self.occurrences.extend(occurrences);
        self.glob_reexports.extend(glob_reexports);
        self.signature_exposures.extend(signature_exposures);
        self.fact_references.extend(fact_references);
        self.fact_provenance.extend(fact_provenance);
        self.generated_files.extend(generated_files);
    }
}

/// Apply a plugin's [`plugins::Lowered`] result for an item-position macro: run
/// the baseline token scan, append the structured occurrences, or both. Factored
/// out of [`collect_module_contents`]'s main loop to keep its complexity in check.
pub(super) fn apply_lowered(
    lowered: plugins::Lowered,
    tokens: &proc_macro2::TokenStream,
    file: &Path,
    occurrences: &mut Vec<Occurrence>,
) {
    match lowered {
        plugins::Lowered::TokenScan => extract_macro_paths(tokens.clone(), file, occurrences),
        plugins::Lowered::Exact(occs) => occurrences.extend(occs),
        plugins::Lowered::ScanPlus(occs) => {
            extract_macro_paths(tokens.clone(), file, occurrences);
            occurrences.extend(occs);
        }
    }
}

/// Resolve an item-position `include!(...)` to the generated file it pulls in, or
/// `None` if the macro isn't `include!` or its argument can't be const-folded to
/// an existing path. A relative path resolves against the directory of the
/// including file (`parent_file`).
pub(super) fn resolve_include_site(
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
/// merged contents (`None` when there is nothing to splice). Factored out of
/// [`collect_module_contents`] to keep that function's complexity in check.
///
/// Each generated file is collected recursively against the including module's
/// *combined* lexical scope (`ambient` = its siblings + imports), because an
/// `include!` pastes into that scope — so a reference from generated code to a
/// handwritten sibling or a parent import resolves instead of leaking as an
/// `unused-*` false positive. The included items belong to *this* module (an
/// `include!` is not a submodule), so they share `mod_dir`/`parent_canonical`;
/// `parent_file` is the generated file itself, so every spliced span is stamped
/// with it — the key the diagnostic pipeline uses to recognize generated code.
///
/// Best-effort: an unreadable / unparsable generated file is skipped rather than
/// failing the load. Cyclic and *fan-out* cyclic includes are broken by the
/// `inc.ancestry` chain set (with `MAX_INCLUDE_DEPTH` as a backstop); a file
/// included twice into the same module is spliced once.
#[allow(clippy::too_many_arguments)]
pub(super) fn splice_includes(
    include_sites: Vec<PathBuf>,
    inc: IncludeCtx<'_>,
    in_inline: bool,
    mod_dir: &Path,
    parent_canonical: &ResolvedPath,
    marker_crates: &[String],
    sibling_names: &HashSet<String>,
    ambient_bindings: Vec<UseBinding>,
    has_glob_import: bool,
) -> Option<ModuleContents> {
    if include_sites.is_empty() {
        return None;
    }
    // The including module's resolution scope handed to every spliced file.
    // `siblings` is this module's combined set.
    let child_ambient = AmbientScope {
        siblings: sibling_names,
        use_bindings: &ambient_bindings,
        glob: has_glob_import,
    };
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
        let Ok((_, parsed, seed)) = read_parse_seed(&included_path) else {
            continue;
        };
        let Ok(spliced) = collect_module_contents(
            &parsed.items,
            &included_path,
            mod_dir,
            parent_canonical,
            marker_crates,
            in_inline,
            seed,
            child,
            child_ambient,
        ) else {
            continue;
        };
        merged.generated_files.push(included_path);
        merged.absorb(spliced);
    }
    Some(merged)
}
