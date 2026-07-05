fn main() {
    // Keeps `demo::entry` and `util::thing` alive cross-crate.
    println!("{}", demo::entry() + util::thing());
}
