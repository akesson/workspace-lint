fn main() {
    let mut total = utils::always_used();
    #[cfg(target_arch = "wasm32")]
    {
        total += utils::tz_offset_minutes();
    }
    println!("{total}");
}
