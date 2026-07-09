pub fn beta() -> u32 {
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_beta() {
        assert_eq!(beta(), 2);
    }
}
