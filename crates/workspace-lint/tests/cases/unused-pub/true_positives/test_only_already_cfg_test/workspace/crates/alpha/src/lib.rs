pub fn shipped() -> u32 {
    1
}

#[cfg(test)]
pub fn helper() -> u32 {
    shipped() + 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn uses_helper() {
        assert_eq!(super::helper(), 2);
    }
}
