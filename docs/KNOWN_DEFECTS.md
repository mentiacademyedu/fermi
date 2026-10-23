# Known defects

Defects found while establishing the Phase 1 baseline. Missing capabilities are tracked in `ACCEPTANCE_MATRIX.md`, not here.

## D-001: formatting gate fails

| Field | Value |
| --- | --- |
| Severity | medium (blocks the mandatory PR gate) |
| Classification | product implementation (code style) |
| Reproduction | `cargo fmt --all -- --check` at `522e356` exits 1 with 651 differences |
| Cause | the code base has never been formatted with rustfmt; lines exceed the default 100-column width |
| Impact | gate 1 (formatting and static analysis) cannot pass |
| Remediation | agree on a `rustfmt.toml` (for example `max_width = 140`), run `cargo fmt --all` in one mechanical commit, then keep the check in CI |
| Rerun | `cargo fmt --all -- --check` |
| Owner | coding agent, after owner approval of the style settings |

## D-002: coverage tool unavailable

| Field | Value |
| --- | --- |
| Severity | low (blocks the coverage deliverable) |
| Classification | environment |
| Reproduction | `cargo llvm-cov --version` returns "no such command" |
| Impact | no Rust line, region or function coverage can be produced |
| Remediation | `cargo install cargo-llvm-cov` and `rustup component add llvm-tools-preview` locally and in CI |
| Rerun | `cargo llvm-cov --workspace --locked --lcov --output-path lcov.info` |
| Owner | coding agent |

## D-003: no pinned toolchain or CI

| Field | Value |
| --- | --- |
| Severity | medium |
| Classification | environment |
| Reproduction | no `rust-toolchain.toml`, no CI configuration in the repository |
| Impact | results cannot be tied to a declared toolchain; no gate runs on pull requests |
| Remediation | add `rust-toolchain.toml` and a CI workflow running the mandatory checks (Phase 6) |
| Owner | coding agent |
