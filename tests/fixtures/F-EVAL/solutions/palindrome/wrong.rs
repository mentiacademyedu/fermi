pub fn is_palindrome(s: &str) -> bool {
    s.chars().eq(s.chars().rev())
}
