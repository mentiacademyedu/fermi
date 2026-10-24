use eval_add::add;

#[test]
fn hidden() {
    assert_eq!(add(0, 0), 0);
    assert_eq!(add(-7, 3), -4);
    assert_eq!(add(1_000_000_000_000, 1), 1_000_000_000_001);
}
