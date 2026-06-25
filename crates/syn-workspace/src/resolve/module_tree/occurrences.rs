//! Phase A/B occurrence handling: scan a regular or macro item body's token
//! stream for path references (Phase A, candidate selection) and resolve each
//! to a canonical [`ResolvedPath`] (Phase B: `crate`/`self`/`super` peeling,
//! use-binding substitution, sibling rewrite). The module-tree walk in the
//! parent module drives both phases via [`extract_code_paths`] and
//! [`resolve_occurrences_in_place`].

use std::collections::HashSet;
use std::path::Path;

use super::span_to_source_span;
use super::use_tree::{self, UseBinding};
use super::{Occurrence, Origin, ResolvedPath, SourceSpan};

/// Candidate-select path references from a regular (non-macro) item body: scan
/// for `Ident :: Ident (:: Ident)*` runs and emit each as a raw `Origin::Code`
/// [`Occurrence`] (segments + span). Resolution — crate/self/super peeling,
/// use-binding substitution, sibling rewrite — happens later and centrally in
/// [`resolve_occurrence`].
///
/// The only resolution-aware decision kept here is candidate SELECTION: a bare
/// single ident is emitted only if it names a `use`-binding (`local_name`) or a
/// same-module sibling item (`sibling_names`) — otherwise it's a local/prelude
/// name and is dropped; multi-segment runs are always emitted. `use_bindings`
/// and `sibling_names` are passed solely for that keep-filter; resolution is
/// deferred to [`resolve_occurrence`] (whose sibling branch turns a kept bare
/// sibling ident into `parent_canonical::Ident`).
///
/// `own_decl` is the span of the scanned item's own declaring ident, if any.
/// Since an item's declaration name is a same-module sibling of itself, the
/// keep-filter would record it as a reference to itself; we drop exactly that
/// one token (matched by span) so a never-used item isn't seen as referencing
/// itself. Genuine refs (recursion, a sibling's bare reference) sit at other
/// spans and are kept.
///
/// Keeping bare sibling names can still record a spurious same-crate ref when a
/// local var/param collides with a sibling *name* (most likely a sibling fn —
/// types are PascalCase). That only ever *suppresses* a lint finding (never
/// invents a reference into another crate, so it can't dent the cross-crate SCIP
/// gate), so the precision-for-recall trade is worth it.
pub(super) fn extract_code_paths(
    tokens: proc_macro2::TokenStream,
    use_bindings: &[UseBinding],
    sibling_names: &HashSet<String>,
    // When the surrounding module has a glob import (`use m::*;`), bare
    // single idents that match no binding or sibling are kept as
    // `Origin::GlobCandidate` occurrences for the Phase B `GlobImportPass`
    // (most are locals and never bind — see that pass's precision notes).
    keep_bare_for_glob: bool,
    file: &Path,
    own_decl: Option<&SourceSpan>,
    out: &mut Vec<Occurrence>,
) {
    let stream: Vec<proc_macro2::TokenTree> = tokens.into_iter().collect();
    let mut i = 0;
    while i < stream.len() {
        if let proc_macro2::TokenTree::Ident(first) = &stream[i] {
            let (segments, j) = consume_path_run(&stream, i);
            // A bare single-ident macro *invocation* — `foo!(…)` / `foo![…]` /
            // `foo!{…}`, i.e. `Ident` then `!` then a delimited group — resolves
            // in the macro namespace, where an exported `macro_rules!` is
            // crate-global. The central path resolver doesn't model that, so emit
            // it as `Origin::MacroCall` for the core `MacroCallPass` to bind to a
            // same-crate definition (the macro's args, in the group, are still
            // scanned via the group recursion below). Multi-segment macro paths
            // (`m::foo!`) stay ordinary `Origin::Code` runs handled by the
            // keep-filter, so this never disturbs the macro-vs-crate distinction
            // the sibling exclusion guards (e.g. `log` in `log::debug!`).
            let is_macro_call = segments.len() == 1
                && matches!(stream.get(j), Some(proc_macro2::TokenTree::Punct(p)) if p.as_char() == '!')
                && matches!(stream.get(j + 1), Some(proc_macro2::TokenTree::Group(_)));
            if is_macro_call {
                out.push(Occurrence {
                    segments,
                    path: None,
                    span: Some(span_to_source_span(file, first.span())),
                    origin: Origin::MacroCall,
                });
                i = j;
                continue;
            }
            // Candidate selection only — resolution happens centrally in
            // `resolve_occurrence`. Keep multi-segment runs, plus single idents
            // that match a use-binding's local name (the binding set is needed
            // here, but the substitution itself is deferred to resolution).
            let keep = segments.len() >= 2
                || (segments.len() == 1
                    && (use_bindings.iter().any(|b| b.local_name == segments[0])
                        || sibling_names.contains(&segments[0])));
            // GlobCandidate filter: skip keywords/primitives, idents in
            // field/method position (preceded by `.`), lifetimes (preceded
            // by `'`), and names declaring a binding/field/bound (followed
            // by a lone `:` — a `::` run was already consumed above, and a
            // turbofish's first `:` is Joint, so a lone Alone `:` is left).
            let glob_candidate = !keep
                && keep_bare_for_glob
                && segments.len() == 1
                && is_glob_candidate_name(&segments[0])
                && !matches!(
                    (i > 0).then(|| &stream[i - 1]),
                    Some(proc_macro2::TokenTree::Punct(p)) if p.as_char() == '.' || p.as_char() == '\''
                )
                && !matches!(
                    stream.get(j),
                    Some(proc_macro2::TokenTree::Punct(p))
                        if p.as_char() == ':' && p.spacing() == proc_macro2::Spacing::Alone
                );
            if keep || glob_candidate {
                let occ_span = span_to_source_span(file, first.span());
                // Drop the item's own declaration name — a single-ident
                // occurrence at the declaring ident's span is the definition,
                // not a use of itself.
                let is_own_decl = segments.len() == 1 && own_decl == Some(&occ_span);
                if !is_own_decl {
                    out.push(Occurrence {
                        segments,
                        path: None,
                        span: Some(occ_span),
                        origin: if keep {
                            Origin::Code
                        } else {
                            Origin::GlobCandidate
                        },
                    });
                }
            }
            i = j;
            continue;
        }
        if let proc_macro2::TokenTree::Group(group) = &stream[i] {
            extract_code_paths(
                group.stream(),
                use_bindings,
                sibling_names,
                keep_bare_for_glob,
                file,
                own_decl,
                out,
            );
        }
        i += 1;
    }
}

/// Consume a path-run `Ident (:: Ident)*` beginning at `stream[start]` (which
/// the caller has already matched as an `Ident`). Returns the dotted segments
/// and the index just past the run (the next unconsumed token).
///
/// Shared by [`extract_code_paths`] and the macro-body scanner
/// ([`crate::macros::autodetect::extract_macro_paths`]) so both see identical
/// run boundaries — a change to run detection (turbofish spacing, raw idents,
/// …) lands in both channels at once.
pub(crate) fn consume_path_run(
    stream: &[proc_macro2::TokenTree],
    start: usize,
) -> (Vec<String>, usize) {
    let proc_macro2::TokenTree::Ident(first) = &stream[start] else {
        // Caller guarantees an `Ident` at `start`; stay defensive rather than panic.
        return (Vec::new(), start);
    };
    let mut segments = vec![first.to_string()];
    let mut j = start + 1;
    while let (Some(p1), Some(p2), Some(next)) =
        (stream.get(j), stream.get(j + 1), stream.get(j + 2))
    {
        let (proc_macro2::TokenTree::Punct(a), proc_macro2::TokenTree::Punct(b)) = (p1, p2) else {
            break;
        };
        if a.as_char() != ':' || b.as_char() != ':' {
            break;
        }
        let proc_macro2::TokenTree::Ident(next) = next else {
            break;
        };
        segments.push(next.to_string());
        j += 3;
    }
    (segments, j)
}

/// Whether a `use` tree contains a glob leaf anywhere (`use a::b::*;`,
/// `use a::{b, c::*};`). Drives the per-module `GlobCandidate` capture gate
/// in [`collect_module_contents`].
pub(super) fn use_tree_has_glob(tree: &syn::UseTree) -> bool {
    match tree {
        syn::UseTree::Glob(_) => true,
        syn::UseTree::Path(p) => use_tree_has_glob(&p.tree),
        syn::UseTree::Group(g) => g.items.iter().any(use_tree_has_glob),
        syn::UseTree::Name(_) | syn::UseTree::Rename(_) => false,
    }
}

/// Idents that can never be a glob-imported item name — Rust keywords (strict,
/// reserved, and the contextual ones that show up in token position) plus
/// primitive type names. Filters the `GlobCandidate` capture in
/// [`extract_code_paths`].
fn is_glob_candidate_name(name: &str) -> bool {
    const NON_CANDIDATES: &[&str] = &[
        // Strict keywords.
        "as",
        "break",
        "const",
        "continue",
        "crate",
        "dyn",
        "else",
        "enum",
        "extern",
        "false",
        "fn",
        "for",
        "if",
        "impl",
        "in",
        "let",
        "loop",
        "match",
        "mod",
        "move",
        "mut",
        "pub",
        "ref",
        "return",
        "self",
        "Self",
        "static",
        "struct",
        "super",
        "trait",
        "true",
        "type",
        "unsafe",
        "use",
        "where",
        "while",
        "async",
        "await",
        // Reserved / contextual.
        "abstract",
        "become",
        "box",
        "do",
        "final",
        "macro",
        "override",
        "priv",
        "try",
        "typeof",
        "union",
        "unsized",
        "virtual",
        "yield",
        "macro_rules",
        "raw",
        "_",
        // Primitives.
        "bool",
        "char",
        "str",
        "u8",
        "u16",
        "u32",
        "u64",
        "u128",
        "usize",
        "i8",
        "i16",
        "i32",
        "i64",
        "i128",
        "isize",
        "f32",
        "f64",
    ];
    !NON_CANDIDATES.contains(&name)
}

/// Apply `crate::` / `self::` / `super::` prefix peeling to the leading
/// segments of a path, mutating the iterator in place. The remaining segments
/// (everything after the last consumed `crate`/`self`/`super`) are left in
/// the iterator for the caller to handle.
///
/// Returns `Some((prefix, started))` where `started` is `true` iff at least
/// one leading segment was peeled (in which case the resolver should NOT
/// apply use-binding / sibling lookups to the next segment — those rules
/// only apply to externally-anchored paths). Returns `None` if a `super::`
/// would escape the crate root: rustc errors on those, so we drop the
/// reference rather than misattribute it to `crate::remaining`.
fn peel_path_prefix(
    iter: &mut std::iter::Peekable<std::vec::IntoIter<String>>,
    scope: &use_tree::Scope,
) -> Option<(Vec<String>, bool)> {
    let mut prefix: Vec<String> = Vec::new();
    let mut started = false;

    while let Some(head) = iter.peek().cloned() {
        match head.as_str() {
            "crate" if !started => {
                prefix.clear();
                prefix.push(scope.crate_name.clone());
                started = true;
                iter.next();
            }
            "self" if !started => {
                prefix.clear();
                prefix.push(scope.crate_name.clone());
                prefix.extend(scope.module_path.iter().cloned());
                started = true;
                iter.next();
            }
            "super" => {
                if !started {
                    prefix.push(scope.crate_name.clone());
                    prefix.extend(scope.module_path.iter().cloned());
                    started = true;
                }
                if prefix.len() <= 1 {
                    return None;
                }
                prefix.pop();
                iter.next();
            }
            _ => break,
        }
    }
    Some((prefix, started))
}

/// Resolve a code-path's leading segment through (in order):
/// crate/self/super peeling, use-binding substitution, sibling lookup,
/// then external-crate fallback. See [`extract_code_paths`] for context.
///
/// Exposed to the sibling `signature` module so the signature-exposure walk
/// resolves type paths through the *identical* logic ordinary references use —
/// guaranteeing its canonicals line up with the occurrence graph.
pub(super) fn resolve_code_path(
    segments: Vec<String>,
    scope: &use_tree::Scope,
    siblings: &HashSet<String>,
    use_bindings: &[UseBinding],
    parent_canonical: &ResolvedPath,
) -> Option<ResolvedPath> {
    if segments.is_empty() {
        return None;
    }
    let mut iter = segments.into_iter().peekable();
    let (mut prefix, started) = peel_path_prefix(&mut iter, scope)?;
    let remaining: Vec<String> = iter.collect();
    if remaining.is_empty() {
        return None;
    }
    let first_remaining = remaining.first()?.clone();

    if !started {
        // Use-binding shadows siblings and externals.
        if let Some(binding) = use_bindings
            .iter()
            .find(|b| b.local_name == first_remaining)
        {
            let mut segs = binding.canonical.segments().to_vec();
            segs.extend(remaining.into_iter().skip(1));
            return Some(ResolvedPath::new(segs));
        }
        if siblings.contains(&first_remaining) {
            let mut segs = parent_canonical.segments().to_vec();
            segs.extend(remaining);
            return Some(ResolvedPath::new(segs));
        }
        // Unmatched single segment — almost always a local var or prelude
        // name, not a cross-crate reference. Drop to avoid noise.
        if remaining.len() == 1 {
            return None;
        }
    }
    prefix.extend(remaining);
    Some(ResolvedPath::new(prefix))
}

/// Apply the same crate/self/super peeling and sibling-prepend logic that
/// `bindings_from_use` does, but for a path extracted from raw tokens.
pub(crate) fn resolve_macro_path(
    segments: Vec<String>,
    scope: &use_tree::Scope,
    siblings: &HashSet<String>,
    parent_canonical: &ResolvedPath,
) -> Option<ResolvedPath> {
    if segments.is_empty() {
        return None;
    }
    let mut iter = segments.into_iter().peekable();
    let (mut prefix, started) = peel_path_prefix(&mut iter, scope)?;
    let remaining: Vec<String> = iter.collect();
    if remaining.is_empty() {
        return None;
    }
    let first_remaining = remaining.first()?.clone();
    if !started && siblings.contains(&first_remaining) {
        // Sibling local — prepend the surrounding module's canonical.
        let mut segs = parent_canonical.segments().to_vec();
        segs.extend(remaining);
        return Some(ResolvedPath::new(segs));
    }
    prefix.extend(remaining);
    Some(ResolvedPath::new(prefix))
}

/// Phase B: fill in each occurrence's canonical `path` in place. An occurrence
/// that already carries a `path` is left as-is — only Tier-H assertions do this,
/// pre-setting an absolute implied path at extraction; re-resolving would route
/// a canonical path back through use-binding substitution. Every other origin
/// arrives with `path == None`, so the guard is behaviour-neutral for them.
pub(super) fn resolve_occurrences_in_place(
    occurrences: &mut [Occurrence],
    scope: &use_tree::Scope,
    use_bindings: &[UseBinding],
    sibling_names: &HashSet<String>,
    parent_canonical: &ResolvedPath,
) {
    for occ in occurrences {
        if occ.path.is_some() {
            continue;
        }
        occ.path = resolve_occurrence(occ, scope, use_bindings, sibling_names, parent_canonical);
    }
}

/// Canonicalize one raw occurrence — the single home for the crate/self/super
/// peeling + use-binding substitution + sibling rewrite that used to be smeared
/// across the extractors. Dispatches on [`Origin`]: `Code` paths resolve against
/// the surrounding `use` bindings; `Macro` paths resolve at the defining scope
/// (no use-binding substitution); `GlobUse`/`ExternCrate` segments are already
/// resolved at extraction, so they pass through unchanged.
fn resolve_occurrence(
    occ: &Occurrence,
    scope: &use_tree::Scope,
    use_bindings: &[UseBinding],
    siblings: &HashSet<String>,
    parent_canonical: &ResolvedPath,
) -> Option<ResolvedPath> {
    match occ.origin {
        Origin::Code => resolve_code_path(
            occ.segments.clone(),
            scope,
            siblings,
            use_bindings,
            parent_canonical,
        ),
        Origin::Macro => {
            resolve_macro_path(occ.segments.clone(), scope, siblings, parent_canonical)
        }
        // Deferred to a Phase B resolver plugin (`global_facts`): a bare
        // framework-component name carries no scope the central resolver can use
        // without wrongly binding it.
        Origin::Component => None,
        // Deferred to the core Phase B `MacroCallPass`: a bare macro invocation
        // `foo!(…)` resolves in the *macro* namespace (crate-global for an
        // exported `macro_rules!`), which path resolution doesn't model.
        Origin::MacroCall => None,
        // Deferred to the core Phase B `GlobImportPass`: a bare ident in a
        // glob-importing module can only be bound once the glob target's
        // module tree exists.
        Origin::GlobCandidate => None,
        Origin::GlobUse | Origin::ExternCrate => Some(ResolvedPath::new(occ.segments.clone())),
    }
}
