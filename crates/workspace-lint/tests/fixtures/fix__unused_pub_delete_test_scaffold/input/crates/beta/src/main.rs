use alpha::kept;

fn main() {
    println!("{}", kept());
}

#[cfg(test)]
mod tests {
    use alpha::embalmed;

    #[test]
    fn exercises_embalmed() {
        assert_eq!(embalmed(), 1);
    }

    #[test]
    fn exercises_kept() {
        assert_eq!(alpha::kept(), 2);
    }
}
