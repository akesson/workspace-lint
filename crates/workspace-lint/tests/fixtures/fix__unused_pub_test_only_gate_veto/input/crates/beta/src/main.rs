fn main() {
    println!("{}", alpha::kept());
}

#[cfg(test)]
mod tests {
    #[test]
    fn exercises_embalmed() {
        assert_eq!(alpha::embalmed(), 1);
    }
}
