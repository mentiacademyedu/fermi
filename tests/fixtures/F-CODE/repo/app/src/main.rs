use engine::describe;
use util::num::helper;

// Mentions normalize() in a comment and "normalize" in a string: neither is a reference.
fn main() {
    let label = "normalize";
    println!("{} {} {}", describe(" Fermi "), helper(), label);
}
