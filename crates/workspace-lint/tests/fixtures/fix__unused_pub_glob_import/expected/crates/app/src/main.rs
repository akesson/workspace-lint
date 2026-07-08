mod gone;
mod kept_macro;
mod kept_trait;
mod stale;

fn main() {
    let _ = kept_macro::build();
    println!("{} {}", kept_trait::speak(), stale::keep());
}
