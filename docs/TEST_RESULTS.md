# Test results

## Run manifest: Phase 1 baseline

| Field | Value |
| --- | --- |
| Run | phase1-baseline |
| Revision | `522e356eac8aa3a3cc0e7fa5cbd87af8afafa443` (working tree clean apart from the untracked `docs/`) |
| Date | 2026-10-23 |
| Environment | Windows 11 Home 10.0.26200, rustfmt 1.9.0, Cargo stable, local developer machine |
| Profile | deterministic (existing unit tests only); no live provider calls |
| Acceptance runner | not yet implemented (Phase 2) |

## Gate checks

| Check | Command | Result |
| --- | --- | --- |
| Formatting | `cargo fmt --all -- --check` | **failed**: 651 differences; the code base has never been run through rustfmt (defect D-001) |
| Static analysis | `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| Unit and integration tests | `cargo test --workspace --locked` | passed: 37 tests (fermi lib 12, fermi bin 1, fermi-core 24, fermi-desktop 0) |
| Coverage | `cargo llvm-cov --workspace --locked --lcov --output-path lcov.info` | **blocked**: cargo-llvm-cov is not installed (defect D-002) |

## Acceptance outcomes

No acceptance test exists yet, so no case was executed.

| Outcome | Cases |
| --- | --- |
| passed | 0 |
| failed | 0 |
| skipped | 0 |
| blocked | 0 |
| not_implemented | 106 (no executable test; see `ACCEPTANCE_MATRIX.md` for the capability audit) |
| Execution state | not_run for all 106 |

## Readiness verdict

**Not ready.** 0 of 96 system cases and 0 of 10 end-to-end workflows have passed. The formatting gate fails and coverage cannot be produced yet.

## Verified status of the 12 systems

| System | Status | Basis |
| --- | --- | --- |
| 1 Model-agnostic AI engine | unverified | 2 implemented, 3 partial, 3 missing (audit); no acceptance run |
| 2 Multi-agent orchestration | unverified | 3 implemented, 4 partial, 1 missing; manual fixture runs only, not evidence |
| 3 Semantic codebase intelligence | unverified | 4 implemented, 2 partial, 2 missing; unit tests on small fixtures, not the F-CODE oracle |
| 4 Persistent project memory | unverified | missing |
| 5 Integrated verification engine | unverified | missing |
| 6 Browser and application control | unverified | missing (the Preview tab is user-facing only) |
| 7 Composable harness and plugin system | unverified | 2 partial, 6 missing |
| 8 Agent observability | unverified | 1 implemented, 5 partial, 2 missing |
| 9 Cost and performance optimization | unverified | 3 partial, 5 missing |
| 10 Security and isolation | unverified | 3 partial, 5 missing |
| 11 Local and remote execution | unverified | 1 implemented, 1 partial, 6 missing |
| 12 Evaluation and self-improvement | unverified | missing |
| End-to-end workflows | unverified | 6 partial, 4 missing |

No capability is labelled *mock-only verified* or *real integration verified* yet.
