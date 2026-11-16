fn main() {
    let _live = store::make_live();
    println!("{} {}", store::make_mixed().kept(), store::Port::DEFAULT);
}
