use lib_a::shared_fn;

// Not pub - won't trigger the visibility check on itself.
pub(crate) fn use_it() {
    shared_fn();
}
