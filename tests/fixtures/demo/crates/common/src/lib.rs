pub fn answer() -> u32 {
    42
}

#[cfg(test)]
mod tests {
    #[test]
    fn answer_is_right() {
        assert_eq!(super::answer(), dev_helper::expected());
    }
}
