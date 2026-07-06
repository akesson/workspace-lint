//! Zero-overhead-when-off hierarchical phase timer for the perf study.
//!
//! Gated on the `WL_TIMING` env var (any value enables). When off, [`phase`]
//! is a straight call through to the closure — no clock read, no allocation.
//! When on, each phase prints one line to stderr on exit, indented by nesting
//! depth, so a run's cost decomposes into a readable (post-order) tree:
//!
//! ```text
//! [wl-timing]     1.9ms    ├ preflight
//! [wl-timing]   118.2ms    ├ build_dylib
//! [wl-timing]   402.7ms    ├ dylint_run[default]
//! [wl-timing]   540.1ms  ├ extract
//! ```
//!
//! The timer is intentionally a study instrument, not a product feature: it is
//! single-mechanism (no aggregation, no percentiles) and reads its gate once.

use std::fmt::Display;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

static DEPTH: AtomicUsize = AtomicUsize::new(0);

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("WL_TIMING").is_some())
}

/// Run `f`, and — when `WL_TIMING` is set — print its wall-clock cost tagged
/// `name`, indented by the current nesting depth. Returns `f`'s value.
///
/// Wall-clock (not CPU) is deliberate: most phases block on a spawned
/// `cargo`/`rustc`/`rustup` subprocess, and that wait *is* the cost we study.
///
/// `name` is `impl Display` (not `&str`) so a dynamic label can be passed as
/// `format_args!(..)` — that borrows its args and formats only on the enabled
/// path, keeping the off path allocation-free even for interpolated names.
pub fn phase<T>(name: impl Display, f: impl FnOnce() -> T) -> T {
    if !enabled() {
        return f();
    }
    let depth = DEPTH.fetch_add(1, Ordering::Relaxed);
    let start = Instant::now();
    let out = f();
    let elapsed = start.elapsed();
    DEPTH.fetch_sub(1, Ordering::Relaxed);
    // Post-order: children print before their parent. The tree glyph marks the
    // depth so the parent (less-indented, printed last of its subtree) is easy
    // to spot.
    let indent = "  ".repeat(depth);
    eprintln!(
        "[wl-timing] {:>9.1?}  {}{} {}",
        elapsed,
        indent,
        if depth == 0 { "▶" } else { "├" },
        name
    );
    out
}
