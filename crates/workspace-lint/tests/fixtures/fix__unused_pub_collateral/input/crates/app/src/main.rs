fn main() {
    // Keeps `demo::entry` and `util::gadget` alive cross-crate.
    println!("{}", demo::entry() + util::gadget());
}
