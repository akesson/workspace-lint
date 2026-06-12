//! Read-only queries over a loaded [`Workspace`]: accessors, the
//! external-reachability walk, and the reference-aggregation helpers consumers
//! use. A second `impl Workspace` block (the loader is in `workspace`); it reads
//! the struct's `pub(super)` fields.

use std::path::{Path, PathBuf};

use super::re_export;
use super::{
    Crate, Error, LoadWarning, ResolvedPath, Result, Target, TargetKind, Visibility, Workspace,
};

impl Workspace {
    /// Non-fatal issues collected during [`Workspace::load`]. Typically
    /// auxiliary targets (test/example/bench/build-script) that failed
    /// to parse — the primary lib/bin/proc-macro target's failure
    /// propagates as `Err` rather than landing here.
    ///
    /// Empty when nothing went wrong. Callers decide whether to log,
    /// print, or ignore the entries; this library never writes to stderr.
    pub fn warnings(&self) -> &[LoadWarning] {
        &self.warnings
    }

    /// Parsed root `Cargo.toml`. Carries the `[workspace.dependencies]`
    /// table (queried by centralized-dep analyses) and the raw source
    /// bytes (useful for comment-based directive scanners).
    pub fn root_manifest(&self) -> &crate::manifest::Manifest {
        &self.root_manifest
    }

    /// Register canonical paths that an external macro's expansion is
    /// known to reference. Each call appends; the underlying
    /// [`std::collections::HashSet`] dedupes. Typically called once after
    /// [`Workspace::load`], passing entries discovered by the caller
    /// (e.g. parsed from a config file, hardcoded, or learned at runtime).
    ///
    /// External-macro refs are treated as workspace-wide (broadcast to
    /// every crate) because `cargo_metadata` can't tell us which workspace
    /// crates actually invoke a given external macro. Callers that want
    /// per-crate scoping should track their own per-crate sets.
    pub fn register_external_macro_uses<I>(&mut self, paths: I)
    where
        I: IntoIterator<Item = ResolvedPath>,
    {
        self.external_macro_refs.extend(paths);
    }

    /// All workspace member crates plus referenced external crates.
    pub fn crates(&self) -> &[Crate] {
        &self.crates
    }

    /// Just the workspace member crates.
    pub fn members(&self) -> impl Iterator<Item = &Crate> {
        self.crates.iter().filter(|c| c.is_workspace_member)
    }

    /// Each workspace member paired with its primary unit (lib / proc-macro /
    /// main bin). Members without a primary target — proc-macro-less binaries
    /// without a `[[bin]]` entry, etc. — are skipped. The pair iterator
    /// subsumes the common "for member; if let Some(target) = lib_or_main"
    /// ladder.
    pub fn primary_units(&self) -> impl Iterator<Item = (&Crate, &Target)> + '_ {
        self.members()
            .filter_map(|c| c.lib_or_main().map(|t| (c, t)))
    }

    /// Look up a workspace member by its Cargo-form name (the value users
    /// write in `Cargo.toml`, hyphens preserved).
    pub fn member_by_name(&self, name: &str) -> Option<&Crate> {
        self.members().find(|c| c.name == name)
    }

    /// Look up a workspace member by its in-code form name (hyphens replaced
    /// with `_` — the form that appears as the leading segment of canonical
    /// paths).
    pub fn member_by_code_name(&self, code_name: &str) -> Option<&Crate> {
        self.members().find(|c| c.code_name() == code_name)
    }

    /// Workspace root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Strip the workspace root prefix from `path` and return a path
    /// relative to [`Self::root`]. Falls back to a clone of `path` when
    /// the input doesn't start with the workspace root — keeps callers
    /// (mostly diagnostic-builder lints) one-liners regardless of
    /// whether the input was inside or outside the workspace tree.
    ///
    /// `cargo_metadata` always hands back absolute paths for member
    /// `manifest_dir`s, but [`Workspace::load`] stores the user's input
    /// root unchanged — so when the caller invoked us with `.`, the
    /// stored root is `.` and a plain `strip_prefix` of an absolute
    /// `manifest_dir` would always fail. We canonicalize the root once
    /// before comparison (lazy: only when the literal strip misses) so
    /// the call site doesn't have to think about which form the root
    /// came in as.
    ///
    /// Use this for any anchor or rendered path that's expected to round-
    /// trip with a `# workspace-lint: …` suppression directive: the
    /// directive scanner emits anchors against workspace-relative paths,
    /// so any absolute `cargo_metadata`-derived path needs to come back
    /// through here before being passed to `at_crate` / `at_file` /
    /// `at_line`.
    pub fn crate_relative_path(&self, path: &Path) -> PathBuf {
        if let Ok(rel) = path.strip_prefix(&self.root) {
            return rel.to_path_buf();
        }
        // Two follow-up attempts handle the platform asymmetries that bite
        // in CI:
        //   - macOS: `/var` ↔ `/private/var` symlink dance — only one side
        //     canonicalizes.
        //   - Windows: `Path::canonicalize` returns a `\\?\` UNC prefix that
        //     the cargo_metadata-derived `manifest_dir` doesn't carry,
        //     so canonicalising only the root still leaves a mismatch.
        // Canonicalising both sides at once normalises away both.
        if let Ok(abs_root) = self.root.canonicalize() {
            if let Ok(rel) = path.strip_prefix(&abs_root) {
                return rel.to_path_buf();
            }
            if let Ok(abs_path) = path.canonicalize()
                && let Ok(rel) = abs_path.strip_prefix(&abs_root)
            {
                return rel.to_path_buf();
            }
        }
        path.to_path_buf()
    }

    /// Read and parse the given source file with `syn::parse_file`.
    ///
    /// `Module` only stores the file *path*, not the parsed AST — that
    /// keeps the whole workspace model `Send + Sync` (a `syn::File` is
    /// `Send` but not `Sync` because `proc-macro2::Span` contains
    /// `PhantomData<Rc<()>>`). Callers that need the AST call this
    /// helper on demand and cache as they see fit (typically a
    /// `HashMap<PathBuf, syn::File>` keyed by `module.file`).
    pub fn parse_file(&self, path: &Path) -> Result<syn::File> {
        let source = std::fs::read_to_string(path)?;
        syn::parse_file(&source).map_err(|e| Error::Parse {
            path: path.to_path_buf(),
            source: e,
        })
    }

    /// Resolve a path through any `pub use` re-export chain to its canonical
    /// definition site. Returns the path unchanged if no chain applies.
    pub fn resolve_canonical(&self, path: &ResolvedPath) -> ResolvedPath {
        self.re_exports.canonical(path)
    }

    /// Borrow the underlying re-export index — useful for callers that
    /// need to enumerate all known re-export edges.
    pub fn re_exports(&self) -> &re_export::ReExportIndex {
        &self.re_exports
    }

    /// A member crate's effective `[package] publish` declaration, resolving
    /// `publish.workspace = true` against the workspace root's
    /// `[workspace.package] publish`. Never returns
    /// [`Publish::Inherited`](crate::manifest::Publish::Inherited).
    ///
    /// Reports only what the manifests say; callers decide what an absent
    /// field means. Useful for distinguishing a crate with a real external
    /// API (`publish = true` / a registry list) from a workspace-internal one
    /// (`publish = false`, or — by a caller's policy — an absent field).
    pub fn resolved_publish(&self, krate: &Crate) -> crate::manifest::Publish {
        use crate::manifest::Publish;
        match krate.manifest.publish() {
            Publish::Inherited => match self.root_manifest.workspace_package_publish() {
                // A root that itself inherits is nonsensical; treat as absent.
                Publish::Inherited => Publish::Absent,
                resolved => resolved,
            },
            other => other,
        }
    }

    /// Returns `true` if `path` names an item in a crate that publishes a
    /// stable external API (a library or proc-macro), and every `mod` hop
    /// from the crate root down to (but not including) the item's own name
    /// is declared `pub mod` — i.e. the item is reachable from an external
    /// consumer through ordinary path resolution.
    ///
    /// Used by structural-fix lints (visibility, unused-pub) to refuse
    /// narrowing items that form part of a published crate's public API
    /// even when no in-workspace consumer references them: external
    /// consumers of a library crate live outside the resolver's view.
    ///
    /// Returns `false` for: items whose owning crate isn't a workspace
    /// member, items in a `[[bin]]`-only crate (binaries don't publish an
    /// API), items in non-primary targets (test/example/build-script),
    /// items the resolver couldn't walk to, and items inside a private or
    /// `pub(crate)` module hop.
    pub fn is_externally_reachable(&self, path: &ResolvedPath) -> bool {
        let segments = path.segments();
        // Need at least `[crate_name, item_name]` to talk about reachability.
        if segments.len() < 2 {
            return false;
        }
        let Some(krate) = self.member_by_code_name(&segments[0]) else {
            return false;
        };
        let Some(target) = krate.lib_or_main() else {
            return false;
        };
        // Only lib / proc-macro publish a stable API surface. Bin targets
        // don't expose items to external consumers, so pub items inside a
        // binary crate aren't "reachable from outside" in any meaningful
        // sense — the visibility lint should still suggest narrowing them.
        if !matches!(target.kind, TargetKind::Lib | TargetKind::ProcMacro) {
            return false;
        }
        // Walk every intermediate module hop (skip the crate-root segment
        // and the item name itself). Any non-Public hop breaks reachability.
        let intermediate = &segments[1..segments.len() - 1];
        let mut module = &target.root;
        for seg in intermediate {
            let Some(child) = module.submodules.iter().find(|m| m.name == *seg) else {
                return false;
            };
            if child.visibility != Visibility::Public {
                return false;
            }
            module = child;
        }
        true
    }

    /// Canonical paths reachable through macro expansions that could
    /// plausibly affect items inside `target_crate`. Built per call by
    /// unioning:
    ///
    /// 1. The target crate's own macros (intra-crate macros may reach
    ///    intra-crate items through expansion).
    /// 2. Macros from every workspace crate that references `target_crate`
    ///    — those are the crates whose code could invoke a macro whose body
    ///    points back at `target_crate`'s items.
    /// 3. External-macro entries registered via
    ///    [`Workspace::register_external_macro_uses`] (broadcast to every
    ///    target crate because we can't infer per-crate invocation).
    ///
    /// Reachability-narrowed: a macro body in an unrelated crate does not
    /// contribute. Useful for any consumer that needs to avoid attributing
    /// macro-mediated references to the wrong crate.
    pub fn macro_implicit_refs_for(
        &self,
        target_crate: &Crate,
    ) -> std::collections::HashSet<ResolvedPath> {
        let target_code = target_crate.code_name();
        let mut result = self.external_macro_refs.clone();
        if let Some(refs) = self.macro_refs_by_crate.get(&target_code) {
            result.extend(refs.iter().cloned());
        }
        for (referring_crate, refs) in &self.references_by_crate {
            if referring_crate == &target_code {
                continue;
            }
            let references_target = refs
                .iter()
                .any(|p| p.crate_name() == Some(target_code.as_str()));
            if references_target
                && let Some(macro_refs) = self.macro_refs_by_crate.get(referring_crate)
            {
                result.extend(macro_refs.iter().cloned());
            }
        }
        result
    }

    /// Set of canonical paths referenced from the named crate's regular
    /// code (function bodies, type signatures, etc.) plus its `use`
    /// declarations. `crate_name` is the in-code form (hyphens replaced
    /// with `_`).
    ///
    /// Prefer [`Workspace::references_from_crate`] when you have a
    /// [`Crate`] in hand — it handles the code-name conversion for you.
    ///
    /// Returns `None` if the crate is not a workspace member or the
    /// resolver couldn't load source for it.
    pub fn references_from(
        &self,
        crate_name: &str,
    ) -> Option<&std::collections::HashSet<ResolvedPath>> {
        self.references_by_crate.get(crate_name)
    }

    /// Same as [`Workspace::references_from`] but takes a [`Crate`] and
    /// applies the Cargo→code name conversion automatically.
    pub fn references_from_crate(
        &self,
        krate: &Crate,
    ) -> Option<&std::collections::HashSet<ResolvedPath>> {
        self.references_by_crate.get(&krate.code_name())
    }

    /// Crate names referenced inside `krate`'s doc-test code fences. A
    /// dependency that appears *only* here is still genuinely used — the
    /// doc-test won't compile without it — so the dependency lint unions this
    /// with [`Self::references_from_crate`]. Deliberately a separate channel:
    /// doc-test code is a separate compilation unit, so these refs must not
    /// reach `unused-pub`, `architecture`, or the SCIP projection. `None` when
    /// the crate has no doc-test references (or isn't a workspace member).
    pub fn doctest_dep_refs(&self, krate: &Crate) -> Option<&std::collections::HashSet<String>> {
        self.doctest_dep_refs_by_crate.get(&krate.code_name())
    }

    /// Iterator over every `(referring_crate, canonical_path)` reference
    /// pair across the workspace. Useful for building reverse indexes (e.g.
    /// "which crates reference symbol X?").
    pub fn iter_references(&self) -> impl Iterator<Item = (&str, &ResolvedPath)> {
        self.references_by_crate
            .iter()
            .flat_map(|(crate_name, refs)| refs.iter().map(move |r| (crate_name.as_str(), r)))
    }

    /// Like [`Workspace::iter_references`] but each path is already passed
    /// through the `pub use` chain in [`Workspace::re_exports`]. Yields one
    /// `(referring_crate, canonical_path)` pair per (referrer, canonical)
    /// combination — the index dedupes referrers, so two `use` statements
    /// from the same crate pointing at the same canonical produce one
    /// pair.
    ///
    /// Includes intra-crate referrers (a crate's own use of its own item).
    /// Callers that want cross-crate-only filter on
    /// `canonical.crate_name() != referring`.
    pub fn iter_canonical_references(&self) -> impl Iterator<Item = (&str, &ResolvedPath)> + '_ {
        self.canonical_refs_by_path
            .iter()
            .flat_map(|(path, crates)| crates.iter().map(move |c| (c.as_str(), path)))
    }

    /// Set of code-form crate names that reference `canonical` (after
    /// `pub use` chain resolution). `None` means no recorded reference at
    /// all. The returned set may include `canonical.crate_name()` itself
    /// when the defining crate references its own item. Prefix-credited: a
    /// recorded reference to `a::b::c` also answers for `a::b` (a
    /// `Type::assoc_fn()` call is a use of `Type`; a `module::item` path is
    /// a use of `module`).
    pub fn referring_crates(
        &self,
        canonical: &ResolvedPath,
    ) -> Option<&std::collections::HashSet<String>> {
        self.canonical_refs_by_path.get(canonical)
    }

    /// True iff `canonical` is referenced (directly, or as a prefix of a
    /// longer referenced path) from a *sibling target* of some package — an
    /// integration test, bench, example, or non-primary bin. Sibling
    /// targets link their package's library as an external crate, so they
    /// can only import `pub` items: an item whose only referrers are
    /// sibling targets must stay `pub` (narrowing it to `pub(crate)` would
    /// break the test/bench). `unused-pub` treats such references like
    /// cross-crate ones.
    pub fn referenced_from_sibling_target(&self, canonical: &ResolvedPath) -> bool {
        self.sibling_target_refs.contains(canonical)
    }
}
