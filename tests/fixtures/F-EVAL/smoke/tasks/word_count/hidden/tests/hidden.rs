use eval_word_count::word_count;

#[test]
fn hidden() {
    assert_eq!(word_count(""), 0);
    assert_eq!(word_count("  a	b
 c  "), 3);
    assert_eq!(word_count("solo"), 1);
}
