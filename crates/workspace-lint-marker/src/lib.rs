//! Marker macros for silencing `workspace-lint` diagnostics at the call site.
//!
//! Add this crate as a dependency (typically renamed to `workspace_lint` for
//! ergonomics) and use the macros at item position to suppress diagnostics
//! that workspace-lint would otherwise emit for the enclosing scope:
//!
//! ```toml
//! [dependencies]
//! workspace_lint = { package = "workspace-lint-marker", version = "0.1" }
//! ```
//!
//! ```ignore
//! workspace_lint::allow!(file_size);          // silence one lint
//! workspace_lint::allow!(file_size, unused_pub);  // silence several
//! workspace_lint::expect!(unused_deps);       // silence but warn if stale
//! ```
//!
//! At compile time the macros expand to nothing. workspace-lint scans the
//! source text and treats each invocation as a suppression directive for the
//! enclosing file or item. Unknown lint names produce a compile error.

/// Silence the listed `workspace-lint` lints for the enclosing scope.
///
/// Expands to nothing. workspace-lint parses the invocation to build its
/// suppression map.
#[macro_export]
macro_rules! allow {
    (file_size) => {};
    (crate_size) => {};
    (centralized_deps) => {};
    (freshness) => {};
    (cli_crate_version) => {};
    (unused_deps) => {};
    (unused_pub) => {};
    (stale_expect) => {};
    (architecture) => {};
    (module_tree) => {};
    (stale_git_index) => {};
    (feature_drift) => {};
    (visibility) => {};
    ($first:ident, $($rest:ident),+ $(,)?) => {
        $crate::allow!($first);
        $crate::allow!($($rest),+);
    };
}

/// Silence the listed `workspace-lint` lints for the enclosing scope, but
/// warn (via the `workspace-lint::stale-expect` diagnostic) if no matching
/// diagnostic actually fires during a run that exercised that lint.
///
/// Use this when you want to make sure the silence stays load-bearing — for
/// example, after fixing a violation, the stale-expect warning will nudge
/// you to remove the directive.
#[macro_export]
macro_rules! expect {
    (file_size) => {};
    (crate_size) => {};
    (centralized_deps) => {};
    (freshness) => {};
    (cli_crate_version) => {};
    (unused_deps) => {};
    (unused_pub) => {};
    (stale_expect) => {};
    (architecture) => {};
    (module_tree) => {};
    (stale_git_index) => {};
    (feature_drift) => {};
    (visibility) => {};
    ($first:ident, $($rest:ident),+ $(,)?) => {
        $crate::expect!($first);
        $crate::expect!($($rest),+);
    };
}

#[cfg(test)]
mod tests {
    // The macros expand to nothing — these tests exist to prove the strict
    // patterns accept every documented lint name and reject typos.

    #[test]
    fn allow_accepts_each_known_lint_individually() {
        crate::allow!(file_size);
        crate::allow!(crate_size);
        crate::allow!(centralized_deps);
        crate::allow!(freshness);
        crate::allow!(cli_crate_version);
        crate::allow!(unused_deps);
        crate::allow!(unused_pub);
        crate::allow!(stale_expect);
        crate::allow!(architecture);
        crate::allow!(module_tree);
        crate::allow!(stale_git_index);
        crate::allow!(feature_drift);
        crate::allow!(visibility);
    }

    #[test]
    fn allow_accepts_comma_lists() {
        crate::allow!(file_size, unused_pub);
        crate::allow!(file_size, unused_pub, centralized_deps);
        crate::allow!(file_size, unused_pub, centralized_deps, unused_deps,);
    }

    #[test]
    fn expect_accepts_each_known_lint_individually() {
        crate::expect!(file_size);
        crate::expect!(crate_size);
        crate::expect!(centralized_deps);
        crate::expect!(freshness);
        crate::expect!(cli_crate_version);
        crate::expect!(unused_deps);
        crate::expect!(unused_pub);
        crate::expect!(stale_expect);
        crate::expect!(architecture);
        crate::expect!(module_tree);
        crate::expect!(stale_git_index);
        crate::expect!(feature_drift);
        crate::expect!(visibility);
    }

    #[test]
    fn expect_accepts_comma_lists() {
        crate::expect!(file_size, unused_pub);
        crate::expect!(file_size, unused_pub, centralized_deps,);
    }

    // Compile-fail tests for typos live as trybuild fixtures alongside the
    // workspace-lint integration tests. Adding trybuild here would force a
    // dev-dep, breaking the zero-dependency goal of this crate.
}
