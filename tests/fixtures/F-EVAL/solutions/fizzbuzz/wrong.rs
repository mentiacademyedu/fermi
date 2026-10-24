pub fn fizzbuzz(n: u32) -> String {
    if n % 3 == 0 { "Fizz".into() } else if n % 5 == 0 { "Buzz".into() } else { n.to_string() }
}
