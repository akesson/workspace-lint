//! Per-lint documentation, keyed on [`LintId`].
//!
//! The 12 real lints carry their docs as a `DOC.md` beside each `impl
//! LintImpl` (reached through [`LintImpl::DOC`]); the three pipeline meta
//! lints — `config`, `stale-expect`, `unknown-lint` — have no lint dir, so
//! their docs live here as sibling `.md` files. [`lint_doc`] is the one place
//! that unifies both, exhaustively over `LintId`, so a new variant can't land
//! without a doc.
//!
//! The docs surface two ways: `workspace-lint explain <lint>` prints one to
//! stdout, and clap attaches each to its `check <lint> --help` long help. Both
//! go through [`render`] first — a small line-oriented markdown-to-terminal
//! pass over the closed schema below ([`render_with`] is the color-parametric
//! core): it drops the `#`/`` ` ``/`*` markers, turns headings bold, code spans
//! and fences into styling, and `- ` into `• ` bullets, so the raw Markdown
//! never reaches the terminal. Color is gated on the same NO_COLOR / tty
//! detection clap uses (`anstream::AutoStream::choice`), so piping to a file
//! yields clean, marker-free plain text.
//!
//! Because the renderer is line-oriented, the `DOC.md` authoring rules
//! (enforced by the tests below) are: first line `# <short-name>`; only
//! headings from the closed schema set (`## What it checks` / `## Configuration`
//! / `## Silencing` required, `## Fix behavior` / `## Examples` / `## Baseline
//! ratchet` / `## Known limits` optional); ATX headings and fenced code only —
//! no pipe tables, no HTML; and prose hard-wrapped so no line outside a code
//! fence exceeds 80 columns (the renderer never re-wraps, so unwrapped lines
//! would overflow narrow terminals).

use std::fmt::Write as _;

use anstyle::{AnsiColor, Style};
use wl_lint_api::{LintId, LintImpl};
use wl_lints::{
    architecture::Architecture, centralized_deps::CentralizedDeps,
    cli_crate_version::CliCrateVersion, crate_size::CrateSize, duplicate_code::DuplicateCode,
    feature_drift::FeatureDrift, file_size::FileSize, orphan_file::OrphanFile,
    stale_git_index::StaleGitIndex, unused_deps::UnusedDeps, unused_pub::UnusedPub,
};

/// The full documentation for `id`. Exhaustive by design: adding a [`LintId`]
/// variant without wiring its doc is a compile error, mirroring `LintId::id`.
/// The 11 real lints resolve to their `DOC.md` const; the 3 meta lints to a
/// sibling `.md` bundled here.
pub(crate) fn lint_doc(id: LintId) -> &'static str {
    match id {
        LintId::Architecture => Architecture::DOC,
        LintId::CentralizedDeps => CentralizedDeps::DOC,
        LintId::CliCrateVersion => CliCrateVersion::DOC,
        LintId::Config => include_str!("config.md"),
        LintId::CrateSize => CrateSize::DOC,
        LintId::DuplicateCode => DuplicateCode::DOC,
        LintId::FeatureDrift => FeatureDrift::DOC,
        LintId::FileSize => FileSize::DOC,
        LintId::OrphanFile => OrphanFile::DOC,
        LintId::StaleExpect => include_str!("stale-expect.md"),
        LintId::StaleGitIndex => StaleGitIndex::DOC,
        LintId::UnknownLint => include_str!("unknown-lint.md"),
        LintId::UnusedDeps => UnusedDeps::DOC,
        LintId::UnusedPub => UnusedPub::DOC,
    }
}

/// Resolve a user-typed lint name — short (`unused-pub`) or fully qualified
/// (`workspace-lint::unused-pub`) — to its [`LintId`]. On a miss, the `Err`
/// carries the closest known short name for a "did you mean …?" hint (or
/// `None` when nothing is close enough). Pure, so it is unit-tested directly
/// without spawning the binary.
pub(crate) fn resolve(name: &str) -> Result<LintId, Option<&'static str>> {
    let short = name.strip_prefix("workspace-lint::").unwrap_or(name);
    LintId::from_short(short).ok_or_else(|| {
        let known: Vec<&str> = LintId::ALL.iter().map(|id| id.short()).collect();
        crate::suggest::closest(short, &known)
    })
}

/// `workspace-lint explain <lint>`: print the lint's documentation to stdout
/// (data, like the machine output formats) and exit `0`. An unknown name is an
/// operational error (exit `2`) with a "did you mean …?" hint. The name
/// resolution is [`resolve`], unit-tested below.
pub(crate) fn explain(name: &str) -> ! {
    match resolve(name) {
        Ok(id) => {
            print!("{}", rendered_doc(id));
            std::process::exit(0);
        }
        Err(hint) => {
            let suffix = hint
                .map(|s| format!(" (did you mean `{s}`?)"))
                .unwrap_or_default();
            wl_lint_api::util::fail(format!("error: unknown lint `{name}`{suffix}"));
        }
    }
}

/// Fully-rendered documentation for `id`, ready to print to the terminal —
/// [`lint_doc`] run through [`render`]. The one entry point both surfaces
/// use: `explain` prints it, and each `check <lint>` subcommand carries it as
/// clap `after_long_help`.
pub(crate) fn rendered_doc(id: LintId) -> String {
    render(lint_doc(id))
}

/// Render a `DOC.md` for the terminal, coloring iff stdout wants it.
fn render(doc: &str) -> String {
    render_with(doc, stdout_wants_color())
}

/// Whether stdout should carry ANSI — the same decision clap makes for its own
/// help (`NO_COLOR` / `CLICOLOR` / tty), so a doc and clap's scaffold agree.
fn stdout_wants_color() -> bool {
    use std::io::IsTerminal;
    match anstream::AutoStream::choice(&std::io::stdout()) {
        anstream::ColorChoice::Always | anstream::ColorChoice::AlwaysAnsi => true,
        anstream::ColorChoice::Never => false,
        anstream::ColorChoice::Auto => std::io::stdout().is_terminal(),
    }
}

/// Inline code spans (`` `like this` ``).
const CODE: AnsiColor = AnsiColor::Cyan;

/// The color-parametric render core (unit-tested at both settings). The text
/// content is identical at `color = true` / `false`; color only wraps runs in
/// ANSI, so stripping the escapes from a colored render yields the plain one.
///
/// Block structure is line-oriented (fence / heading / bullet / blank), but
/// **inline** markup is resolved a whole *paragraph* at a time — a run of
/// consecutive prose/bullet lines flushed through [`inline`] joined by `\n`.
/// That is what lets emphasis wrap across a hard line break (`**mark\ngenuinely
/// published…**`), the way CommonMark allows and the docs actually write; a
/// per-line pass would leave the split `**` markers stranded.
fn render_with(doc: &str, color: bool) -> String {
    let mut out = String::new();
    let mut para: Vec<String> = Vec::new();
    let mut in_fence = false;
    for line in doc.lines() {
        let trimmed = line.trim_start();

        // A fenced block: swallow the ``` / ~~~ markers, indent + dim the body
        // so it still reads as code once the fence is gone (crucial in plain
        // mode, where dim is absent and the indent is the only cue).
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            flush_paragraph(&mut para, color, &mut out);
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            if !line.is_empty() {
                emit(
                    &mut out,
                    &format!("    {line}"),
                    color,
                    Style::new().dimmed(),
                );
            }
            out.push('\n');
            continue;
        }

        // Title: bold, with a rule under it.
        if let Some(rest) = line.strip_prefix("# ") {
            flush_paragraph(&mut para, color, &mut out);
            emit(&mut out, rest, color, Style::new().bold());
            out.push('\n');
            let rule = "\u{2500}".repeat(rest.chars().count());
            emit(&mut out, &rule, color, Style::new().dimmed());
            out.push('\n');
            continue;
        }
        // Section heading: bold + underline.
        if let Some(rest) = line.strip_prefix("## ") {
            flush_paragraph(&mut para, color, &mut out);
            emit(&mut out, rest, color, Style::new().bold().underline());
            out.push('\n');
            continue;
        }
        // Blank line: paragraph boundary (emphasis never crosses it).
        if line.is_empty() {
            flush_paragraph(&mut para, color, &mut out);
            out.push('\n');
            continue;
        }
        // Bullet: `- ` → `• `, preserving indent (both are two columns, so
        // wrapped continuation lines still align). Buffered, like prose, so a
        // bullet whose emphasis spills onto its continuation line still closes.
        let indent = line.len() - trimmed.len();
        if let Some(item) = trimmed.strip_prefix("- ") {
            para.push(format!("{}\u{2022} {item}", &line[..indent]));
            continue;
        }
        // Prose (incl. bullet continuation lines): buffer for paragraph flush.
        para.push(line.to_string());
    }
    flush_paragraph(&mut para, color, &mut out);
    out
}

/// Flush the buffered paragraph: resolve its inline markup across the joined
/// lines and append it (with a trailing newline) to `out`. A no-op on an empty
/// buffer, so it's safe to call at every block boundary.
fn flush_paragraph(para: &mut Vec<String>, color: bool, out: &mut String) {
    if para.is_empty() {
        return;
    }
    let joined: Vec<char> = para.join("\n").chars().collect();
    inline(&joined, color, Style::new(), out);
    out.push('\n');
    para.clear();
}

/// Style the inline markup in a paragraph's `chars`, appending to `out`.
/// Handles `` `code` `` / `**bold**` / `*italic*` / `[text](url)`, dropping the
/// markers and (when `color`) wrapping each run in ANSI. Marker pairs may span
/// the `\n`s the caller joins a paragraph's lines with. `base` is the
/// accumulated style of the enclosing span, so nesting (`` **bold with `code`**
/// ``) composes; literal runs flush under `base` so they inherit it too.
fn inline(chars: &[char], color: bool, base: Style, out: &mut String) {
    let mut lit = String::new();
    let mut i = 0;
    while i < chars.len() {
        // `code`
        if chars[i] == '`'
            && let Some(end) = find(chars, i + 1, '`')
        {
            emit(out, &lit, color, base);
            lit.clear();
            let inner: String = chars[i + 1..end].iter().collect();
            emit(out, &inner, color, base.fg_color(Some(CODE.into())));
            i = end + 1;
            continue;
        }
        // **bold** (checked before *italic* so the doubled star isn't misread)
        if chars[i] == '*'
            && chars.get(i + 1) == Some(&'*')
            && let Some(end) = find_bold_close(chars, i + 2)
        {
            emit(out, &lit, color, base);
            lit.clear();
            inline(&chars[i + 2..end], color, base.bold(), out);
            i = end + 2;
            continue;
        }
        // *italic*
        if chars[i] == '*'
            && let Some(end) = find(chars, i + 1, '*')
            && end != i + 1
        {
            emit(out, &lit, color, base);
            lit.clear();
            inline(&chars[i + 1..end], color, base.italic(), out);
            i = end + 1;
            continue;
        }
        // [text](url) → underlined text + dimmed url
        if chars[i] == '['
            && let Some((close, paren)) = find_link(chars, i)
        {
            emit(out, &lit, color, base);
            lit.clear();
            let text: Vec<char> = chars[i + 1..close].to_vec();
            let url: String = chars[close + 2..paren].iter().collect();
            inline(&text, color, base.underline(), out);
            emit(out, &format!(" ({url})"), color, base.dimmed());
            i = paren + 1;
            continue;
        }
        lit.push(chars[i]);
        i += 1;
    }
    emit(out, &lit, color, base);
}

/// Write `text` under `style`: with ANSI when `color`, bare otherwise. An empty
/// `style` renders to nothing, so plain runs stay escape-free even in color
/// mode — which is why stripping ANSI recovers the exact plain render.
fn emit(out: &mut String, text: &str, color: bool, style: Style) {
    if text.is_empty() {
        return;
    }
    if color {
        let _ = write!(out, "{}{text}{}", style.render(), style.render_reset());
    } else {
        out.push_str(text);
    }
}

/// First index `>= from` holding `target`.
fn find(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == target)
}

/// First `**` at or after `from` (returns the index of its first star).
fn find_bold_close(chars: &[char], from: usize) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == '*' && chars.get(j + 1) == Some(&'*'))
}

/// For a `[` at `open`, the `(close, paren)` of a `[text](url)` link, or `None`
/// if the shape isn't a link.
fn find_link(chars: &[char], open: usize) -> Option<(usize, usize)> {
    let close = find(chars, open + 1, ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let paren = find(chars, close + 2, ')')?;
    Some((close, paren))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    const REQUIRED_HEADINGS: &[&str] = &["## What it checks", "## Configuration", "## Silencing"];
    const OPTIONAL_HEADINGS: &[&str] = &[
        "## Fix behavior",
        "## Examples",
        "## Baseline ratchet",
        "## Known limits",
    ];
    /// The pipeline meta lints, whose docs live beside this module rather than
    /// in a lint dir (see [`lint_doc`]).
    const META: &[LintId] = &[LintId::Config, LintId::StaleExpect, LintId::UnknownLint];

    /// The repo-relative file a lint's doc lives in — for actionable failure
    /// messages and the README-link check.
    fn doc_path(id: LintId) -> String {
        if META.contains(&id) {
            format!("crates/workspace-lint/src/docs/{}.md", id.short())
        } else {
            format!(
                "crates/wl-lints/src/{}/DOC.md",
                id.short().replace('-', "_")
            )
        }
    }

    /// Every lint's doc obeys the terminal-readable schema (see the module
    /// docs). A crossed [`lint_doc`] match arm also trips the title check here.
    #[test]
    fn every_lint_doc_matches_the_schema() {
        for &id in LintId::ALL {
            let doc = lint_doc(id);
            let path = doc_path(id);
            assert!(!doc.is_empty(), "{path} is empty");
            assert_eq!(
                doc.lines().next(),
                Some(format!("# {}", id.short()).as_str()),
                "{path} must start with `# {}` (the title is the lint's short name)",
                id.short()
            );

            let mut in_fence = false;
            let mut headings: Vec<&str> = Vec::new();
            for (i, line) in doc.lines().enumerate() {
                let trimmed = line.trim_start();
                if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                    in_fence = !in_fence;
                    continue;
                }
                if in_fence {
                    continue;
                }
                assert!(
                    line.chars().count() <= 80,
                    "{path}:{} is {} columns — hard-wrap prose at 80 (clap doesn't wrap):\n  {line}",
                    i + 1,
                    line.chars().count()
                );
                assert!(
                    !trimmed.starts_with('|'),
                    "{path}:{} is a pipe table — use a bullet list (tables don't render in a terminal)",
                    i + 1
                );
                if line.starts_with("## ") {
                    headings.push(line);
                }
            }
            assert!(!in_fence, "{path} has an unclosed code fence");

            for req in REQUIRED_HEADINGS {
                assert!(
                    headings.contains(req),
                    "{path} is missing required heading `{req}`"
                );
            }
            for h in &headings {
                assert!(
                    REQUIRED_HEADINGS.contains(h) || OPTIONAL_HEADINGS.contains(h),
                    "{path} has non-schema heading `{h}` (allowed: {REQUIRED_HEADINGS:?} + {OPTIONAL_HEADINGS:?})"
                );
            }
        }
    }

    /// Each real lint's `lint_doc` arm resolves to *its own* `DOC` const — an
    /// explicit guard against a crossed match arm.
    #[test]
    fn lint_doc_wires_each_real_lint_to_its_own_const() {
        assert_eq!(lint_doc(LintId::Architecture), Architecture::DOC);
        assert_eq!(lint_doc(LintId::CentralizedDeps), CentralizedDeps::DOC);
        assert_eq!(lint_doc(LintId::CliCrateVersion), CliCrateVersion::DOC);
        assert_eq!(lint_doc(LintId::CrateSize), CrateSize::DOC);
        assert_eq!(lint_doc(LintId::DuplicateCode), DuplicateCode::DOC);
        assert_eq!(lint_doc(LintId::FeatureDrift), FeatureDrift::DOC);
        assert_eq!(lint_doc(LintId::FileSize), FileSize::DOC);
        assert_eq!(lint_doc(LintId::OrphanFile), OrphanFile::DOC);
        assert_eq!(lint_doc(LintId::StaleGitIndex), StaleGitIndex::DOC);
        assert_eq!(lint_doc(LintId::UnusedDeps), UnusedDeps::DOC);
        assert_eq!(lint_doc(LintId::UnusedPub), UnusedPub::DOC);
    }

    /// Every `check <lint>` subcommand carries its lint's *rendered* doc as long
    /// help, so `check <lint> --help` and `explain <lint>` show the same text.
    /// Also guards against a new `CheckRule` variant landing without the
    /// attribute. Compared with ANSI stripped so the assert is deterministic
    /// regardless of whether the test process's stdout is a tty.
    #[test]
    fn check_subcommand_help_carries_the_lint_doc() {
        let cmd = crate::cli::Cli::command();
        let check = cmd
            .find_subcommand("check")
            .expect("the check subcommand exists");
        for sub in check.get_subcommands() {
            let name = sub.get_name();
            let id = LintId::from_short(name)
                .unwrap_or_else(|| panic!("check subcommand `{name}` is not a lint short name"));
            let help = sub
                .get_after_long_help()
                .unwrap_or_else(|| {
                    panic!("check {name} has no after_long_help (missing attribute)")
                })
                .to_string();
            let plain = String::from_utf8(strip_ansi_escapes::strip(help.as_bytes())).unwrap();
            assert_eq!(
                plain.trim_end(),
                render_with(lint_doc(id), false).trim_end(),
                "check {name}'s long help must be its own rendered DOC.md"
            );
        }
    }

    /// The plain render is marker-free: the terminal never sees raw Markdown
    /// (`#`, fences, `**`, backticks) and bullets become `•`.
    #[test]
    fn render_plain_drops_markdown_markers() {
        let plain = render_with(lint_doc(LintId::UnusedPub), false);
        assert!(!plain.contains('\u{1b}'), "no ANSI in plain mode");
        assert!(!plain.contains("```"), "code fences swallowed");
        assert!(!plain.contains("## "), "heading markers stripped");
        assert!(plain.contains("What it checks"), "heading text kept");
        assert!(plain.contains("\u{2022} "), "`- ` became a bullet");
        // A `pub use`-style inline span keeps its text, loses its backticks.
        assert!(plain.contains("pub use") && !plain.contains("`pub use`"));
        // A bold span that wraps a hard line break still loses its markers —
        // the paragraph-level inline pass, not a stranded `**mark`. The break
        // itself is preserved, so "mark" and "genuinely" stay on separate
        // lines; what matters is the `**` around them doesn't survive.
        assert!(
            plain.contains("genuinely-published crates with publish = true"),
            "multi-line bold body renders as one marker-free run"
        );
        assert!(!plain.contains("**mark"), "the opening `**` must not leak");
    }

    /// Every paragraph's inline emphasis is balanced — the precondition the
    /// paragraph-level renderer relies on. An author who leaves a `**`/`*`/`` `
    /// `` unclosed (the one way a raw marker could still reach the terminal, and
    /// a leak none of the render-equivalence tests would catch) trips this.
    #[test]
    fn inline_markup_is_balanced_per_paragraph() {
        /// Drop `` `code` `` spans (their contents aren't emphasis).
        fn strip_code_spans(s: &str) -> String {
            let mut out = String::new();
            let mut in_code = false;
            for c in s.chars() {
                match c {
                    '`' => in_code = !in_code,
                    _ if !in_code => out.push(c),
                    _ => {}
                }
            }
            out
        }
        for &id in LintId::ALL {
            // Prose only — fenced blocks are verbatim, never inline-parsed.
            let mut prose = String::new();
            let mut in_fence = false;
            for line in lint_doc(id).lines() {
                let t = line.trim_start();
                if t.starts_with("```") || t.starts_with("~~~") {
                    in_fence = !in_fence;
                } else if !in_fence {
                    prose.push_str(line);
                    prose.push('\n');
                }
            }
            for (n, para) in prose.split("\n\n").enumerate() {
                let short = id.short();
                assert_eq!(
                    para.matches('`').count() % 2,
                    0,
                    "{short} paragraph {n}: unbalanced backtick"
                );
                let no_code = strip_code_spans(para);
                assert_eq!(
                    no_code.matches("**").count() % 2,
                    0,
                    "{short} paragraph {n}: unbalanced `**`"
                );
                assert_eq!(
                    no_code.replace("**", "").matches('*').count() % 2,
                    0,
                    "{short} paragraph {n}: unbalanced `*`"
                );
            }
        }
    }

    /// Color mode adds ANSI but no text: stripping the escapes must reproduce
    /// the plain render exactly (the invariant the help test above leans on).
    #[test]
    fn render_color_is_plain_plus_ansi() {
        for &id in LintId::ALL {
            let colored = render_with(lint_doc(id), true);
            assert!(
                colored.contains('\u{1b}'),
                "{} colors something",
                id.short()
            );
            let stripped =
                String::from_utf8(strip_ansi_escapes::strip(colored.as_bytes())).unwrap();
            assert_eq!(
                stripped,
                render_with(lint_doc(id), false),
                "{}: stripping color must recover the plain render",
                id.short()
            );
        }
    }

    /// The README links every lint's doc file, so the docs stay discoverable
    /// and a renamed file can't silently orphan the link.
    #[test]
    fn readme_links_every_lint_doc() {
        let readme =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../README.md"))
                .expect("README.md is readable");
        for &id in LintId::ALL {
            let path = doc_path(id);
            assert!(readme.contains(&path), "README.md must link to `{path}`");
        }
    }

    #[test]
    fn resolve_accepts_short_and_fully_qualified_names() {
        assert_eq!(resolve("unused-pub"), Ok(LintId::UnusedPub));
        assert_eq!(resolve("workspace-lint::unused-pub"), Ok(LintId::UnusedPub));
    }

    #[test]
    fn resolve_suggests_the_closest_lint_on_a_typo() {
        assert_eq!(resolve("unused-dep"), Err(Some("unused-deps")));
    }

    #[test]
    fn resolve_gives_no_suggestion_when_nothing_is_close() {
        assert_eq!(resolve("zzzzzzzzzz"), Err(None));
    }
}
