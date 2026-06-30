// `generated.rs` is pulled in via include! (Tier-1, offline). The generated-code
// drop lazily loads a workspace so the oversized generated file is recognized and
// dropped, even though file-size loads no workspace itself. This handwritten file
// stays under the threshold.
include!("generated.rs");
