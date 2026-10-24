use eval_fizzbuzz::fizzbuzz;

#[test]
fn hidden() {
    assert_eq!(fizzbuzz(5), "Buzz");
    assert_eq!(fizzbuzz(15), "FizzBuzz");
    assert_eq!(fizzbuzz(7), "7");
}
