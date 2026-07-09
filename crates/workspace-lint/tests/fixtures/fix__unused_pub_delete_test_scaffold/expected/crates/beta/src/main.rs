use alpha::kept;

fn main() {
    println!("{}", kept());
}

#[cfg(test)]
mod tests {
        #[test]
    fn exercises_kept() {
        assert_eq!(alpha::kept(), 2);
    }
}
