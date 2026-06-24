use demo::Value;

fn main() {
    // `demo::make()` returns `impl Value`; the concrete `Calc` is never named.
    // `.value()` is a trait-method call — neither writes `Calc` as a path the syn
    // resolver can see, but rust-analyzer resolves it to `<Calc as Value>::value`.
    let c = demo::make();
    println!("{}", c.value());
}
