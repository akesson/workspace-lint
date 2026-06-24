// No `use` — `Secret` is reached through a fully-qualified path. The deny
// targets `data-models::internal::**`; the only exception is the bare
// `data-models::internal` module (a shorter ancestor of `Secret`). Because the
// exception is above the denied prefix, it must NOT exempt this reference.
pub fn touch() -> data_models::internal::Secret {
    data_models::internal::Secret::new()
}
