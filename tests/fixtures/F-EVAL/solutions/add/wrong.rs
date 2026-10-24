pub fn add(a: i64, b: i64) -> i64 {
    (a as i32).wrapping_add(b as i32) as i64
}
