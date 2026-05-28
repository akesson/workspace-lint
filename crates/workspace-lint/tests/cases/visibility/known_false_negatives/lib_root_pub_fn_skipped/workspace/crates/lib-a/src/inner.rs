use crate::internal_only;

// Not pub — won't trigger the visibility check on itself.
pub(crate) fn caller() {
    internal_only();
}
