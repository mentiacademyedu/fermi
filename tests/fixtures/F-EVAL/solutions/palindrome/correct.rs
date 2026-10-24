pub fn is_palindrome(s: &str) -> bool {
    let v: Vec<char> = s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect();
    v.iter().eq(v.iter().rev())
}
