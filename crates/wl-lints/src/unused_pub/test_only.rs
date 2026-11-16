//! The advice on an "only used by test code" finding, decided by *which*
//! test code uses the item. The fix depends on it:
//!
//! - only the defining crate's own unit tests → `#[cfg(test)]` works;
//! - another crate's tests, or an integration test / bench (a separate
//!   crate even in the same package) → those compile this crate as a plain
//!   dependency with `cfg(test)` OFF, so a `#[cfg(test)]` gate breaks them;
//! - the item is already `#[cfg(test)]` → gating is no advice at all.

use wl_engine::semantic::{PubCandidate, SemanticModel, TestUser};

/// Help line + "used by" note for one test-only finding.
pub(super) struct TestOnlyAdvice {
    pub(super) help: &'static str,
    /// `None` when the engine resolved no test referrer (reach credited
    /// across configs only) — the generic help applies then.
    pub(super) users_note: Option<String>,
    pub(super) no_fix_note: &'static str,
}

impl TestOnlyAdvice {
    pub(super) fn for_item(model: &SemanticModel, cand: &PubCandidate, crate_code: &str) -> Self {
        let users = &cand.test_users;
        let already_test_only = model.defined_only_under_test(&cand.id);
        let own_only = users.iter().all(|u| *u == TestUser::OwnUnitTests);
        let help = match (already_test_only, users.is_empty(), own_only) {
            (true, _, _) => {
                "it is already compiled only for tests, where nothing outside this crate can \
                 see it — narrow it to `pub(crate)`"
            }
            (false, true, _) => "gate it `#[cfg(test)]`, move it into test code, or remove it",
            (false, false, true) => {
                "only this crate's own unit tests use it — gate it `#[cfg(test)]` and narrow \
                 it to `pub(crate)`, move it into the test module, or remove it (doctests are \
                 not analyzed: a doc example using it would break)"
            }
            (false, false, false) => {
                "`#[cfg(test)]` would break the users below: other crates' tests and \
                 integration tests compile this crate without `cfg(test)` — move it into a \
                 test-support crate or feature, mark it as deliberate test support with \
                 `expect`, or remove it together with its tests"
            }
        };
        let no_fix_note = if already_test_only {
            "no fix is auto-applied: narrow it by hand"
        } else {
            "no fix is auto-applied: `pub(crate)` would trip `dead_code` on the non-test \
             build, and deleting the item would break the tests that reference it"
        };
        let users_note = (!users.is_empty()).then(|| {
            let list: Vec<String> = users.iter().map(|u| describe(u, crate_code)).collect();
            format!("used only by {}", list.join(", "))
        });
        Self {
            help,
            users_note,
            no_fix_note,
        }
    }
}

fn describe(user: &TestUser, crate_code: &str) -> String {
    match user {
        TestUser::OwnUnitTests => format!("`{crate_code}`'s own unit tests"),
        TestUser::UnitTests { krate } => format!("`{krate}`'s unit tests"),
        TestUser::BinUnitTests { krate } => format!("binary `{krate}`'s unit tests"),
        TestUser::TestTarget { krate } => format!("test target `{krate}`"),
    }
}
