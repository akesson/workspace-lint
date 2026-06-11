fn main() {
    // `demo::make()` returns Calc (inferred, never named); `.value()` is a
    // method call — neither writes `Calc` as a path the resolver can see.
    let c = demo::make();
    println!("{}", c.value());
}
