fn main() {
    // Cross-crate use of `demo::public_entry` keeps it (and, transitively,
    // `kept` / `extra_kept`) alive — only the genuinely dead chain is removed.
    println!("{}", demo::public_entry());
}
