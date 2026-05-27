// apps-foo imports helper::process_user — looks innocent. But helper
// internally touches data-models::internal, transitively making apps-foo
// depend on data internals. The architecture check (v1, direct-imports
// only) sees only the apps-foo → helper edge and misses this. Tracked as
// known_false_negative.

use helper::process_user;

pub fn touch() {
    let _user = process_user();
}
