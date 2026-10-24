use eval_palindrome::is_palindrome;

#[test]
fn hidden() {
    assert!(is_palindrome("A man, a plan, a canal: Panama"));
    assert!(!is_palindrome("rust"));
    assert!(is_palindrome(""));
}
