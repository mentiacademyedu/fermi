/// The text helper.
pub fn helper() -> u32 {
    // Rewritten by a structured edit.
    10
}

/// Mutual recursion: a reference cycle.
pub fn ping(n: u32) -> u32 {
    if n == 0 { 0 } else { pong(n - 1) }
}

pub fn pong(n: u32) -> u32 {
    if n == 0 { 1 } else { ping(n - 1) }
}
