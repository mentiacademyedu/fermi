# F-EVAL

`smoke/` is the evaluation dataset the EV cases run (`dataset.toml`, version 1): five small Rust
crates, each with a stub to implement (`src/lib.rs`), visible tests the candidate may read but
not change (`tests/`, protected), and hidden tests only the evaluator lays over the work when
scoring (`hidden/`). Each task scores 5: it builds (1), the visible tests pass (1), the hidden
tests pass (3). Five tasks are a smoke test, not a benchmark of model quality.

`solutions/<task>/correct.rs` and `wrong.rs` are reference implementations the tests script the
(fake) candidate with: a correct one, and one that passes the visible test but fails the hidden
ones (the known failures of EV-03). They are not part of the dataset, so the dataset's hash
doesn't depend on them.
