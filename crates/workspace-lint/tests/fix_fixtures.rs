//! Fixture-pair tests for `workspace-lint --fix`.
//!
//! Each fixture under `tests/fixtures/fix__<name>/` has two sibling trees:
//! - `input/` — the workspace state before `--fix` runs.
//! - `expected/` — what the tree should look like after `--fix`.
//!
//! On `cargo test`, the driver copies `input/` to a tempdir, runs the
//! binary with `--fix`, and asserts the resulting tree equals `expected/`
//! byte-for-byte.
//!
//! On `WORKSPACE_LINT_BLESS=1 cargo test`, the driver instead overwrites
//! `expected/` with the post-fix tree. The test still passes so a casual
//! `BLESS=1 cargo test` run does the right thing.
//!
//! A fixture may carry a `setup.toml` sibling of `input/` (same schema as the
//! `tests/cases/` harness — see [`common::apply_setup`]); it's applied to the
//! staged tempdir after copy. `fix__stale_expect` uses its `[[append]]` hook to
//! inject the `expect` directive uncommitted, so this repo's own dogfood scan
//! doesn't trip on a committed stale directive under `tests/fixtures/`.

use std::path::{Path, PathBuf};
use tempfile::TempDir;

mod common;
use common::{apply_setup, bless_enabled, copy_tree, walk_files, workspace_lint};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn run_fix_fixture(name: &str) {
    let fixture = manifest_dir().join("tests/fixtures").join(name);
    let input = fixture.join("input");
    let expected = fixture.join("expected");
    assert!(
        input.is_dir(),
        "fixture {name}: missing input/ at {}",
        input.display()
    );
    if !bless_enabled() {
        assert!(
            expected.is_dir(),
            "fixture {name}: missing expected/ at {} \
             (run `WORKSPACE_LINT_BLESS=1 cargo test {name}` to generate)",
            expected.display()
        );
    }

    let tmp = TempDir::new().expect("create tempdir");
    copy_tree(&input, tmp.path()).expect("copy input → tempdir");

    // Apply an optional `setup.toml` (sibling of input/) — e.g. inject an
    // `expect` directive that must stay out of the committed fixture. Extra
    // CLI args it returns are appended to the `--fix` invocation.
    let setup_args = apply_setup(&fixture, tmp.path()).expect("apply setup.toml");

    // --fix runs the renderer after fixing, which exits 1 if any Deny-level
    // diagnostic survived. Fixture tests focus on the resulting tree, not
    // exit status, so the assertion is dropped here.
    let mut cmd = workspace_lint();
    cmd.current_dir(tmp.path()).arg("--fix").args(&setup_args);
    let _ = cmd.assert();

    if bless_enabled() {
        sync_tree(tmp.path(), &expected).expect("bless expected/");
        eprintln!("blessed: {}", expected.display());
    } else {
        assert_trees_equal(tmp.path(), &expected);
    }
}

/// Wholesale replace `dst` with `src`'s comparison-relevant files — the same set
/// [`assert_trees_equal`] checks, excluding the `Cargo.lock` that
/// `cargo metadata` generates as a side-effect (no fixture commits one, so it
/// must not leak into a blessed `expected/`). Deletes any pre-existing `dst`
/// first so removals propagate through bless.
fn sync_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.is_dir() {
        std::fs::remove_dir_all(dst)?;
    }
    for rel in walk_files(src, &["Cargo.lock"]) {
        let to = dst.join(&rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src.join(&rel), &to)?;
    }
    Ok(())
}

fn assert_trees_equal(actual: &Path, expected: &Path) {
    // `walk_files` returns relative, sorted paths (and excludes the generated
    // `Cargo.lock`), so the two lists compare directly.
    let actual_files = walk_files(actual, &["Cargo.lock"]);
    let expected_files = walk_files(expected, &["Cargo.lock"]);

    assert_eq!(
        actual_files, expected_files,
        "tree contents differ.\n  actual:   {actual_files:#?}\n  expected: {expected_files:#?}\n\
         (run `WORKSPACE_LINT_BLESS=1 cargo test` to regenerate expected/)"
    );

    for rel in actual_files {
        let a = std::fs::read_to_string(actual.join(&rel))
            .unwrap_or_else(|e| panic!("read actual {}: {e}", rel.display()));
        let e = std::fs::read_to_string(expected.join(&rel))
            .unwrap_or_else(|err| panic!("read expected {}: {err}", rel.display()));
        assert_eq!(
            a,
            e,
            "file content differs at {}\n--- actual ---\n{a}\n--- expected ---\n{e}\n\
             (run `WORKSPACE_LINT_BLESS=1 cargo test` to update)",
            rel.display()
        );
    }
}

// --- One test per fixture below. New fixtures go in
//     tests/fixtures/fix__<name>/{input,expected}/ then get a #[test]
//     wrapper here. The `every_fixturable_lint_has_a_fix_fixture` guard in
//     src/lints/lints_id.rs (its `#[cfg(test)] mod tests`) verifies the
//     FIXTURABLE_LINTS list stays in sync with what exists on disk.

#[test]
fn fix_centralized_deps() {
    run_fix_fixture("fix__centralized_deps");
}

/// The absent-table shape: --fix creates `[workspace.dependencies]` exactly
/// once, carrying every agreed entry (incl. `default-features = false`) —
/// per-dep header insertions stacked N duplicate sections here and cargo
/// rejected the manifest (ripgrep, 2026-07-10 validation Issues 2 + 3).
#[test]
fn fix_centralized_deps_create_table() {
    run_fix_fixture("fix__centralized_deps_create_table");
}

#[test]
fn fix_unused_deps() {
    run_fix_fixture("fix__unused_deps");
}

#[test]
fn fix_unused_deps_dirty_manifest() {
    // The per-file git gate, dirty flavor: the manifest carrying the unused
    // dep is dirtied post-commit (setup.toml's `[[append_after_commit]]`),
    // so the dep-line deletion is withheld (MaybeIncorrect + "commit first"
    // note) and the tree keeps both the dep and the local edit — even under
    // `--allow-dirty`, which bypasses only the tree-level gate.
    run_fix_fixture("fix__unused_deps_dirty");
}

#[test]
fn fix_unused_pub() {
    run_fix_fixture("fix__unused_pub");
}

#[test]
fn fix_unused_pub_delete() {
    // `--fix-auto-delete` + the one-pass cascade: a 3-deep dead chain crossing a
    // module boundary (`dead_a` → `helper` → `inner::extra::inner_dead`) is
    // removed whole-item in a single run, the `use inner::{helper, kept}` list
    // is trimmed to `{kept}`, and the live cross-crate API stays `pub`. The
    // flag comes from setup.toml's `args`; the `[git] init` setup makes the
    // tree clean so the deletions are
    // MachineApplicable; the blessed `expected/` tree is a compiling workspace.
    run_fix_fixture("fix__unused_pub_delete");
}

#[test]
fn fix_unused_pub_delete_attrs() {
    // The deletion SURFACE (LeaveDates 2026-07-05): attributes the extractor's
    // `full_span` can never see — `#[cfg(test)]` (stripped from HIR in the
    // unit where the item survives) and Parsed built-ins (`#[deprecated]`,
    // `#[must_use]`) — are extended over lexically instead of being orphaned
    // onto the next item. The deleted item's last-user import
    // (`use util::thing;`, target SURVIVING) is trimmed second-order, and
    // blank separators collapse so the result is fmt-clean.
    run_fix_fixture("fix__unused_pub_delete_attrs");
}

#[test]
fn fix_unused_pub_trait_import() {
    // Lexical import-scope attribution (LeaveDates 2026-07-06): the deleted
    // `dead_sort` and a SURVIVING `FromIterator` impl in the same module both
    // call the provided trait method `DateFn::year`. The impl's rendered path
    // (`<list::List as FromIterator<…>>`) hides its module, so without the
    // edge's lexical `from_module` the survivor's call credited no scope and
    // the trait import was trimmed — E0599 on the fixed tree. The
    // `use crate::datefn::DateFn;` in list.rs must survive the fix; the
    // blessed `expected/` tree is a compiling workspace.
    run_fix_fixture("fix__unused_pub_trait_import");
}

#[test]
fn fix_unused_pub_cfg_veto() {
    // The cfg-shadow deletion veto (the LeaveDates `utc_offset` incident):
    // `utils::tz_offset_minutes` is unused in every DECLARED config (host
    // `cargo build` only), but app mentions it inside a
    // `#[cfg(target_arch = "wasm32")]` block the matrix never compiles.
    // Deleting it would break a build the engine never saw, so the cascade
    // downgrades the deletion (MaybeIncorrect + the uncovered-cfg note) and
    // the item survives — while the genuinely-unmentioned `dead_helper` is
    // still deleted in the same run.
    run_fix_fixture("fix__unused_pub_cfg_veto");
}

#[test]
fn fix_unused_pub_glob_import() {
    // The glob-import cleanup (LeaveDates 2026-07-07, `feature-state/util.rs`):
    // deleting a module's last consumer of `use demo::prelude::*;` removes the
    // glob statement too — judged by the resolver-grounded accounting
    // (glob_map names + trait_scope facts), which keeps the glob wherever a
    // survivor still leans on it (a `widget!` invocation, a trait-method
    // call) and never touches a pre-existing unused one (causality).
    run_fix_fixture("fix__unused_pub_glob_import");
}

#[test]
fn fix_unused_pub_delete_unmask_field() {
    // The deletion-unmask veto, field flavor (LeaveDates 2026-07-07,
    // `PopoverMenuClose.is_open`): unused `open_state` holds the LAST read of
    // the surviving `Panel`'s private `open` field — deleting it trips rustc
    // `dead_code` on the fixed tree, so the cascade vetoes it (MaybeIncorrect
    // + the field note) while `dead_helper` is still deleted in the same run.
    run_fix_fixture("fix__unused_pub_delete_unmask_field");
}

#[test]
fn fix_unused_pub_delete_unmask_len() {
    // The deletion-unmask veto, clippy flavor (LeaveDates 2026-07-07,
    // `PasswordData::is_empty`): deleting unused `Buf::is_empty` out from
    // under the surviving `Buf::len` unmasks clippy `len_without_is_empty` —
    // vetoed, while `dead_helper` is still deleted in the same run.
    run_fix_fixture("fix__unused_pub_delete_unmask_len");
}

#[test]
fn fix_unused_pub_collateral() {
    // The cascade's second-order surfaces (LeaveDates 2026-07-05 follow-ups):
    // deleting `dead` strands the private `helper` → `inner` chain (rustc
    // `dead_code` on the fixed tree), which is deleted as collateral; its
    // out-of-workspace import (`use std::fmt::Write;`) is trimmed via the
    // display-path pseudo-identity; and excising the last leaf of
    // `use util::{gadget}` (gadget survives — the app bin uses it) collapses
    // the whole statement instead of leaving `use util::{};`.
    run_fix_fixture("fix__unused_pub_collateral");
}

#[test]
fn fix_unused_pub_delete_test_scaffold() {
    // The test-scaffolding deletion: a `TestOnly` item (reached only from
    // test code) is deleted TOGETHER with its exclusively-scaffolding tests —
    // `alpha::embalmed` + the `#[cfg(test)]`-module test that calls it (and
    // its `use`), and `alpha::it_only` + the integration-test fn (one fixture
    // per referrer provenance: `+test` cfg variant, `target_kind = "test"`
    // crate). `alpha::kept` (production-used, also covered by a surviving
    // test) is untouched, and the blessed `expected/` tree compiles with its
    // remaining tests green.
    run_fix_fixture("fix__unused_pub_delete_test_scaffold");
}

#[test]
fn fix_unused_pub_test_only_veto() {
    // The scaffolding VETO: the only test reaching `alpha::embalmed` also
    // asserts on surviving `alpha::kept`, so it is not exclusive scaffolding
    // — deleting it would drop real coverage. Nothing is deleted (expected/
    // == input/); the finding is downgraded with a note naming the blocking
    // test.
    run_fix_fixture("fix__unused_pub_test_only_veto");
}

#[test]
fn fix_unused_pub_test_only_gate_veto() {
    // The LINT-layer veto: the engine clears `beta`'s test fn as exclusive
    // scaffolding of `alpha::embalmed`, but the test fn is allowlisted —
    // out of fix scope. Deleting the target without its test would break
    // `cargo test`, so the target stays too: nothing is deleted (expected/
    // == input/) and the note names the out-of-scope test item.
    run_fix_fixture("fix__unused_pub_test_only_gate_veto");
}

#[test]
fn fix_stale_expect() {
    // The stale `expect` directive is injected via setup.toml's `[[append]]`
    // (kept out of the committed input/ so dogfood stays green); `--fix`
    // deletes the now-pointless line.
    run_fix_fixture("fix__stale_expect");
}
