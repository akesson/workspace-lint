// `AF` (a `pub const` in a private `mod data`) is referenced only through a
// function-local glob import — `use data::*;` inside a fn body — then a bare
// `AF`. The resolver must honor glob `use` statements nested in fn bodies, not
// just module-level ones, or `AF` reads as unused. Real-world trigger: a
// 460-const auto-generated country table whose every const is reached this way.
mod data {
    pub const AF: &str = "af";
}

fn country_code() -> &'static str {
    use data::*;
    AF
}
