//! The fast tier's syntactic tree model: [`Module`] and its walk iterator,
//! the cargo [`Target`]/[`TargetKind`].
//! Built by the lean walker in `module_tree` during [`FastModel`] load.
//!
//! This is the *syntactic* slice of syn-workspace's resolved model — the
//! fields the build-free consumers (feature-drift, module-tree, the directive
//! scanner's parse cache, the generated-file drop) actually read. The
//! resolver-feeding fields (items, use bindings, occurrences, signature
//! exposures) are deliberately absent: name resolution is the semantic tier's
//! job.
//!
//! [`FastModel`]: super::FastModel

use std::path::PathBuf;

/// A module within a crate target. Modules form a tree rooted at the target's
/// source root (`lib.rs` / `main.rs` / `tests/it.rs` / …).
#[derive(Debug, Clone)]
pub struct Module {
    /// File backing this module. An inline `mod foo { ... }` block carries its
    /// *parent's* file — its content lives there.
    pub file: PathBuf,
    /// Feature names referenced via `#[cfg(feature = "...")]` or
    /// `#[cfg_attr(feature = "...", ...)]` on any item declared in this
    /// module, plus any `feature = "..."` token sequence inside an
    /// item-position bang-macro invocation (the expansion may weave the gate
    /// into generated items — coarse by design, fail-toward-not-flagging).
    /// Outer attributes only — feature gates inside function bodies are not
    /// extracted here. Deduped, sorted lexicographically.
    pub cfg_features: Vec<String>,
    /// Absolute paths of files this module's source *names* but does not own as
    /// a submodule: the targets of `include_str!` / `include_bytes!`, and of an
    /// `include!` outside item position (an expression or initializer, e.g. a
    /// generated lookup table). Only existing files are recorded.
    ///
    /// They are reached — rustc reads every one — but they are not generated
    /// *code* in the surgery sense (no items, no `use` declarations), so they
    /// deliberately stay out of [`Self::generated_files`], whose meaning is
    /// "spliced source; do not edit".
    pub named_files: Vec<PathBuf>,
    /// Absolute paths of files spliced into this module via `include!(...)`
    /// (generated code). The included items live directly in this module (an
    /// `include!` is not a submodule), so these files are *not* the `file` of
    /// any node; recording them here lets the module-tree integrity check
    /// treat them as reached and the diagnostic pipeline mark findings
    /// anchored in them as generated. Empty for the overwhelmingly common
    /// no-`include!` module.
    pub generated_files: Vec<PathBuf>,
    /// Crate names referenced inside this module's rust-compiling doc-test
    /// fences (see `doc_fences`). A dep used only in a doc example is
    /// genuinely used — the doc-test won't compile without it — but doc-test
    /// code is a separate compilation unit the rustc IR never sees, so the
    /// dependency lint reads this syntactic signal instead.
    pub doctest_crate_refs: std::collections::HashSet<String>,
    pub submodules: Vec<Module>,
}

impl Module {
    /// Recursively iterate this module and all its submodules, depth-first,
    /// root first. The most common entry point for callers that need to
    /// scan every module under a crate target.
    pub fn walk(&self) -> impl Iterator<Item = &Module> + '_ {
        ModuleWalk::new(self)
    }
}

/// Kind of a Cargo target. Library crate-types (`lib`/`rlib`/`dylib`/
/// `cdylib`/`staticlib`) are coalesced into [`TargetKind::Lib`] since
/// downstream consumers rarely distinguish them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetKind {
    Lib,
    ProcMacro,
    Bin,
    Example,
    Test,
    Bench,
    BuildScript,
}

/// One Cargo target inside a crate: a `[lib]`, `[[bin]]`, `[[example]]`,
/// `[[test]]`, `[[bench]]`, proc-macro library, or `build.rs`. Each target
/// has its own module tree, since cargo compiles each as a separate crate.
#[derive(Debug, Clone)]
pub struct Target {
    pub kind: TargetKind,
    /// Target name from `Cargo.toml` (or auto-derived for path-discovered
    /// targets).
    pub name: String,
    /// Absolute path to the target's root source file (e.g.
    /// `…/src/lib.rs`, `…/src/main.rs`, `…/build.rs`,
    /// `…/tests/integration.rs`).
    pub src_path: PathBuf,
    /// Module tree rooted at `src_path`.
    pub root: Module,
}

impl Target {
    /// Recursively iterate every module in this target's tree, root first.
    pub(crate) fn all_modules(&self) -> impl Iterator<Item = &Module> + '_ {
        self.root.walk()
    }
}

/// Recursive iterator over modules in a tree, yielding the root first
/// then descending into submodules depth-first. The public entry point
/// is [`Module::walk`].
struct ModuleWalk<'a> {
    stack: Vec<&'a Module>,
}

impl<'a> ModuleWalk<'a> {
    fn new(root: &'a Module) -> Self {
        Self { stack: vec![root] }
    }
}

impl<'a> Iterator for ModuleWalk<'a> {
    type Item = &'a Module;

    fn next(&mut self) -> Option<Self::Item> {
        let module = self.stack.pop()?;
        for sub in module.submodules.iter().rev() {
            self.stack.push(sub);
        }
        Some(module)
    }
}
