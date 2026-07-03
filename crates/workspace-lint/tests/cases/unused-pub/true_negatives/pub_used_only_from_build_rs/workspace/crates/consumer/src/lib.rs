// No pub surface of its own — the case isolates the build.rs edge: `tools`'s
// pub fn must read as used solely because consumer's build script calls it.
#[allow(dead_code)]
fn output() -> &'static str {
    env!("TOOLS_OUT")
}
