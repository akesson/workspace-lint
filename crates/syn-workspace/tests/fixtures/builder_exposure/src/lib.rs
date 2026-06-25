//! Fixture for builder-macro attribute recognition (`module_tree/signature.rs`).
//! Every type a builder attribute promotes into a generated `build()` signature
//! lives in a private `mod inner` so its canonical is unambiguous; the asserting
//! test (`builder_exposure_*` in `module_tree/tests.rs`) checks exactly which
//! ones the walk records.

use derive_builder::Builder;
use typed_builder::TypedBuilder;

// typed_builder: `into = <Type>` becomes the generated `build()` return type.
#[derive(TypedBuilder)]
#[builder(build_method(into = Result<Foo, inner::TbErr>))]
pub struct Foo {
    pub x: u32,
}

// typed_builder with an earlier `vis = "..."` key — exercises skipping an
// unrecognized sibling to still reach `into`.
#[derive(TypedBuilder)]
#[builder(build_method(vis = "pub", into = Result<Bar, inner::TbErrVis>))]
pub struct Bar {
    pub y: u32,
}

// derive_builder: `error = "<Type>"` becomes the generated `build()` error type.
#[derive(Builder)]
#[builder(build_fn(error = "inner::DbErr"))]
pub struct Baz {
    z: u32,
}

// --- negatives: must NOT be recorded as exposures ---

// A bare `into` (no `= <type>`) yields a generic builder — nothing to record.
#[derive(TypedBuilder)]
#[builder(build_method(into))]
pub struct BareInto {
    pub w: u32,
}

// A non-`build_method`/`build_fn` item-level key naming a private type must be
// skipped (targeted recognition, not a broad attribute scan).
#[derive(TypedBuilder)]
#[builder(crate_module_path = inner::NotExposed)]
pub struct Other {
    pub v: u32,
}

// `build_fn(private)` makes `build` private — no public error exposure, and the
// group form must be consumed without panicking.
#[derive(Builder)]
#[builder(build_fn(private))]
pub struct Hidden {
    h: u32,
}

mod inner {
    pub struct TbErr;
    pub struct TbErrVis;
    pub struct DbErr;
    pub struct NotExposed;
}
