//! The clippy-unmask guard: would narrowing a candidate to `pub(crate)`
//! *activate* a clippy lint the item is currently exempt from?
//!
//! Clippy's `avoid-breaking-exported-api` default suppresses several style
//! lints on exported items. Tightening `pub` → `pub(crate)` un-exports the
//! item (and every method reachable only through it), so a `--fix`-narrowed
//! tree can newly fail a `-D warnings` clippy gate — the 2026-07-05
//! LeaveDates validation hit `wrong_self_convention` and
//! `len_without_is_empty` this way. The unused-pub lint downgrades such
//! tighten suggestions to `MaybeIncorrect` (shown, never machine-applied) and
//! says why.
//!
//! The two replayed lints:
//!
//! - **`wrong_self_convention`** — clippy's naming table, replayed against
//!   the extractor-emitted [`DefInfo::self_kind`] / [`DefInfo::self_copy`]:
//!   `new`/`from_*` want no receiver, `into_*` wants `self`, `as_*` wants a
//!   reference, `is_*` wants `&self` or none, `to_mut`/`to_*_mut` want
//!   `&mut self`, and `to_*` wants `&self` — except on a `Copy` self type
//!   (non-trait), where it wants `self` by value. A reference-expecting slot
//!   also accepts by-value `self` on `Copy` types, mirroring clippy's
//!   `SelfKind::Ref` semantics.
//! - **`len_without_is_empty`** — a pub `len(&self)` assoc fn with no
//!   `is_empty` sibling.
//!
//! Both checks run on the candidate itself (an inherent method being
//! narrowed) and on a type/trait candidate's assoc-fn members (narrowing the
//! owner un-exports them all). Trait-*impl* methods are never consulted:
//! their conventions are the trait's, and clippy skips them too. The guard's
//! error direction is deliberate: a false hit only costs an auto-fix (the
//! narrow is still suggested), never a broken gate.
//!
//! This module also hosts the two rustc-`dead_code`-replay guards that share
//! the same narrowing rationale — [`Assembly::has_unreached_trait_member`]
//! (never-called trait members) and [`Assembly::has_unread_field`]
//! (write-only fields) — plus the member/field index construction they and
//! the unmask checks enumerate over.

use std::collections::BTreeMap;

use super::assembly::{Assembly, DefMap, FastMap};
use super::def_info::DefInfo;

/// Owner → assoc-fn members: inherent members hang off their nominal self
/// type, trait-declaration members off the trait. (Trait-impl members carry
/// neither handle, so they're absent by construction.)
pub(super) fn assoc_members_index(
    defs: &DefMap,
    trait_parent: &FastMap<String, String>,
) -> BTreeMap<String, Vec<String>> {
    let mut assoc_members: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, def) in defs {
        if def.kind != "fn" {
            continue;
        }
        let owner = def
            .self_type
            .as_deref()
            .or_else(|| trait_parent.get(key).map(String::as_str));
        if let Some(owner) = owner {
            assoc_members
                .entry(owner.to_string())
                .or_default()
                .push(key.clone());
        }
    }
    // `defs` iterates in insertion order (it is an `IndexMap`), so sort each
    // member list by key to make the enumeration deterministic and independent
    // of that order — `owner_unmask` returns the *first* violating member, so
    // its identity must not depend on fragment/item iteration order.
    for members in assoc_members.values_mut() {
        members.sort();
    }
    assoc_members
}

/// ADT → its field keys, for the dead-field narrow guard. A field's
/// declaration path is always `Type::field` — declared in the type's body,
/// so no impl-module rendering applies; an enum variant's field
/// (`Enum::Variant::field`) hops once more (variants aren't emitted).
/// Underscore-prefixed fields are `dead_code`-exempt and stay out.
pub(super) fn fields_of_index(
    defs: &DefMap,
    id_key: &FastMap<String, String>,
) -> BTreeMap<String, Vec<String>> {
    let mut fields_of: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, def) in defs {
        if def.kind != "field"
            || def
                .path
                .rsplit("::")
                .next()
                .is_some_and(|n| n.starts_with('_'))
        {
            continue;
        }
        let parent = &def.path[..def.path.rfind("::").map_or(0, |p| p)];
        let owner = id_key
            .get(parent)
            .or_else(|| parent.rfind("::").and_then(|p| id_key.get(&parent[..p])));
        if let Some(owner) = owner {
            fields_of
                .entry(owner.clone())
                .or_default()
                .push(key.clone());
        }
    }
    // Deterministic order regardless of `defs` (an `IndexMap`) iteration —
    // `has_unread_field` only `any()`s over these, but keep it order-stable.
    for fields in fields_of.values_mut() {
        fields.sort();
    }
    fields_of
}

/// Why a tighten must not be machine-applied: the clippy lint that would fire
/// on the narrowed tree, and the method it fires on.
#[derive(Debug, Clone)]
pub struct NarrowUnmask {
    /// The clippy lint name (`wrong_self_convention` / `len_without_is_empty`).
    pub lint: &'static str,
    /// The assoc fn the lint would anchor to.
    pub member: String,
}

impl Assembly {
    /// Does `trait_key`'s trait declare a member no edge reaches under
    /// `in_degree`? Narrowing such a trait to `pub(crate)` un-exempts it from
    /// rustc's `dead_code`, which then flags the never-called members — a
    /// `-D warnings` failure on the narrowed tree (the ChronoExt
    /// `checked_sub_signed`/`set_time` cluster from the 2026-07-05 LeaveDates
    /// validation). Members reached only via dispatch still carry edges to
    /// their decl (type-dependent resolution), so a zero-degree member really
    /// is dead weight.
    pub(super) fn has_unreached_trait_member(
        &self,
        trait_key: &str,
        in_degree: &BTreeMap<String, usize>,
    ) -> bool {
        self.trait_parent
            .iter()
            .filter(|(_, tk)| tk.as_str() == trait_key)
            .any(|(member, _)| !in_degree.contains_key(member))
    }

    /// Does `adt_key`'s type declare a field no READ edge reaches under
    /// `in_degree`? The field twin of
    /// [`has_unreached_trait_member`](Self::has_unreached_trait_member): a
    /// `pub` type's fields are exempt from rustc `dead_code`, a `pub(crate)`
    /// type's are not — narrowing a type with a write-only field (the
    /// `CacheLock.path` case from the 2026-07-06 re-validation) fails a
    /// `-D warnings` gate. The extractor emits field-read edges (field exprs
    /// off assignment-LHS, pattern bindings) but never counts struct-literal
    /// writes, mirroring `dead_code`'s read/write split. Underscore-prefixed
    /// fields are exempt, as rustc exempts them.
    pub(super) fn has_unread_field(
        &self,
        adt_key: &str,
        in_degree: &BTreeMap<String, usize>,
    ) -> bool {
        self.fields_of
            .get(adt_key)
            .is_some_and(|fields| fields.iter().any(|f| !in_degree.contains_key(f)))
    }

    /// The clippy lint (if any) that narrowing `key` would unmask — the
    /// per-candidate guard `pub_usage` folds into [`super::PubCandidate`].
    pub(super) fn narrow_unmask(&self, key: &str, def: &DefInfo) -> Option<NarrowUnmask> {
        match def.kind.as_str() {
            "fn" => self.fn_unmask(key, def),
            "struct" | "enum" | "union" | "trait" | "type" => self.owner_unmask(key),
            _ => None,
        }
    }

    /// An assoc fn being narrowed directly: its own convention, and the
    /// `len`-without-`is_empty` shape against its siblings.
    fn fn_unmask(&self, key: &str, def: &DefInfo) -> Option<NarrowUnmask> {
        let owner = def
            .self_type
            .as_deref()
            .or_else(|| self.trait_parent.get(key).map(String::as_str));
        self.member_violation(key, def, owner)
    }

    /// A type/trait being narrowed: every pub assoc-fn member it exports.
    fn owner_unmask(&self, key: &str) -> Option<NarrowUnmask> {
        for member in self.assoc_members.get(key)? {
            let mdef = &self.defs[member];
            if !mdef.public {
                continue; // never exported — clippy already judged it
            }
            if let Some(u) = self.member_violation(member, mdef, Some(key)) {
                return Some(u);
            }
        }
        None
    }

    /// The two replayed checks for one assoc fn. `owner` keys the sibling
    /// scan for `is_empty`.
    fn member_violation(
        &self,
        key: &str,
        def: &DefInfo,
        owner: Option<&str>,
    ) -> Option<NarrowUnmask> {
        let name = def.path.rsplit("::").next().unwrap_or_default();
        let self_kind = def.self_kind.as_deref()?;
        let copy = def.self_copy.unwrap_or(false);
        let trait_item = def.self_type.is_none() && self.trait_parent.contains_key(key);
        if convention_violation(name, self_kind, copy, trait_item) {
            return Some(NarrowUnmask {
                lint: "wrong_self_convention",
                member: name.to_string(),
            });
        }
        if name == "len"
            && self_kind == "ref"
            && let Some(owner) = owner
            && !self.has_assoc_fn(owner, "is_empty")
        {
            return Some(NarrowUnmask {
                lint: "len_without_is_empty",
                member: name.to_string(),
            });
        }
        None
    }

    fn has_assoc_fn(&self, owner: &str, name: &str) -> bool {
        self.assoc_members.get(owner).is_some_and(|members| {
            members
                .iter()
                .any(|m| self.defs[m].path.rsplit("::").next() == Some(name))
        })
    }
}

/// Clippy's `wrong_self_convention` table (first matching row wins), reduced
/// to the IR's `self_kind` vocabulary. Returns `true` when the receiver
/// violates the matched convention.
fn convention_violation(name: &str, self_kind: &str, copy: bool, trait_item: bool) -> bool {
    // How clippy's `SelfKind` slots match our vocabulary: a reference slot
    // also accepts by-value `self` on `Copy` types.
    let no = |k: &str| k == "none";
    let value = |k: &str| k == "value";
    let r#ref = |k: &str| k == "ref" || (copy && k == "value");
    let ref_mut = |k: &str| k == "ref_mut" || (copy && k == "value");

    if name == "new" {
        return !no(self_kind);
    }
    if name == "to_mut" {
        return !ref_mut(self_kind);
    }
    if let Some(rest) = name.strip_prefix("to_") {
        if rest.ends_with("_mut") {
            return !ref_mut(self_kind);
        }
        return if copy && !trait_item {
            !value(self_kind)
        } else {
            !r#ref(self_kind)
        };
    }
    if name.starts_with("as_") {
        return !(r#ref(self_kind) || ref_mut(self_kind));
    }
    if name.starts_with("from_") {
        return !no(self_kind);
    }
    if name.starts_with("into_") {
        return !value(self_kind);
    }
    if name.starts_with("is_") {
        return !(r#ref(self_kind) || no(self_kind));
    }
    false
}
