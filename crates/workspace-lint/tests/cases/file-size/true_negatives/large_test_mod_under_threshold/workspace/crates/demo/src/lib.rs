pub fn one() {}
pub fn two() {}
pub fn three() {}

#[cfg(test)]
mod tests {
    #[test]
    fn t1() {
        assert!(true);
    }
    #[test]
    fn t2() {
        assert!(true);
    }
    #[test]
    fn t3() {
        assert!(true);
    }
}
