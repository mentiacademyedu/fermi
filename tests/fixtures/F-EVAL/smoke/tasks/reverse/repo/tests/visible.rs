use eval_reverse::reverse;

#[test]
fn visible() {
    assert_eq!(reverse("abc"), "cba");
}
