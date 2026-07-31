//! Naming the fix a clone group calls for.
//!
//! Detection says *these regions are structurally identical*; the classifier
//! says *and here is what to do about it*. It reads each group's syntactic
//! shape (via the fast tier's [`MetaResolver`]) and, for whole-fn groups, the
//! semantic tier's call graph, and picks the one refactoring that fits — or
//! leaves the group [`Unclassified`](RefactoringClass::Unclassified), which
//! keeps the generic help verbatim. Classification never changes which groups
//! fire or at what level; it only reshapes the help line and appends a note.
//!
//! The merge family is the only class that consults the call graph. Its
//! contract is *IR-confirm*: two fns whose normalized tokens match are only
//! interchangeable if they also **resolve the same callees** — a locally
//! shadowed name (`let month_short = month_short(m);`) matches by structure but
//! calls a different def, so a differing callee set withholds the merge
//! ([`MergeWithheld`](RefactoringClass::MergeWithheld)) rather than advising a
//! rewrite that would not compile or would change behavior. Zero in-degree
//! alone never marks a copy dead either: only a fn whose category
//! `supports_deletion` may be advised deleted — a trait-impl method is reached
//! through dispatch the ref graph doesn't edge, and deleting it is E0046
//! (issue #141), so such a copy is spared with an explanatory note instead.

use std::collections::BTreeSet;
use std::path::PathBuf;

use wl_diagnostic::render::display_path;
use wl_engine::SemanticModel;
use wl_engine::fast::clones::meta::{FirstParam, FnMeta, FnOwner, MetaResolver};
use wl_engine::fast::clones::{CandidateKind, CloneGroup, Region};
use wl_engine::semantic::Category;

/// The generic help every group falls back to — literally the pre-classifier
/// message, kept for `MergeWithheld` / `Unclassified` and the no-classify path.
pub(crate) const GENERIC_HELP: &str =
    "extract the shared logic into one function the copies can call";

use super::MAX_LISTED;

/// A `file:line` location — a dead copy, or a call site to redirect.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Site {
    pub file: PathBuf,
    pub line: u32,
}

/// The outside call sites a merge would redirect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallSites {
    pub count: usize,
    /// The earliest call site (by file, line); `None` if every referrer is
    /// spanless (lowered-signature edges).
    pub first: Option<Site>,
}

/// A copy with zero outside in-degree that must NOT be advised dead: its
/// enclosing fn's category does not support deletion (a trait-impl method is
/// reached through the trait and required by its `impl` — deleting it is
/// E0046, issue #141).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SparedCopy {
    pub site: Site,
    /// `Trait::method` display when the implemented trait is
    /// workspace-resolvable; `None` for a foreign trait (`std`'s `Display`, …).
    pub trait_method: Option<String>,
}

/// The refactoring a clone group calls for. Classes 1–5 replace the generic
/// help; the last two keep it (and `MergeWithheld` adds a caution note).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RefactoringClass {
    /// Identical fns confirmed interchangeable — keep one, redirect callers.
    /// `spared`: copies with zero in-degree the delete guard held back.
    MergeIdenticalFns {
        call_sites: Option<CallSites>,
        spared: Vec<SparedCopy>,
    },
    /// Some copies have no outside referrer — delete the dead ones. `spared`
    /// as above (a mixed group renders both stories).
    DeleteDeadCopy {
        dead: Vec<Site>,
        spared: Vec<SparedCopy>,
    },
    /// The same method across impls of one trait — hoist to a default method.
    DefaultTraitMethod { trait_name: String, method: String },
    /// Free/inherent fns all taking one workspace type first — make it a method.
    MethodOnReceiverType { ty: String },
    /// Copies built from the same UI macro (`rsx!`) — extract a component.
    UiComponent { macro_name: String },
    /// Identical tokens but divergent resolved callees — merge withheld.
    MergeWithheld,
    /// No named fix — keep the generic help.
    Unclassified,
}

impl RefactoringClass {
    /// The help line for this class (generic for withheld / unclassified).
    pub(crate) fn help(&self) -> String {
        match self {
            Self::MergeIdenticalFns { .. } => {
                "these are copies of the same function — keep one and redirect the other call sites"
                    .to_string()
            }
            Self::DeleteDeadCopy { dead, .. } => dead_copy_help(dead),
            Self::DefaultTraitMethod { trait_name, method } => format!(
                "every copy implements `{trait_name}::{method}` — make it a default method on the trait"
            ),
            Self::MethodOnReceiverType { ty } => format!(
                "extract the shared logic into a method on `{ty}` — every copy takes it as the first parameter"
            ),
            Self::UiComponent { macro_name } => format!(
                "extract the shared `{macro_name}!` markup into one component the copies can render"
            ),
            Self::MergeWithheld | Self::Unclassified => GENERIC_HELP.to_string(),
        }
    }

    /// The notes this class appends after the divergence note (if any):
    /// call-sites first, then one per delete-guard-spared copy.
    pub(crate) fn notes(&self) -> Vec<String> {
        let mut out = Vec::new();
        match self {
            Self::MergeIdenticalFns { call_sites, spared } => {
                if let Some(cs) = call_sites {
                    out.push(call_sites_note(cs));
                }
                out.extend(spared.iter().take(MAX_LISTED).map(spared_copy_note));
            }
            Self::DeleteDeadCopy { spared, .. } => {
                out.extend(spared.iter().take(MAX_LISTED).map(spared_copy_note));
            }
            Self::MergeWithheld => out.push(
                "instances resolve different callees — the copies may not be interchangeable"
                    .to_string(),
            ),
            _ => {}
        }
        out
    }

    /// The kebab-case class name — the `--stats` readout's class column. The
    /// merge family (`merge-identical-fns` / `delete-dead-copy` /
    /// `merge-withheld`) is a call-graph verdict absent from the build-free
    /// `--stats` path, which only ever produces the syntactic classes.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::MergeIdenticalFns { .. } => "merge-identical-fns",
            Self::DeleteDeadCopy { .. } => "delete-dead-copy",
            Self::DefaultTraitMethod { .. } => "default-trait-method",
            Self::MethodOnReceiverType { .. } => "method-on-receiver-type",
            Self::UiComponent { .. } => "ui-component",
            Self::MergeWithheld => "merge-withheld",
            Self::Unclassified => "unclassified",
        }
    }
}

fn dead_copy_help(dead: &[Site]) -> String {
    let listed: Vec<String> = dead
        .iter()
        .take(MAX_LISTED)
        .map(|s| format!("{}:{}", display_path(&s.file), s.line))
        .collect();
    let more = if dead.len() > MAX_LISTED {
        format!(" and {} more", dead.len() - MAX_LISTED)
    } else {
        String::new()
    };
    if dead.len() == 1 {
        format!("the copy at {} is never referenced — delete it", listed[0])
    } else {
        format!(
            "the copies at {}{more} are never referenced — delete them",
            listed.join(", ")
        )
    }
}

fn spared_copy_note(s: &SparedCopy) -> String {
    let loc = format!("{}:{}", display_path(&s.site.file), s.site.line);
    match &s.trait_method {
        Some(m) => format!(
            "the copy at {loc} implements `{m}` — it is reached through the trait \
             despite having no direct caller, so it is not reported as dead"
        ),
        None => format!(
            "the copy at {loc} has no direct caller but implements a trait method — \
             deleting it would break the `impl`, so it is not reported as dead"
        ),
    }
}

fn call_sites_note(cs: &CallSites) -> String {
    let (noun, verb) = if cs.count == 1 {
        ("call site", "references")
    } else {
        ("call sites", "reference")
    };
    let first = match &cs.first {
        Some(s) => format!(" (first at {}:{})", display_path(&s.file), s.line),
        None => String::new(),
    };
    format!("{} {noun} {verb} the copies{first}", cs.count)
}

/// Classifies clone groups, holding the per-run resolvers. The semantic model
/// is optional: absent (the `--stats` path) the merge family is skipped and
/// only the syntactic classes are produced.
pub(crate) struct Classifier<'a> {
    resolver: MetaResolver<'a>,
    type_names: BTreeSet<String>,
    trait_names: BTreeSet<String>,
    component_macros: BTreeSet<String>,
    model: Option<&'a SemanticModel>,
}

impl<'a> Classifier<'a> {
    pub(crate) fn new(
        files: &'a [wl_engine::fast::clones::ScanFile],
        model: Option<&'a SemanticModel>,
        component_macros: &[String],
    ) -> Self {
        let mut resolver = MetaResolver::new(files);
        let type_names = resolver.defined_type_names().clone();
        let trait_names = resolver.defined_trait_names().clone();
        Self {
            resolver,
            type_names,
            trait_names,
            component_macros: component_macros.iter().cloned().collect(),
            model,
        }
    }

    /// The refactoring `group` calls for. `identical` — the group's instances
    /// differ at most in local names (divergence `params == 0`, no drift, or
    /// exact-literals mode) — gates the merge family: a fn group carrying
    /// extraction parameters is not "the same function".
    pub(crate) fn classify(&mut self, group: &CloneGroup, identical: bool) -> RefactoringClass {
        let metas = self.fn_metas(group);

        // 1. A trait method duplicated across impls → a default method.
        if let Some(ms) = &metas
            && let Some(c) = default_trait_method(ms, &self.trait_names)
        {
            return c;
        }
        // 2. Identical fns, IR-confirmed → merge / delete-dead-copy / withhold.
        if identical
            && let (Some(model), Some(ms)) = (self.model, &metas)
            && let Some(c) = merge_family(group, model, ms)
        {
            return c;
        }
        // 3. Built from a UI markup macro → extract a component.
        if let Some(macro_name) = self.ui_component(group) {
            return RefactoringClass::UiComponent { macro_name };
        }
        // 4. All take one workspace type first → a method on that type.
        if let Some(ms) = &metas
            && let Some(ty) = method_on_receiver(ms, &self.type_names)
        {
            return RefactoringClass::MethodOnReceiverType { ty };
        }
        RefactoringClass::Unclassified
    }

    /// Each instance's fn metadata, or `None` if the group is not a whole-fn
    /// group (block / statement-run) or any instance fails to resolve.
    fn fn_metas(&mut self, group: &CloneGroup) -> Option<Vec<FnMeta>> {
        if group.instances.first()?.kind != CandidateKind::Fn {
            return None;
        }
        group
            .instances
            .iter()
            .map(|inst| self.resolver.fn_meta(inst))
            .collect()
    }

    /// The component macro invoked by the group (its anchor represents the
    /// group — instances are structurally identical).
    fn ui_component(&mut self, group: &CloneGroup) -> Option<String> {
        let macros = self.resolver.macros_in(&group.instances[0]);
        self.component_macros.intersection(&macros).next().cloned()
    }
}

/// Class 1: every instance is a trait-impl method of the same name and trait,
/// and that trait is workspace-defined — the fix ("add a default method") is
/// only reachable if you own the trait. A foreign trait (`std`'s `TryFrom`,
/// `From`, …) can't take a default method here, so it falls through.
fn default_trait_method(ms: &[FnMeta], trait_names: &BTreeSet<String>) -> Option<RefactoringClass> {
    let first = ms.first()?;
    let FnOwner::TraitImpl { trait_path, .. } = &first.owner else {
        return None;
    };
    let (trait_path, method) = (trait_path.clone(), first.fn_name.clone());
    for m in ms {
        let FnOwner::TraitImpl { trait_path: tp, .. } = &m.owner else {
            return None;
        };
        if m.fn_name != method || !traits_match(tp, &trait_path) {
            return None;
        }
    }
    let trait_name = last_segment(&trait_path);
    if !trait_names.contains(trait_name) {
        return None;
    }
    Some(RefactoringClass::DefaultTraitMethod {
        trait_name: trait_name.to_string(),
        method,
    })
}

/// Class 4: every instance is a free / inherent fn whose first parameter is one
/// and the same workspace-defined type.
fn method_on_receiver(ms: &[FnMeta], type_names: &BTreeSet<String>) -> Option<String> {
    let mut ty: Option<&str> = None;
    for m in ms {
        if !matches!(m.owner, FnOwner::Free | FnOwner::Inherent { .. }) {
            return None;
        }
        let base = match &m.first_param {
            Some(FirstParam::Typed { base }) if !base.is_empty() => base.as_str(),
            _ => return None,
        };
        if !type_names.contains(base) {
            return None;
        }
        match ty {
            None => ty = Some(base),
            Some(t) if t == base => {}
            Some(_) => return None, // instances take different types — no single method
        }
    }
    ty.map(str::to_string)
}

/// Class 2: the merge family. Every instance is resolved to its fn identity;
/// differing callee sets withhold; otherwise the copies with no outside
/// referrer are dead (`DeleteDeadCopy`) and the rest merge (`MergeIdenticalFns`)
/// — except that only a `supports_deletion` category may be advised dead: a
/// trait-impl copy with zero in-degree is spared with a note (issue #141).
fn merge_family(
    group: &CloneGroup,
    model: &SemanticModel,
    metas: &[FnMeta],
) -> Option<RefactoringClass> {
    let mut encs = Vec::with_capacity(metas.len());
    for (inst, m) in group.instances.iter().zip(metas) {
        encs.push(model.enclosing_fn(&inst.file, m.byte_offset)?);
    }
    // IR-confirm: interchangeable only if the copies call the same things.
    let sets: Vec<_> = encs.iter().map(|e| model.callees_of(&e.identity)).collect();
    if sets.windows(2).any(|w| w[0] != w[1]) {
        return Some(RefactoringClass::MergeWithheld);
    }
    // A referrer that is itself a copy (mutual / self recursion) is not an
    // "outside" caller — the merge subsumes it.
    let group_ids: BTreeSet<&str> = encs.iter().map(|e| e.identity.as_str()).collect();
    let mut dead: Vec<Site> = Vec::new();
    let mut spared: Vec<SparedCopy> = Vec::new();
    let mut total = 0usize;
    let mut sites: Vec<Site> = Vec::new();
    for (inst, enc) in group.instances.iter().zip(&encs) {
        let outside: Vec<_> = model
            .references_to(&enc.identity)
            .into_iter()
            .filter(|r| !r.import && !r.in_signature && !group_ids.contains(r.from.as_str()))
            .collect();
        if outside.is_empty() {
            if enc.category.supports_deletion() {
                dead.push(site_of(inst));
            } else if enc.category == Category::TraitImpl {
                spared.push(SparedCopy {
                    site: site_of(inst),
                    trait_method: enc
                        .trait_method
                        .as_deref()
                        .map(last_two_segments)
                        .map(str::to_owned),
                });
            }
            // `Category::Other` (a trait-decl default method, a fn-local def)
            // is spared silently — no delete verdict, no trait-impl story.
        } else {
            total += outside.len();
            sites.extend(outside.iter().filter_map(|r| {
                r.span.as_ref().map(|s| Site {
                    file: PathBuf::from(&s.file),
                    line: s.line,
                })
            }));
        }
    }
    if !dead.is_empty() && dead.len() < group.instances.len() {
        return Some(RefactoringClass::DeleteDeadCopy { dead, spared });
    }
    let call_sites = (total > 0).then(|| CallSites {
        count: total,
        first: sites.into_iter().min(),
    });
    Some(RefactoringClass::MergeIdenticalFns { call_sites, spared })
}

fn site_of(region: &Region) -> Site {
    Site {
        file: region.file.clone(),
        line: region.line_start,
    }
}

/// Two written trait paths name the same trait: full equality, or a matching
/// final segment (an impl may write `Tr` where another writes `crate::Tr`).
fn traits_match(a: &str, b: &str) -> bool {
    a == b || last_segment(a) == last_segment(b)
}

fn last_segment(path: &str) -> &str {
    path.rsplit("::").next().unwrap_or(path)
}

/// `demo::Selectable::id` → `Selectable::id` — the `Trait::method` display the
/// default-trait-method help also uses.
fn last_two_segments(path: &str) -> &str {
    match path.rmatch_indices("::").nth(1) {
        Some((i, _)) => &path[i + 2..],
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use wl_engine::fast::clones::{Options, ScanFile, find_clones};

    use super::*;

    /// Classify a two-fn source syntactically (no semantic model — the merge
    /// family is skipped, exactly as `--stats` runs).
    fn syntactic(src: &str, component_macros: &[&str]) -> RefactoringClass {
        let files = vec![ScanFile {
            rel_path: PathBuf::from("src/lib.rs"),
            krate: "demo".into(),
            ast: syn::parse_file(src).expect("valid source"),
        }];
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
        let macros: Vec<String> = component_macros.iter().map(|s| s.to_string()).collect();
        let mut c = Classifier::new(&files, None, &macros);
        c.classify(group, true)
    }

    #[test]
    fn default_trait_method_across_impls() {
        let src = r#"
            struct A; struct B;
            trait Tr { fn go(&self); }
            impl Tr for A { fn go(&self) { let x = read(); let y = x + step(); done(y); } }
            impl Tr for B { fn go(&self) { let x = read(); let y = x + step(); done(y); } }
        "#;
        assert_eq!(
            syntactic(src, &["rsx"]),
            RefactoringClass::DefaultTraitMethod {
                trait_name: "Tr".into(),
                method: "go".into(),
            }
        );
    }

    /// A trait defined outside the workspace (here `std::convert::TryFrom`)
    /// can't take a default method, so identical impls of it are NOT
    /// `DefaultTraitMethod` — the class gates on trait locality.
    #[test]
    fn foreign_trait_impls_are_not_default_trait_method() {
        let src = r#"
            struct A; struct B;
            impl TryFrom<u16> for A {
                type Error = ();
                fn try_from(v: u16) -> Result<Self, ()> { let x = read(v); let y = x + step(); done(y); Err(()) }
            }
            impl TryFrom<u16> for B {
                type Error = ();
                fn try_from(v: u16) -> Result<Self, ()> { let x = read(v); let y = x + step(); done(y); Err(()) }
            }
        "#;
        assert_eq!(syntactic(src, &["rsx"]), RefactoringClass::Unclassified);
    }

    #[test]
    fn ui_component_from_markup_macro() {
        let src = r#"
            fn one() { let a = pre(); rsx! { div { "x" } span { "y" } } }
            fn two() { let a = pre(); rsx! { div { "x" } span { "y" } } }
        "#;
        assert_eq!(
            syntactic(src, &["rsx"]),
            RefactoringClass::UiComponent {
                macro_name: "rsx".into()
            }
        );
    }

    #[test]
    fn method_on_receiver_type_when_first_param_is_a_workspace_type() {
        let src = r#"
            struct Config { n: u8 }
            fn one(c: &Config) { let x = c.n; let y = x + step(); done(y); }
            fn two(c: &Config) { let x = c.n; let y = x + step(); done(y); }
        "#;
        assert_eq!(
            syntactic(src, &["rsx"]),
            RefactoringClass::MethodOnReceiverType {
                ty: "Config".into()
            }
        );
    }

    #[test]
    fn unclassified_when_no_named_fix_fits() {
        let src = r#"
            fn one() { let x = read(); let y = x + step(); let z = y * scale(); done(z); }
            fn two() { let x = read(); let y = x + step(); let z = y * scale(); done(z); }
        "#;
        assert_eq!(syntactic(src, &["rsx"]), RefactoringClass::Unclassified);
    }

    /// Precedence: a UI-component shape wins over method-on-receiver (a
    /// component fn takes its `*Props` first, but the fix is "extract a
    /// component", not "make it a method").
    #[test]
    fn ui_component_beats_method_on_receiver() {
        let src = r#"
            struct Props { n: u8 }
            fn one(p: &Props) { let a = p.n; rsx! { div { "x" } span { "y" } } }
            fn two(p: &Props) { let a = p.n; rsx! { div { "x" } span { "y" } } }
        "#;
        assert_eq!(
            syntactic(src, &["rsx"]),
            RefactoringClass::UiComponent {
                macro_name: "rsx".into()
            }
        );
    }

    /// A non-fn (block / run) group resolves no fn metadata, so no fn-shaped
    /// class fires — it stays unclassified.
    #[test]
    fn non_fn_group_is_unclassified() {
        let src = r#"
            fn holder() {
                {
                    let x = read();
                    let y = x + step();
                    done(y);
                }
                {
                    let x = read();
                    let y = x + step();
                    done(y);
                }
            }
        "#;
        let files = vec![ScanFile {
            rel_path: PathBuf::from("src/lib.rs"),
            krate: "demo".into(),
            ast: syn::parse_file(src).unwrap(),
        }];
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
            .find(|g| g.instances[0].kind != CandidateKind::Fn)
            .expect("a block/run group");
        let mut c = Classifier::new(&files, None, &["rsx".to_string()]);
        assert_eq!(c.classify(group, true), RefactoringClass::Unclassified);
    }

    /// Every class maps to its kebab-case `--stats` label. Nothing else in the
    /// crate exercises `label` (the `--stats` measure path is integration-only),
    /// so this pins the column names directly.
    #[test]
    fn label_names_every_class() {
        use RefactoringClass::*;
        assert_eq!(
            MergeIdenticalFns {
                call_sites: None,
                spared: vec![],
            }
            .label(),
            "merge-identical-fns"
        );
        assert_eq!(
            DeleteDeadCopy {
                dead: vec![],
                spared: vec![],
            }
            .label(),
            "delete-dead-copy"
        );
        assert_eq!(
            DefaultTraitMethod {
                trait_name: String::new(),
                method: String::new(),
            }
            .label(),
            "default-trait-method"
        );
        assert_eq!(
            MethodOnReceiverType { ty: String::new() }.label(),
            "method-on-receiver-type"
        );
        assert_eq!(
            UiComponent {
                macro_name: String::new(),
            }
            .label(),
            "ui-component"
        );
        assert_eq!(MergeWithheld.label(), "merge-withheld");
        assert_eq!(Unclassified.label(), "unclassified");
    }

    #[test]
    fn last_two_segments_keeps_trait_and_method() {
        assert_eq!(last_two_segments("demo::Selectable::id"), "Selectable::id");
        assert_eq!(last_two_segments("a::b::c::Trait::method"), "Trait::method");
        assert_eq!(last_two_segments("Trait::method"), "Trait::method");
        assert_eq!(last_two_segments("bare"), "bare");
    }

    fn spared(trait_method: Option<&str>) -> SparedCopy {
        SparedCopy {
            site: Site {
                file: PathBuf::from("src/lib.rs"),
                line: 23,
            },
            trait_method: trait_method.map(str::to_owned),
        }
    }

    /// The delete guard's note names the trait method when the trait is
    /// workspace-resolvable and stays generic for a foreign trait.
    #[test]
    fn spared_copy_note_wordings() {
        assert_eq!(
            spared_copy_note(&spared(Some("Selectable::id"))),
            "the copy at src/lib.rs:23 implements `Selectable::id` — it is reached \
             through the trait despite having no direct caller, so it is not reported as dead"
        );
        assert_eq!(
            spared_copy_note(&spared(None)),
            "the copy at src/lib.rs:23 has no direct caller but implements a trait method — \
             deleting it would break the `impl`, so it is not reported as dead"
        );
    }

    /// Both merge-family variants render their spared notes; MergeIdenticalFns
    /// keeps the call-sites note first.
    #[test]
    fn notes_include_spared_copies() {
        let merge = RefactoringClass::MergeIdenticalFns {
            call_sites: Some(CallSites {
                count: 2,
                first: Some(Site {
                    file: PathBuf::from("src/main.rs"),
                    line: 7,
                }),
            }),
            spared: vec![spared(Some("Selectable::id"))],
        };
        let notes = merge.notes();
        assert_eq!(notes.len(), 2);
        assert!(notes[0].starts_with("2 call sites reference the copies"));
        assert!(notes[1].contains("implements `Selectable::id`"));

        let delete = RefactoringClass::DeleteDeadCopy {
            dead: vec![Site {
                file: PathBuf::from("src/lib.rs"),
                line: 40,
            }],
            spared: vec![spared(None)],
        };
        let notes = delete.notes();
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("deleting it would break the `impl`"));
    }
}
