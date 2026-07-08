//! `--baseline-write`: regenerate the `[duplicate-code]` accepted-clone
//! baseline. Lints never touch the filesystem, so the write lives here in the
//! binary; the group collection is the build-free
//! `wl_lints::duplicate_code::collect_baseline` (the `measure`/`--stats`
//! precedent). No clean-git gate: the baseline is a single reviewable file and
//! regeneration is idempotent.

use wl_lint_api::util;

use crate::config;

/// Collect every clone group `duplicate-code` currently reports and overwrite
/// the configured baseline file, then exit. Diverges (never returns).
pub(crate) fn run(config: &config::Config) -> ! {
    let Some(dc) = &config.duplicate_code else {
        util::fail("--baseline-write needs a [duplicate-code] table in your config");
    };
    let Some(rel) = &dc.baseline else {
        util::fail("--baseline-write needs `baseline = \"<path>\"` in [duplicate-code]");
    };
    let fast = crate::load_fast_model();
    let file = wl_lints::duplicate_code::collect_baseline(&fast, dc);
    let path = fast.root().join(rel);
    if let Err(e) = std::fs::write(&path, file.render()) {
        util::fail(format!("writing {}: {e}", path.display()));
    }
    eprintln!(
        "wrote {} clone group(s) to {}",
        file.groups(),
        rel.display()
    );
    std::process::exit(0);
}
