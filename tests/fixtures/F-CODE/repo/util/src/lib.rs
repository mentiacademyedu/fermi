pub mod num;
pub mod text;

/// Trims and lowercases a name.
pub fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trims() {
        assert_eq!(normalize(" A "), "a");
    }
}
