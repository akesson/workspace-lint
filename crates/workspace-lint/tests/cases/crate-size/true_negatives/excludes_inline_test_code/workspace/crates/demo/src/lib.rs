pub fn one() {}
pub fn two() {}
pub fn three() {}
pub fn four() {}
pub fn five() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn t_one() {
        one();
    }

    #[test]
    fn t_two() {
        two();
    }

    #[test]
    fn t_three() {
        three();
    }
}
