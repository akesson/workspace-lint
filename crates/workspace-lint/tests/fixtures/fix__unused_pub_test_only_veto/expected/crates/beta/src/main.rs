fn main() {
    println!("{}", alpha::kept());
}

#[cfg(test)]
mod tests {
    #[test]
    fn covers_both() {
        assert_eq!(alpha::embalmed(), 1);
        assert_eq!(alpha::kept(), 2);
    }
}
