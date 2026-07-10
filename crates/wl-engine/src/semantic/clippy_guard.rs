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

use super::SemanticModel;
use super::assembly::{Assembly, DefMap, FastMap};
use super::def_info::DefInfo;
use super::removal::RemovalSet;

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
/// (`Enum::Variant::field`) resolves to the variant fact (schema 13+) and
/// hops to the owning enum — the guards judge the ADT, matching rustc's
/// per-ADT `dead_code` grouping. Underscore-prefixed fields are
/// `dead_code`-exempt and stay out.
pub(super) fn fields_of_index(
    defs: &DefMap,
    id_key: &FastMap<String, String>,
) -> BTreeMap<String, Vec<String>> {
    landings_index(defs, "field", |parent| {
        let hop = || parent.rfind("::").and_then(|p| id_key.get(&parent[..p]));
        match id_key.get(parent) {
            // A variant-owned field's parent is the VARIANT fact — the
            // guard's owner is the enum, one segment up.
            Some(k) if defs[k].kind == "variant" => hop(),
            Some(k) => Some(k),
            None => hop(),
        }
    })
}

/// Enum → its variant keys, for the unconstructed-variant deletion veto —
/// [`fields_of_index`]'s twin. A variant's declaration path is always
/// `Enum::Variant`, declared in the enum's body. Underscore-prefixed
/// variants are `dead_code`-exempt and stay out, as rustc excludes them.
pub(super) fn variants_of_index(
    defs: &DefMap,
    id_key: &FastMap<String, String>,
) -> BTreeMap<String, Vec<String>> {
    landings_index(defs, "variant", |parent| id_key.get(parent))
}

/// The shared sweep behind the two indexes above: non-underscore defs of
/// `kind`, grouped under the owner key `owner_of` resolves their declaring
/// parent path to. Sorted so the enumeration is deterministic regardless of
/// `defs` (an `IndexMap`) iteration order — the guards only `any()`/probe
/// these, but keep them order-stable.
fn landings_index<'k>(
    defs: &DefMap,
    kind: &str,
    owner_of: impl Fn(&str) -> Option<&'k String>,
) -> BTreeMap<String, Vec<String>> {
    let mut index: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, def) in defs {
        if def.kind != kind
            || def
                .path
                .rsplit("::")
                .next()
                .is_some_and(|n| n.starts_with('_'))
        {
            continue;
        }
        let parent = &def.path[..def.path.rfind("::").map_or(0, |p| p)];
        if let Some(owner) = owner_of(parent) {
            index.entry(owner.clone()).or_default().push(key.clone());
        }
    }
    for landings in index.values_mut() {
        landings.sort();
    }
    index
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

/// Why a deletion must not proceed: the `-D warnings` failure it would unmask
/// on a **surviving** item. The deletion twin of [`NarrowUnmask`]: where
/// narrowing un-exports an item and activates lints on *it*, deleting an item
/// can activate lints on what it leaves behind — the 2026-07-07 LeaveDates
/// validation hit both shapes (`PopoverMenuClose.is_open` lost its only
/// reader; `PasswordData::is_empty` was deleted out from under a surviving
/// `len`). Keyed to the *offending deletion*, so the cascade can veto exactly
/// the removals that would break a `-D warnings` gate.
#[derive(Debug, Clone)]
pub enum DeletionUnmask {
    /// The deletion removes the last READ of a surviving type's non-pub
    /// field — rustc `dead_code` flags the now-write-only field.
    UnreadField {
        /// The surviving type's identity (display path).
        owner: String,
        /// The field's own name.
        field: String,
    },
    /// Deleting a pub `is_empty` while a pub `len(&self)` sibling survives —
    /// clippy `len_without_is_empty` fires on the survivor.
    LenWithoutIsEmpty {
        /// The owning type/trait's identity (display path).
        owner: String,
    },
    /// The deletion removes the last CONSTRUCTION of a surviving enum's
    /// variant — rustc `dead_code` flags the now-never-constructed variant
    /// (match arms naming it don't keep it alive).
    UnconstructedVariant {
        /// The surviving enum's identity (display path).
        owner: String,
        /// The variant's own name.
        variant: String,
    },
}

impl SemanticModel {
    /// The warnings the `trial` removal set would unmask on surviving items,
    /// attributed to the offending members of `newly` (this round's deletion
    /// seeds — earlier rounds were already judged). Every check fails toward
    /// vetoing: a false hit costs one kept item and an explanatory note,
    /// never a broken gate.
    ///
    /// - **Field reads** are judged in each type's home config (mirroring the
    ///   `Assembly::has_unread_field` narrow guard): a field key whose
    ///   trial-overlay in-degree is zero, whose owner survives, and whose
    ///   killing edges come from `newly` members flags those members.
    ///   Exported fields (pub field of a pub type) are skipped — `dead_code`
    ///   exempts them; pre-existing write-only fields have no `newly` edge to
    ///   attribute and stay the author's business.
    /// - **Variant constructions** ride the same sweep: variant keys receive
    ///   only expression-position construction edges (pattern mentions
    ///   project onto the enum, matching rustc's construction-only variant
    ///   liveness), so a variant key with zero trial-overlay in-degree on a
    ///   surviving enum flags the deleters of its last construction sites
    ///   (ripgrep's `Sorter::ByPath`, 2026-07-10 validation).
    /// - **`len`/`is_empty`** is judged in the first config defining the
    ///   deleted `is_empty`: only a *pub* `is_empty` masks clippy, and only a
    ///   surviving *pub* `len(&self)` re-triggers it. Deleting both together
    ///   is fine (no survivor to fire on).
    pub fn deletion_unmasks(
        &self,
        trial: &RemovalSet,
        newly: &[String],
    ) -> BTreeMap<String, DeletionUnmask> {
        let mut out: BTreeMap<String, DeletionUnmask> = BTreeMap::new();
        let newly_segs: Vec<(&str, Vec<&str>)> = newly
            .iter()
            .map(|id| (id.as_str(), id.split("::").collect()))
            .collect();
        let covering = super::covering_configs(&self.configs);

        for (ci, (_, asm)) in self.configs.iter().enumerate() {
            // Fields and variants share one landing-point map and one edge
            // sweep: both are non-tree-item defs whose zero-in-degree under
            // the trial overlay unmasks `dead_code` on the surviving owner —
            // fields lose their last READ, variants their last CONSTRUCTION
            // (the extractor lands only construction edges on variants).
            let mut owner_of: FastMap<&str, &str> = FastMap::default();
            for (owner, landings) in asm.fields_of.iter().chain(asm.variants_of.iter()) {
                for l in landings {
                    owner_of.insert(l.as_str(), owner.as_str());
                }
            }
            if owner_of.is_empty() {
                continue;
            }
            let overlay = asm.refold_excluding(trial);
            for frag in asm.archived_fragments() {
                for e in frag.references.iter() {
                    if e.import || !trial.covers(&e.from) {
                        continue;
                    }
                    let Some(key) = asm.resolve_key(e) else {
                        continue;
                    };
                    let Some(&owner_key) = owner_of.get(key) else {
                        continue;
                    };
                    let ldef = &asm.defs[key];
                    if overlay.in_degree.contains_key(key) {
                        continue;
                    }
                    let odef = &asm.defs[owner_key];
                    // `dead_code` only exempts a field/variant that is itself
                    // exported — pub field of a pub type (a variant inherits
                    // its enum's visibility); a pub field of a non-pub type
                    // is as flaggable as a private one.
                    if ldef.public && odef.public {
                        continue;
                    }
                    if covering.get(&odef.krate) != Some(&ci)
                        || trial.covers(&odef.path.split("::").collect::<Vec<_>>())
                    {
                        continue;
                    }
                    let name = ldef.path.rsplit("::").next().unwrap_or_default();
                    for (id, segs) in &newly_segs {
                        let covers = e.from.len() >= segs.len()
                            && segs
                                .iter()
                                .zip(e.from.iter())
                                .all(|(a, b)| *a == b.as_str());
                        if covers {
                            out.entry((*id).to_string()).or_insert_with(|| {
                                if ldef.kind == "variant" {
                                    DeletionUnmask::UnconstructedVariant {
                                        owner: odef.path.clone(),
                                        variant: name.to_string(),
                                    }
                                } else {
                                    DeletionUnmask::UnreadField {
                                        owner: odef.path.clone(),
                                        field: name.to_string(),
                                    }
                                }
                            });
                        }
                    }
                }
            }
        }

        for id in newly {
            if id.rsplit("::").next() != Some("is_empty") || out.contains_key(id) {
                continue;
            }
            for (_, asm) in &self.configs {
                let Some(key) = asm.key_for_identity(id) else {
                    continue;
                };
                let def = &asm.defs[key];
                if def.kind != "fn" || !def.public {
                    break; // a private `is_empty` never masked clippy
                }
                let owner = def
                    .self_type
                    .as_deref()
                    .or_else(|| asm.trait_parent.get(key).map(String::as_str));
                let Some(owner) = owner else { break };
                let Some(odef) = asm.defs.get(owner) else {
                    break;
                };
                if trial.covers(&odef.path.split("::").collect::<Vec<_>>()) {
                    break; // the owner goes too — nothing survives to fire on
                }
                let len_survives = asm.assoc_members.get(owner).is_some_and(|members| {
                    members.iter().any(|m| {
                        let md = &asm.defs[m];
                        md.public
                            && md.self_kind.as_deref() == Some("ref")
                            && md.path.rsplit("::").next() == Some("len")
                            && !trial.covers(&md.path.split("::").collect::<Vec<_>>())
                    })
                });
                if len_survives {
                    out.insert(
                        id.clone(),
                        DeletionUnmask::LenWithoutIsEmpty {
                            owner: odef.path.clone(),
                        },
                    );
                }
                break; // judged in the first config that defines it
            }
        }
        out
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
