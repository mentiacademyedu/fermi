use util::normalize;
use util::text::helper;

/// Normalizes a name and scores it.
pub fn process(name: &str) -> (String, u32) {
    (normalize(name), helper())
}

/// Describes a name as "name:score".
pub fn describe(name: &str) -> String {
    let (n, s) = process(name);
    format!("{n}:{s}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_scores() {
        assert_eq!(process(" X ").1, 1);
    }
}

/// Greets a normalized name.
pub fn greet(name: &str) -> String {
    format!("hello {}", process(name).0)
}

#[test]
fn greet_uses_the_normalized_name() {
    assert_eq!(greet(" Ada "), "hello ada");
}
