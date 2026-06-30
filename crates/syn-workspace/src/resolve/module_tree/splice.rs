//! Helpers for the `include!` splice in the module-tree walk (`mod.rs`), factored
//! out to keep that file focused: the ambient lexical scope threaded into a
//! spliced file, and the shared read→parse→seed file prelude.

use std::collections::HashSet;
use std::path::Path;

use crate::macros::annotation::comment_expansion_uses_occurrences;
use crate::resolve::use_tree::UseBinding;
use crate::resolve::{Error, Occurrence, Result};

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
