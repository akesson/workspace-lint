pub fn child_item() {}

// `sub.rs` owns the `sub/` directory: this file child lives at
// `src/sub/leaf.rs` (the `foo.rs`-owns-`foo/` convention).
pub mod leaf;

// An inline module declared inside `sub.rs` owns a deeper dir: the file child
// `nested` lives at `src/sub/wrap/nested.rs`.
pub mod wrap {
    pub mod nested;
}
