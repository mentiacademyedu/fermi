use eval_word_count::word_count;

#[test]
fn visible() {
    assert_eq!(word_count("one two"), 2);
}
