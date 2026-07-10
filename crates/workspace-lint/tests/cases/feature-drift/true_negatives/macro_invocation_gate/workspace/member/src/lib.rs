macro_rules! gated {
    (if $($cfg:tt)*) => {
        #[cfg($($cfg)*)]
        pub fn only_special() -> u32 {
            42
        }
    };
}
gated!(if feature = "special");

pub fn always() -> u32 {
    1
}
