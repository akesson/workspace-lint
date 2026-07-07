//! Cfg-coverage: which `#[cfg(...)]` regions does the declared `[engine]
//! configs` matrix actually compile?
//!
//! The evaluator is deliberately **tri-state** (`True`/`False`/`Unknown`) and
//! biased toward *shadowed*: a region is compiled only when its predicate
//! provably evaluates `True` under at least one declared config; anything the
//! model can't decide (custom cfgs, `miri`, exotic target axes) counts as
//! shadowed. For the deletion veto that is the safe direction — a false veto
//! costs one manual review, a false delete costs a broken build on a target
//! the engine never compiled.
//!
//! Feature atoms scope to the *enclosing package* (that is what
//! `cfg(feature = "…")` means): a feature is `True` if a config declares it
//! (`--features`/`--all-features`) or if it is in the package's own
//! default-feature closure and some config runs default features; a declared
//! but non-enabled feature is `Unknown`, never `False` — a future
//! `--features` entry could enable it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::orchestrate::{ConfigSpec, Kinds};
use wl_fast::FastModel;
use wl_fast::cfg_regions::{CfgAtom, CfgPredicate, CfgRegion, scan_target};

/// One shadowed region, text pre-read for mention queries.
#[derive(Debug)]
pub struct ShadowRegion {
    /// Owning member's crate code name (underscored).
    pub krate: String,
    pub file: PathBuf,
    /// Human rendering of the uncovered predicate, e.g.
    /// `target_arch = "wasm32"`.
    pub predicate: String,
    text: String,
}

/// The shadowed-region index for one run: every cfg region no declared config
/// compiles, per member crate, plus the member dependency closure that scopes
/// mention queries.
#[derive(Debug, Default)]
pub struct CfgShadow {
    regions: Vec<ShadowRegion>,
    /// crate (code name) → the members that can name its items: itself plus
    /// every member whose transitive declared deps reach it. Scopes
    /// [`Self::mention`] so an unrelated crate's shadowed region can't veto
    /// by identifier coincidence.
    visible: BTreeMap<String, BTreeSet<String>>,
}

impl CfgShadow {
    /// Scan every member's targets and keep the regions the matrix does not
    /// compile. `host_triple` resolves target atoms of host configs (pass
    /// [`host_triple`]'s result; `None` degrades those atoms to Unknown —
    /// still safe, just more vetoes).
    pub fn compute(fast: &FastModel, specs: &[ConfigSpec], host_triple: Option<&str>) -> Self {
        let mut regions = Vec::new();
        for member in fast.members() {
            let default_features = member.manifest().default_features();
            let declared: BTreeSet<String> = member.declared_features().iter().cloned().collect();
            let mut raw: Vec<CfgRegion> = Vec::new();
            for target in member.targets() {
                scan_target(&target.src_path, &mut raw);
            }
            let mut texts: BTreeMap<PathBuf, String> = BTreeMap::new();
            for region in raw {
                let compiled = specs.iter().any(|spec| {
                    eval(
                        &region.predicate,
                        spec,
                        host_triple,
                        &default_features,
                        &declared,
                    ) == Tri::True
                });
                if compiled {
                    continue;
                }
                let text = texts
                    .entry(region.file.clone())
                    .or_insert_with(|| std::fs::read_to_string(&region.file).unwrap_or_default());
                let slice = text
                    .get(region.lo..region.hi.min(text.len()))
                    .unwrap_or("")
                    .to_string();
                // Workspace-relative for the diagnostic note (deterministic
                // across checkouts).
                let file = region
                    .file
                    .strip_prefix(fast.root())
                    .map(Path::to_path_buf)
                    .unwrap_or(region.file);
                regions.push(ShadowRegion {
                    krate: member.name.replace('-', "_"),
                    file,
                    predicate: render(&region.predicate),
                    text: slice,
                });
            }
        }
        Self {
            regions,
            visible: dependents_of(fast),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    /// Does any shadowed region in a crate that can name `candidate_crate`'s
    /// items (a dependent, or itself) plausibly mention the candidate?
    /// `segments` is `[name]` for free items and `[parent, name]` for assoc
    /// items (matched as `Parent::name` or the `.name(` / `.name::<`
    /// method-call forms — a bare method name alone would veto every unused
    /// `new` in the workspace).
    /// [`Self::mention`] keyed by a candidate identity
    /// (`crate::module::…::name`): assoc items — a type-like (uppercase)
    /// second-to-last segment — query the qualified forms, free items their
    /// bare name.
    pub fn mention_id(&self, id: &str) -> Option<&ShadowRegion> {
        let segments: Vec<&str> = id.split("::").collect();
        let (krate, rest) = segments.split_first()?;
        let name = rest.last()?;
        let parent = rest
            .len()
            .checked_sub(2)
            .map(|i| rest[i])
            .filter(|p| p.chars().next().is_some_and(char::is_uppercase));
        match parent {
            Some(p) => self.mention(krate, &[p, name]),
            None => self.mention(krate, &[name]),
        }
    }

    pub fn mention(&self, candidate_crate: &str, segments: &[&str]) -> Option<&ShadowRegion> {
        let visible_from = self.visible.get(candidate_crate)?;
        self.regions
            .iter()
            .filter(|r| visible_from.contains(&r.krate))
            .find(|r| match segments {
                [name] => word_match(&r.text, name),
                [parent, name] => {
                    r.text.contains(&format!("{parent}::{name}"))
                        || r.text.contains(&format!(".{name}("))
                        || r.text.contains(&format!(".{name}::<"))
                }
                _ => false,
            })
    }

    /// The distinct uncovered predicates, with one representative file each —
    /// the coverage audit's payload.
    pub fn uncovered_predicates(&self) -> Vec<(&str, &std::path::Path)> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for r in &self.regions {
            if seen.insert(r.predicate.as_str()) {
                out.push((r.predicate.as_str(), r.file.as_path()));
            }
        }
        out
    }
}

/// crate (code name) → the members that can name its items: itself plus every
/// member whose transitive declared deps (any section — a dev/build dep can
/// name it) reach it.
fn dependents_of(fast: &FastModel) -> BTreeMap<String, BTreeSet<String>> {
    let member_names: BTreeSet<String> = fast.members().iter().map(|m| m.name.clone()).collect();
    let deps: BTreeMap<&str, BTreeSet<String>> = fast
        .members()
        .iter()
        .map(|m| {
            let ds = m
                .manifest()
                .declared_deps()
                .filter(|d| member_names.contains(&d.original_name))
                .map(|d| d.original_name)
                .collect::<BTreeSet<String>>();
            (m.name.as_str(), ds)
        })
        .collect();
    let closure = |root: &str| -> BTreeSet<String> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue = vec![root.to_string()];
        while let Some(n) = queue.pop() {
            if !seen.insert(n.clone()) {
                continue;
            }
            if let Some(ds) = deps.get(n.as_str()) {
                queue.extend(ds.iter().cloned());
            }
        }
        seen
    };
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for target in &member_names {
        let visible = member_names
            .iter()
            .filter(|m| closure(m).contains(target.as_str()))
            .map(|m| m.replace('-', "_"))
            .collect();
        out.insert(target.replace('-', "_"), visible);
    }
    out
}

/// The build host's target triple (`rustc -vV`), for host configs' target
/// atoms. `None` when rustc can't be asked — the evaluator degrades safely.
pub fn host_triple() -> Option<String> {
    let out = std::process::Command::new("rustc")
        .arg("-vV")
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: ").map(str::to_string))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tri {
    True,
    False,
    Unknown,
}

fn eval(
    pred: &CfgPredicate,
    spec: &ConfigSpec,
    host_triple: Option<&str>,
    default_features: &BTreeSet<String>,
    declared_features: &BTreeSet<String>,
) -> Tri {
    match pred {
        CfgPredicate::Atom(atom) => {
            eval_atom(atom, spec, host_triple, default_features, declared_features)
        }
        CfgPredicate::All(ps) => ps.iter().fold(Tri::True, |acc, p| {
            and(
                acc,
                eval(p, spec, host_triple, default_features, declared_features),
            )
        }),
        CfgPredicate::Any(ps) => ps.iter().fold(Tri::False, |acc, p| {
            or(
                acc,
                eval(p, spec, host_triple, default_features, declared_features),
            )
        }),
        CfgPredicate::Not(p) => {
            match eval(p, spec, host_triple, default_features, declared_features) {
                Tri::True => Tri::False,
                Tri::False => Tri::True,
                Tri::Unknown => Tri::Unknown,
            }
        }
        CfgPredicate::Unknown => Tri::Unknown,
    }
}

fn eval_atom(
    atom: &CfgAtom,
    spec: &ConfigSpec,
    host_triple: Option<&str>,
    default_features: &BTreeSet<String>,
    declared_features: &BTreeSet<String>,
) -> Tri {
    let triple = spec.target.as_deref().or(host_triple);
    let axes = triple.map(TripleAxes::parse);
    let axis = |get: fn(&TripleAxes) -> Option<&str>, want: &str| match axes.as_ref().and_then(get)
    {
        Some(have) => {
            if have == want {
                Tri::True
            } else {
                Tri::False
            }
        }
        None => Tri::Unknown,
    };
    match atom {
        CfgAtom::KeyValue { key, value } => match key.as_str() {
            "target_arch" => axis(|a| a.arch.as_deref(), value),
            "target_os" => axis(|a| a.os.as_deref(), value),
            "target_family" => axis(|a| a.family.as_deref(), value),
            "target_env" => axis(|a| a.env.as_deref(), value),
            "feature" => {
                let enabled = spec.features.features.iter().any(|f| f == value)
                    || (spec.features.all_features && declared_features.contains(value))
                    || (!spec.features.no_default_features && default_features.contains(value));
                if enabled {
                    Tri::True
                } else {
                    // A future `--features` entry could enable it: never False.
                    Tri::Unknown
                }
            }
            _ => Tri::Unknown, // pointer_width, vendor, custom key-values…
        },
        CfgAtom::Flag(flag) => match flag.as_str() {
            "unix" => axis(|a| a.family.as_deref(), "unix"),
            "windows" => axis(|a| a.family.as_deref(), "windows"),
            // `--tests`/`--benches` harness units compile with cfg(test).
            "test" => {
                if spec.kinds == Kinds::Default {
                    Tri::False
                } else {
                    Tri::True
                }
            }
            "debug_assertions" => Tri::True, // extraction always runs dev
            _ => Tri::Unknown,               // doc, docsrs, miri, fuzzing, custom flags…
        },
    }
}

fn and(a: Tri, b: Tri) -> Tri {
    match (a, b) {
        (Tri::False, _) | (_, Tri::False) => Tri::False,
        (Tri::True, Tri::True) => Tri::True,
        _ => Tri::Unknown,
    }
}

fn or(a: Tri, b: Tri) -> Tri {
    match (a, b) {
        (Tri::True, _) | (_, Tri::True) => Tri::True,
        (Tri::False, Tri::False) => Tri::False,
        _ => Tri::Unknown,
    }
}

/// The cfg axes a target triple implies. Component-parsed with a small table
/// over the OS component; anything unrecognized stays `None` (Unknown).
struct TripleAxes {
    arch: Option<String>,
    os: Option<String>,
    family: Option<String>,
    env: Option<String>,
}

impl TripleAxes {
    fn parse(triple: &str) -> Self {
        let parts: Vec<&str> = triple.split('-').collect();
        let arch = parts.first().map(|s| s.to_string());
        let rest = parts[1..].join("-");
        let (os, family, env) = if rest.contains("darwin") {
            (Some("macos"), Some("unix"), Some(""))
        } else if rest.contains("ios") {
            (Some("ios"), Some("unix"), Some(""))
        } else if rest.contains("android") {
            (Some("android"), Some("unix"), Some(""))
        } else if rest.contains("linux") {
            let env = if rest.contains("musl") { "musl" } else { "gnu" };
            (Some("linux"), Some("unix"), Some(env))
        } else if rest.contains("windows") {
            let env = if rest.contains("gnu") { "gnu" } else { "msvc" };
            (Some("windows"), Some("windows"), Some(env))
        } else if rest.contains("wasi") {
            (Some("wasi"), Some("wasm"), Some(""))
        } else if arch.as_deref().is_some_and(|a| a.starts_with("wasm")) {
            // wasm32-unknown-unknown: target_os = "unknown", family "wasm".
            (Some("unknown"), Some("wasm"), Some(""))
        } else if rest.contains("freebsd") {
            (Some("freebsd"), Some("unix"), Some(""))
        } else {
            (None, None, None)
        };
        Self {
            arch,
            os: os.map(String::from),
            family: family.map(String::from),
            env: env.map(String::from),
        }
    }
}

/// `needle` as a whole identifier word in `text`.
fn word_match(text: &str, needle: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut start = 0;
    while let Some(pos) = text[start..].find(needle) {
        let at = start + pos;
        let before_ok = at == 0 || !text[..at].chars().next_back().is_some_and(ident);
        let after = at + needle.len();
        let after_ok = after >= text.len() || !text[after..].chars().next().is_some_and(ident);
        if before_ok && after_ok {
            return true;
        }
        start = at + needle.len().max(1);
    }
    false
}

fn render(pred: &CfgPredicate) -> String {
    match pred {
        CfgPredicate::Atom(CfgAtom::KeyValue { key, value }) => {
            format!("{key} = \"{value}\"")
        }
        CfgPredicate::Atom(CfgAtom::Flag(f)) => f.clone(),
        CfgPredicate::All(ps) => format!(
            "all({})",
            ps.iter().map(render).collect::<Vec<_>>().join(", ")
        ),
        CfgPredicate::Any(ps) => format!(
            "any({})",
            ps.iter().map(render).collect::<Vec<_>>().join(", ")
        ),
        CfgPredicate::Not(p) => format!("not({})", render(p)),
        CfgPredicate::Unknown => "<unrecognized predicate>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrate::parse_command;
    use wl_fast::cfg_regions::{CfgAtom, CfgPredicate};

    fn kv(key: &str, value: &str) -> CfgPredicate {
        CfgPredicate::Atom(CfgAtom::KeyValue {
            key: key.into(),
            value: value.into(),
        })
    }

    fn ev(pred: &CfgPredicate, cmd: &str) -> Tri {
        let spec = parse_command(cmd).unwrap();
        let defaults: BTreeSet<String> = ["default".into(), "std".into()].into();
        let declared: BTreeSet<String> = ["default".into(), "std".into(), "gpu".into()].into();
        eval(
            pred,
            &spec,
            Some("aarch64-apple-darwin"),
            &defaults,
            &declared,
        )
    }

    #[test]
    fn target_atoms_follow_the_configs_triple() {
        let wasm = kv("target_arch", "wasm32");
        assert_eq!(ev(&wasm, "cargo build"), Tri::False);
        assert_eq!(
            ev(&wasm, "cargo build --target wasm32-unknown-unknown"),
            Tri::True
        );
        assert_eq!(ev(&kv("target_os", "macos"), "cargo build"), Tri::True);
        assert_eq!(
            ev(
                &CfgPredicate::Atom(CfgAtom::Flag("unix".into())),
                "cargo build"
            ),
            Tri::True
        );
        assert_eq!(
            ev(
                &CfgPredicate::Atom(CfgAtom::Flag("windows".into())),
                "cargo build"
            ),
            Tri::False
        );
    }

    #[test]
    fn test_flag_follows_config_kind() {
        let t = CfgPredicate::Atom(CfgAtom::Flag("test".into()));
        assert_eq!(ev(&t, "cargo build"), Tri::False);
        assert_eq!(ev(&t, "cargo test"), Tri::True);
    }

    #[test]
    fn feature_atoms_are_never_false() {
        // Enabled by default features → True; declared-but-unenabled →
        // Unknown (a future --features could flip it) — never False.
        assert_eq!(ev(&kv("feature", "std"), "cargo build"), Tri::True);
        assert_eq!(ev(&kv("feature", "gpu"), "cargo build"), Tri::Unknown);
        assert_eq!(
            ev(&kv("feature", "gpu"), "cargo build --features gpu"),
            Tri::True
        );
        assert_eq!(
            ev(&kv("feature", "std"), "cargo build --no-default-features"),
            Tri::Unknown
        );
        assert_eq!(
            ev(&kv("feature", "gpu"), "cargo build --all-features"),
            Tri::True
        );
    }

    #[test]
    fn combinators_use_kleene_logic() {
        let not_wasm = CfgPredicate::Not(Box::new(kv("target_arch", "wasm32")));
        assert_eq!(ev(&not_wasm, "cargo build"), Tri::True);
        let unknown_all = CfgPredicate::All(vec![
            CfgPredicate::Atom(CfgAtom::Flag("miri".into())),
            kv("target_os", "macos"),
        ]);
        assert_eq!(ev(&unknown_all, "cargo build"), Tri::Unknown);
        let short_circuit = CfgPredicate::All(vec![
            CfgPredicate::Atom(CfgAtom::Flag("miri".into())),
            kv("target_os", "windows"),
        ]);
        assert_eq!(ev(&short_circuit, "cargo build"), Tri::False);
    }

    #[test]
    fn mention_matching_is_scoped_and_qualified() {
        // `utils` is visible from `app` (a dependent); `other` sees nothing.
        let visible: BTreeMap<String, BTreeSet<String>> = [
            (
                "utils".to_string(),
                ["app".to_string(), "utils".to_string()].into(),
            ),
            ("other".to_string(), BTreeSet::new()),
        ]
        .into();
        let shadow = CfgShadow {
            regions: vec![ShadowRegion {
                krate: "app".into(),
                file: PathBuf::from("src/lib.rs"),
                predicate: "target_arch = \"wasm32\"".into(),
                text: "{ let z = utils::tz_offset(); Command::new(\"x\"); v.push(1); }".into(),
            }],
            visible,
        };
        // Free item: word match, scoped to crates that can name the candidate.
        assert!(shadow.mention("utils", &["tz_offset"]).is_some());
        assert!(shadow.mention("other", &["tz_offset"]).is_none());
        assert!(
            shadow.mention("utils", &["tz_off"]).is_none(),
            "no substring match"
        );
        // Assoc item: qualified or method-call form only — a bare `new` from
        // an unrelated type must NOT match `Command::new`.
        assert!(shadow.mention("utils", &["Command", "new"]).is_some());
        assert!(shadow.mention("utils", &["MyType", "new"]).is_none());
        assert!(
            shadow.mention("utils", &["Vec", "push"]).is_some(),
            "method-call form"
        );
    }

    #[test]
    fn host_triple_resolves() {
        let t = host_triple().expect("rustc is present in the test env");
        assert!(t.contains('-'), "{t}");
    }
}
