//! Commands-as-configs: parse a declared cargo command line into the
//! compilation universe the engine must extract.
//!
//! An `[engine] configs` entry is the *real command the project runs* —
//! `"cargo build"`, `"cargo nextest run"`, `"cargo build --target
//! wasm32-unknown-unknown -p app"` — so the declared matrix *is* the support
//! matrix. The engine never runs the command itself: it reproduces the
//! command's compilation universe with a dylint-driven `cargo check`, which is
//! sound because `check` compiles the same units under the same cfgs (build
//! scripts, proc macros, features, target) as every supported verb — only
//! codegen differs.
//!
//! The parser is **strict and closed**: any token it does not positively
//! recognize is a hard error, never silently dropped — a swallowed `-p` would
//! widen the extracted universe, a swallowed `--target` would put it in the
//! wrong one. The only tokens accepted *and ignored* are flags that provably
//! cannot change what compiles (`--locked`, verbosity, job counts, …).

use std::fmt;

/// Which cargo target kinds a config compiles, mirroring the verb families
/// (`build`/`check`/`clippy` ⇒ lib+bins; `test`/`nextest run` ⇒ test
/// harnesses; `bench` ⇒ bench targets). One kind per config — a matrix entry
/// per universe, exactly like the extractor's `+test` fragment keying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kinds {
    Default,
    Tests,
    Benches,
}

impl Kinds {
    /// The cargo target-selection flag reproducing this kind under `cargo
    /// check` (none for the default lib+bins set).
    fn cargo_flag(self) -> Option<&'static str> {
        match self {
            Kinds::Default => None,
            Kinds::Tests => Some("--tests"),
            Kinds::Benches => Some("--benches"),
        }
    }

    /// The filesystem token naming this kind's IR subdirectory. Kept identical
    /// to the pre-command-vocabulary ids so existing IR dirs stay warm.
    fn id_token(self) -> &'static str {
        match self {
            Kinds::Default => "default",
            Kinds::Tests => "tests",
            Kinds::Benches => "benches",
        }
    }

    /// The canonical verb for the human-facing `display` string.
    fn verb(self) -> &'static str {
        match self {
            Kinds::Default => "build",
            Kinds::Tests => "test",
            Kinds::Benches => "bench",
        }
    }
}

/// Cargo feature selection, folded into the config id (a different feature set
/// is a different compilation universe).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureSel {
    /// `--features` entries, comma/space-split, sorted + deduped.
    pub features: Vec<String>,
    pub all_features: bool,
    pub no_default_features: bool,
}

impl FeatureSel {
    pub fn is_default(&self) -> bool {
        self.features.is_empty() && !self.all_features && !self.no_default_features
    }

    fn push_cargo_args(&self, out: &mut Vec<String>) {
        if self.all_features {
            out.push("--all-features".into());
        }
        if self.no_default_features {
            out.push("--no-default-features".into());
        }
        if !self.features.is_empty() {
            out.push("--features".into());
            out.push(self.features.join(","));
        }
    }
}

/// One parsed `[engine] configs` entry: the compilation universe of a declared
/// cargo command. The `id` names the IR subdirectory (fs-safe, total over the
/// spec); `display` is the canonical command for humans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSpec {
    pub id: String,
    pub display: String,
    pub kinds: Kinds,
    /// `--target` triple; `None` = the host.
    pub target: Option<String>,
    /// Declared `-p` roots (pre-closure), sorted + deduped. Empty ⇒ whole
    /// workspace.
    pub packages: Vec<String>,
    pub features: FeatureSel,
}

impl ConfigSpec {
    /// The args forwarded to the dylint-driven `cargo check` (target-kind
    /// selection, target triple, features). Package selection is NOT here —
    /// it flows through dylint's structured `packages` channel so the
    /// completeness guard can model it.
    pub fn cargo_args(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(flag) = self.kinds.cargo_flag() {
            out.push(flag.to_string());
        }
        if let Some(t) = &self.target {
            out.push("--target".into());
            out.push(t.clone());
        }
        self.features.push_cargo_args(&mut out);
        out
    }

    /// Plain host `cargo build` (test convenience; identical to parsing it).
    pub fn host_default() -> Self {
        parse_command("cargo build").expect("static command parses")
    }

    /// Plain host `cargo test` (test convenience).
    pub fn host_tests() -> Self {
        parse_command("cargo test").expect("static command parses")
    }

    /// Plain host `cargo bench` (test convenience).
    pub fn host_benches() -> Self {
        parse_command("cargo bench").expect("static command parses")
    }

    fn finish(
        kinds: Kinds,
        target: Option<String>,
        mut packages: Vec<String>,
        mut features: FeatureSel,
    ) -> Self {
        packages.sort();
        packages.dedup();
        features.features.sort();
        features.features.dedup();

        let mut id = kinds.id_token().to_string();
        if let Some(t) = &target {
            id.push('@');
            id.push_str(t);
        }
        // Packages/features re-key the universe. Hash the *declared* selection
        // (never a computed closure — closures re-key whenever a transitive
        // dep is added, orphaning warm IR dirs).
        if !packages.is_empty() || !features.is_default() {
            let mut token = String::new();
            for p in &packages {
                token.push_str("p=");
                token.push_str(p);
                token.push(';');
            }
            for f in &features.features {
                token.push_str("f=");
                token.push_str(f);
                token.push(';');
            }
            if features.all_features {
                token.push_str("all-features;");
            }
            if features.no_default_features {
                token.push_str("no-default;");
            }
            id.push('-');
            id.push_str(&fnv1a_hex8(&token));
        }

        let mut display = format!("cargo {}", kinds.verb());
        if let Some(t) = &target {
            display.push_str(" --target ");
            display.push_str(t);
        }
        for p in &packages {
            display.push_str(" -p ");
            display.push_str(p);
        }
        if features.all_features {
            display.push_str(" --all-features");
        }
        if features.no_default_features {
            display.push_str(" --no-default-features");
        }
        if !features.features.is_empty() {
            display.push_str(" --features ");
            display.push_str(&features.features.join(","));
        }

        Self {
            id,
            display,
            kinds,
            target,
            packages,
            features,
        }
    }
}

/// FNV-1a 64, hex-truncated to 8 chars. Inlined so config ids are stable
/// across Rust releases (std's `DefaultHasher` algorithm is unspecified).
fn fnv1a_hex8(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    format!("{h:016x}")[..8].to_string()
}

/// Why an entry failed to parse. `Display` is the user-facing audit message —
/// each variant says what to write instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandError {
    /// A pre-command-vocabulary selector name; maps to its command spelling.
    OldVocabulary {
        entry: String,
        replacement: &'static str,
    },
    NotCargo {
        entry: String,
    },
    UnknownVerb {
        verb: String,
    },
    UnsupportedFlag {
        flag: String,
        why: &'static str,
    },
    UnknownFlag {
        flag: String,
    },
    MissingValue {
        flag: String,
    },
    Positional {
        token: String,
    },
    DuplicateTarget,
    Empty,
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommandError::OldVocabulary { entry, replacement } => write!(
                f,
                "`{entry}` is the old selector vocabulary; engine configs are now real cargo \
                 commands — write `{replacement}`"
            ),
            CommandError::NotCargo { entry } => write!(
                f,
                "`{entry}` is not a cargo command; an engine config is the cargo command whose \
                 compilation universe the lints should see, e.g. `cargo build` or `cargo test`"
            ),
            CommandError::UnknownVerb { verb } => write!(
                f,
                "unsupported cargo subcommand `{verb}`; the engine can reproduce `build`, \
                 `check`, `clippy`, `test`, `nextest run`, and `bench`"
            ),
            CommandError::UnsupportedFlag { flag, why } => {
                write!(f, "unsupported flag `{flag}`: {why}")
            }
            CommandError::UnknownFlag { flag } => write!(
                f,
                "unrecognized flag `{flag}`; the engine refuses to guess — a dropped flag could \
                 silently change which code the lints judge"
            ),
            CommandError::MissingValue { flag } => write!(f, "`{flag}` needs a value"),
            CommandError::Positional { token } => write!(
                f,
                "unexpected argument `{token}`; name filters and runtime arguments don't change \
                 what compiles — remove them from the config entry"
            ),
            CommandError::DuplicateTarget => write!(f, "`--target` given more than once"),
            CommandError::Empty => write!(f, "empty config entry"),
        }
    }
}

/// Flags that provably cannot change the compilation universe: accepted and
/// dropped. Everything else unrecognized is an error.
const UNIVERSE_NEUTRAL: &[&str] = &[
    "--locked",
    "--frozen",
    "--offline",
    "--quiet",
    "-q",
    "--verbose",
    "-v",
    "-vv",
    "--no-fail-fast",
    "--no-run",
];
/// Universe-neutral flags that carry a value to skip.
const UNIVERSE_NEUTRAL_VALUED: &[&str] = &["--color", "--jobs", "-j", "--message-format"];

/// Target-selection flags the completeness guard cannot model per-target;
/// each maps to the guidance replacing it.
const UNSUPPORTED: &[(&str, &str)] = &[
    (
        "--all-targets",
        "declare separate `cargo build` / `cargo test` entries instead — each config is one \
         target-kind universe",
    ),
    (
        "--exclude",
        "exclusion is ambiguous once dependency closures are expanded; list the packages you \
         want with `-p` instead",
    ),
    (
        "--release",
        "extraction always uses the dev profile; the profile does not change the reference \
         graph (only `debug_assertions`) — drop the flag",
    ),
    (
        "--profile",
        "extraction always uses the dev profile; the profile does not change the reference \
         graph (only `debug_assertions`) — drop the flag",
    ),
    (
        "--lib",
        "per-target-kind selection below `--tests` granularity isn't modeled; use a plain `cargo build` entry",
    ),
    (
        "--bins",
        "per-target-kind selection below `--tests` granularity isn't modeled; use a plain `cargo build` entry",
    ),
    (
        "--bin",
        "per-target-kind selection below `--tests` granularity isn't modeled; use a plain `cargo build` entry",
    ),
    (
        "--examples",
        "example targets aren't extracted; drop the flag",
    ),
    (
        "--example",
        "example targets aren't extracted; drop the flag",
    ),
    (
        "--test",
        "per-target selection isn't modeled; use a plain `cargo test` entry",
    ),
    (
        "--bench",
        "per-target selection isn't modeled; use a plain `cargo bench` entry",
    ),
    ("--doc", "doctests aren't extracted; drop the flag"),
    (
        "-E",
        "nextest filter expressions don't change what compiles; remove them",
    ),
    (
        "--filter-expr",
        "nextest filter expressions don't change what compiles; remove them",
    ),
];

/// Old selector vocabulary → the command that replaces it.
const OLD_VOCABULARY: &[(&str, &str)] = &[
    ("default", "cargo build"),
    ("--tests", "cargo test"),
    ("tests", "cargo test"),
    ("--benches", "cargo bench"),
    ("benches", "cargo bench"),
];

/// Parse one `[engine] configs` entry. Strict: every token is positively
/// recognized or the entry is rejected with guidance.
pub fn parse_command(entry: &str) -> Result<ConfigSpec, CommandError> {
    let entry = entry.trim();
    if entry.is_empty() {
        return Err(CommandError::Empty);
    }
    if let Some((_, replacement)) = OLD_VOCABULARY.iter().find(|(old, _)| *old == entry) {
        return Err(CommandError::OldVocabulary {
            entry: entry.to_string(),
            replacement,
        });
    }

    let mut toks = entry.split_whitespace().peekable();
    if toks.next() != Some("cargo") {
        return Err(CommandError::NotCargo {
            entry: entry.to_string(),
        });
    }

    let verb = toks.next().ok_or(CommandError::Empty)?;
    let mut spec = SpecBuilder {
        kinds: parse_verb(verb, &mut toks)?,
        target: None,
        packages: Vec::new(),
        features: FeatureSel::default(),
    };
    while let Some(tok) = toks.next() {
        spec.apply_flag(tok, &mut toks)?;
    }
    Ok(ConfigSpec::finish(
        spec.kinds,
        spec.target,
        spec.packages,
        spec.features,
    ))
}

type Toks<'a> = std::iter::Peekable<std::str::SplitWhitespace<'a>>;

fn parse_verb(verb: &str, toks: &mut Toks<'_>) -> Result<Kinds, CommandError> {
    match verb {
        "build" | "b" | "check" | "c" | "clippy" => Ok(Kinds::Default),
        "test" | "t" => Ok(Kinds::Tests),
        "bench" => Ok(Kinds::Benches),
        "nextest" => {
            // `cargo nextest run` (bare `cargo nextest` accepted); compiles
            // the `cargo test --no-run` set.
            if toks.peek() == Some(&"run") {
                toks.next();
            }
            Ok(Kinds::Tests)
        }
        other => Err(CommandError::UnknownVerb {
            verb: other.to_string(),
        }),
    }
}

/// The spec-in-progress while the flag loop runs; folded into
/// [`ConfigSpec::finish`] once the tokens are exhausted.
struct SpecBuilder {
    kinds: Kinds,
    target: Option<String>,
    packages: Vec<String>,
    features: FeatureSel,
}

impl SpecBuilder {
    fn apply_flag(&mut self, tok: &str, toks: &mut Toks<'_>) -> Result<(), CommandError> {
        // `--flag=value` splits here; bare `--flag` keeps `None`.
        let (flag, inline_val) = match tok.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (tok, None),
        };
        let value = |toks: &mut Toks<'_>| {
            inline_val
                .clone()
                .or_else(|| toks.next().map(str::to_string))
                .ok_or(CommandError::MissingValue {
                    flag: flag.to_string(),
                })
        };

        if let Some((_, why)) = UNSUPPORTED.iter().find(|(f, _)| *f == flag) {
            return Err(CommandError::UnsupportedFlag {
                flag: flag.to_string(),
                why,
            });
        }
        match flag {
            "--target" => {
                if self.target.is_some() {
                    return Err(CommandError::DuplicateTarget);
                }
                self.target = Some(value(toks)?);
            }
            "-p" | "--package" => self.packages.push(value(toks)?),
            "--workspace" | "--all" => {} // the default
            "--features" | "-F" => {
                let v = value(toks)?;
                self.features.features.extend(
                    v.split([',', ' '])
                        .filter(|s| !s.is_empty())
                        .map(String::from),
                );
            }
            "--all-features" => self.features.all_features = true,
            "--no-default-features" => self.features.no_default_features = true,
            "--tests" => self.kinds = Kinds::Tests,
            "--benches" => self.kinds = Kinds::Benches,
            _ if UNIVERSE_NEUTRAL.contains(&flag) => {}
            _ if UNIVERSE_NEUTRAL_VALUED.contains(&flag) => {
                value(toks)?;
            }
            _ if flag.starts_with('-') => {
                return Err(CommandError::UnknownFlag {
                    flag: flag.to_string(),
                });
            }
            _ => {
                return Err(CommandError::Positional {
                    token: tok.to_string(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> ConfigSpec {
        parse_command(s).unwrap_or_else(|e| panic!("`{s}` should parse: {e}"))
    }

    #[test]
    fn verbs_map_to_kinds() {
        assert_eq!(parse("cargo build").kinds, Kinds::Default);
        assert_eq!(parse("cargo check").kinds, Kinds::Default);
        assert_eq!(parse("cargo clippy").kinds, Kinds::Default);
        assert_eq!(parse("cargo test").kinds, Kinds::Tests);
        assert_eq!(parse("cargo nextest run").kinds, Kinds::Tests);
        assert_eq!(parse("cargo nextest").kinds, Kinds::Tests);
        assert_eq!(parse("cargo bench").kinds, Kinds::Benches);
    }

    #[test]
    fn plain_host_ids_stay_stable() {
        // Existing IR dirs must stay warm across the vocabulary change.
        assert_eq!(parse("cargo build").id, "default");
        assert_eq!(parse("cargo test").id, "tests");
        assert_eq!(parse("cargo bench").id, "benches");
    }

    #[test]
    fn equivalent_commands_normalize_identically() {
        assert_eq!(parse("cargo build"), parse("cargo check"));
        assert_eq!(parse("cargo build"), parse("cargo clippy --workspace"));
        assert_eq!(parse("cargo test"), parse("cargo nextest run"));
        assert_eq!(parse("cargo test"), parse("cargo check --tests"));
        // Order-insensitive package/feature normalization.
        assert_eq!(
            parse("cargo build -p b -p a --features y,x"),
            parse("cargo build --features x --features y -p a -p b"),
        );
    }

    #[test]
    fn target_lands_in_spec_and_id() {
        let s = parse("cargo build --target wasm32-unknown-unknown -p app");
        assert_eq!(s.kinds, Kinds::Default);
        assert_eq!(s.target.as_deref(), Some("wasm32-unknown-unknown"));
        assert_eq!(s.packages, vec!["app"]);
        assert!(
            s.id.starts_with("default@wasm32-unknown-unknown-"),
            "id: {}",
            s.id
        );
        assert_eq!(
            s.display,
            "cargo build --target wasm32-unknown-unknown -p app"
        );
        // Equals form parses the same.
        assert_eq!(
            s,
            parse("cargo build --target=wasm32-unknown-unknown --package app")
        );
    }

    #[test]
    fn cargo_args_reproduce_the_universe() {
        assert!(parse("cargo build").cargo_args().is_empty());
        assert_eq!(parse("cargo test").cargo_args(), ["--tests"]);
        assert_eq!(
            parse("cargo build --target wasm32-unknown-unknown").cargo_args(),
            ["--target", "wasm32-unknown-unknown"],
        );
        assert_eq!(
            parse("cargo test --no-default-features --features gpu").cargo_args(),
            ["--tests", "--no-default-features", "--features", "gpu"],
        );
        // Packages are NOT in cargo_args — they flow through the structured
        // channel the guard models.
        assert!(parse("cargo build -p app").cargo_args().is_empty());
    }

    #[test]
    fn ids_are_total_over_the_spec() {
        // Every universe-changing axis must re-key the IR dir.
        let ids: Vec<String> = [
            "cargo build",
            "cargo test",
            "cargo bench",
            "cargo build --target wasm32-unknown-unknown",
            "cargo build -p app",
            "cargo build --features gpu",
            "cargo build --all-features",
            "cargo build --no-default-features",
        ]
        .iter()
        .map(|s| parse(s).id)
        .collect();
        let mut deduped = ids.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(deduped.len(), ids.len(), "id collision in {ids:?}");
        // …and all ids are fs-safe.
        for id in &ids {
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_@.".contains(c)),
                "unsafe id: {id}"
            );
        }
    }

    #[test]
    fn universe_neutral_flags_are_dropped() {
        assert_eq!(
            parse("cargo test --locked --no-fail-fast -q"),
            parse("cargo test")
        );
        assert_eq!(
            parse("cargo build --jobs 4 --color=always"),
            parse("cargo build")
        );
    }

    #[test]
    fn old_vocabulary_maps_to_commands() {
        for (old, new) in [
            ("default", "cargo build"),
            ("--tests", "cargo test"),
            ("tests", "cargo test"),
            ("--benches", "cargo bench"),
            ("benches", "cargo bench"),
        ] {
            match parse_command(old) {
                Err(CommandError::OldVocabulary { replacement, .. }) => {
                    assert_eq!(replacement, new);
                }
                other => panic!("`{old}` should map to `{new}`, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejections_are_loud_and_specific() {
        assert!(matches!(
            parse_command("cargo run"),
            Err(CommandError::UnknownVerb { .. })
        ));
        assert!(matches!(
            parse_command("make lint"),
            Err(CommandError::NotCargo { .. })
        ));
        assert!(matches!(
            parse_command("cargo build --all-targets"),
            Err(CommandError::UnsupportedFlag { .. })
        ));
        assert!(matches!(
            parse_command("cargo build --release"),
            Err(CommandError::UnsupportedFlag { .. })
        ));
        assert!(matches!(
            parse_command("cargo build --workspace --exclude app"),
            Err(CommandError::UnsupportedFlag { .. })
        ));
        assert!(matches!(
            parse_command("cargo nextest run -E 'test(foo)'"),
            Err(CommandError::UnsupportedFlag { .. })
        ));
        assert!(matches!(
            parse_command("cargo test my_filter"),
            Err(CommandError::Positional { .. })
        ));
        assert!(matches!(
            parse_command("cargo build --frobnicate"),
            Err(CommandError::UnknownFlag { .. })
        ));
        assert!(matches!(
            parse_command("cargo build --target"),
            Err(CommandError::MissingValue { .. })
        ));
        assert!(matches!(
            parse_command("cargo build --target a --target b"),
            Err(CommandError::DuplicateTarget)
        ));
    }

    #[test]
    fn display_is_canonical() {
        assert_eq!(parse("cargo nextest run --locked").display, "cargo test");
        assert_eq!(parse("cargo clippy").display, "cargo build");
        assert_eq!(
            parse("cargo check --tests --features b,a").display,
            "cargo test --features a,b"
        );
    }
}
