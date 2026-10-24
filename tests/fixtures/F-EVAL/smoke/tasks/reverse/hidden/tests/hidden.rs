use eval_reverse::reverse;

#[test]
fn hidden() {
    assert_eq!(reverse(""), "");
    assert_eq!(reverse("héllo"), "olléh");
    assert_eq!(reverse("ab cd"), "dc ba");
}
