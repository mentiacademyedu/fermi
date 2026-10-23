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
