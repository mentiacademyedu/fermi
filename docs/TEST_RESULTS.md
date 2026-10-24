# Test results

## Run manifest

| Field | Value |
| --- | --- |
| Run | `2026-10-23-19f85c1` (deterministic) and `2026-10-23-19f85c1-security` (security) |
| Revision | `19f85c1` (full SHA in the run summaries), clean working tree (recorded by the runner at start) |
| Date | 2026-10-23 |
| Environment | Windows 11 Home 10.0.26200, Rust 1.96.0 (`rust-toolchain.toml`), local developer machine |
| Providers | fake CLI only (`crates/acceptance-tests/src/bin/fake-cli.rs`); no live provider calls, no metered usage |
| Real tools | git, cargo (check, test, clippy) on fixture copies, the dashboard's HTTP API (VE-08) |
| Command | `ACCEPTANCE_RUN_ID=2026-10-23-19f85c1 cargo run -p xtask -- acceptance --implemented` and the same with `--profile security` |
| Evidence | `tests/reports/<run>/<case>/<attempt>/` (result.json, manifest.json with SHA-256, artifacts); summaries and reports published in `docs/acceptance-runs/` |
| CI | GitHub Actions on Ubuntu runs the same gates on every push |

## Gate checks

| Check | Command | Result |
| --- | --- | --- |
| Formatting | `cargo fmt --all -- --check` | passed |
| Static analysis | `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| Unit tests | `cargo test --workspace --locked --lib --bins` | passed: 42 (fermi-core 29, fermi lib 12, fermi bin 1) |
| Acceptance, deterministic | `xtask acceptance --implemented` | passed: 27 of 27 executed cases, runner exit 0 |
| Acceptance, security | `xtask acceptance --implemented --profile security` | passed: MA-06, MA-07, runner exit 0 |
| Coverage | `cargo llvm-cov --workspace --locked` | see `TEST_COVERAGE.md` |

## Acceptance outcomes

| Outcome | Cases |
| --- | --- |
| passed | 27: AI-01, AI-02, AI-03, AI-05, MA-01 (20 of 20 repetitions), MA-02, MA-03, MA-04, MA-06, MA-07, MA-08, CI-01, CI-02, CI-03, CI-05, CI-07, CI-08, VE-01 … VE-08, OB-05, LR-01 |
| failed | 0 |
| skipped | 0 |
| blocked | 0 |
| not_implemented | 79 (no executable test; the capability is missing or only partly present, see `ACCEPTANCE_MATRIX.md`) |

**Overall acceptance: 27 / 106 passed** (25.5%). Execution coverage: 27 / 106.

Every pass has a `result.json` whose checks all passed and whose artifacts match their recorded hashes. Each test declares what was real and what was mocked: model output is mocked (fake CLI); the engine, hub, team scheduler, code index, verification service, git, worktrees, compilers, test runners, files and processes are real.

**System 5 (integrated verification engine) meets its completion gate: 8 / 8.**

### Controls

| Control | Run | Result |
| --- | --- | --- |
| Negative: D-004 fix reverted | `negative-control` | AI-05 failed, runner exit 1 |
| Positive: fix restored | `positive-control` | AI-05 passed, runner exit 0 |
| Negative: merge gate removed (run ready without checks, merge ignores them) | `negative-control-ve08` | VE-08 failed (the API merged the run), runner exit 1 |
| Positive: gate restored | `2026-10-23-19f85c1` | VE-08 passed |

### Defects

No product defect is open. Product defects found and fixed so far: CLI outages without failover (D-004), orphaned processes after cancel (D-005), four code-index gaps (D-006); plus gate and environment issues D-001, D-002, D-003, D-007. A design gap in the new merge gate was caught before commit. Details in `KNOWN_DEFECTS.md`.

## Readiness verdict

**Not ready.** 27 of 96 system cases and 0 of 10 end-to-end workflows pass. The cases that exist pass reliably; the gap is missing capability, not failing behavior.

## Verified status of the 12 systems

*Mock-only verified* means the case passed with real fermi components and a simulated model; no live provider was used.

| System | Passed | Status | Basis |
| --- | --- | --- | --- |
| 1 Model-agnostic AI engine | 4 / 8 | partly verified (mock-only) | normalization (three CLI adapters), common interface, model switching, outage failover. Missing: 429 backoff, capability validation, budgets; API adapters untested (no HTTP mock) |
| 2 Multi-agent orchestration | 7 / 8 | partly verified (mock-only) | concurrency (20 repetitions), dependencies, independent and conflicting edits, crash isolation, cancellation, verified integration. Missing: scoped delegation |
| 3 Semantic codebase intelligence | 6 / 8 | partly verified (real, against F-CODE ground truth) | indexing, lookup, callers, incremental refresh, impact, identity. Missing: semantic rename, parse-error reporting |
| 4 Persistent project memory | 0 / 8 | unverified | missing |
| 5 Integrated verification engine | 8 / 8 | **verified** (real compilers, test runners, linter and scanner; agent turns mock-only) | diagnostics, test feedback, repair loop, lint, advisories, structured edits, bounded repairs, completion gate |
| 6 Browser and application control | 0 / 8 | unverified | missing (the Preview tab is user-facing only) |
| 7 Composable harness and plugin system | 0 / 8 | unverified | 2 partial, 6 missing |
| 8 Agent observability | 1 / 8 | partly verified (mock-only) | trace persistence across restart |
| 9 Cost and performance optimization | 0 / 8 | unverified | 3 partial, 5 missing |
| 10 Security and isolation | 0 / 8 | unverified | 3 partial, 5 missing |
| 11 Local and remote execution | 1 / 8 | partly verified (mock-only model, real cargo test) | local execution |
| 12 Evaluation and self-improvement | 0 / 8 | unverified | missing |
| End-to-end workflows | 0 / 10 | unverified | 6 partial, 4 missing |

## Earlier runs

| Run | Revision | Result |
| --- | --- | --- |
| phase1-baseline | `522e356` | fmt failed (651 differences), clippy passed, 37 unit tests passed, coverage blocked, 0 / 106 acceptance |
| 2026-10-23-0221093 | `0221093` (flagged dirty: documentation edited during the run) | 18 / 18 executed passed; superseded |
| 2026-10-23-114516b | `114516b`, clean | 18 / 18 executed passed; superseded when AI-01 gained the Gemini adapter |
| 2026-10-23-faa705c | `faa705c`, clean | 18 / 18 executed passed (18 / 106); superseded by the verification engine |
