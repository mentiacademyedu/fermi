pub fn reverse(s: &str) -> String {
    String::from_utf8_lossy(&s.bytes().rev().collect::<Vec<u8>>()).into_owned()
}
