# Test results

## Run manifest

| Field | Value |
| --- | --- |
| Run | `2026-10-24-e72c2c7` (deterministic) and `2026-10-24-e72c2c7-security` (security) |
| Revision | `e72c2c7` (full SHA in the run summaries); both runs on a clean working tree |
| Date | 2026-10-24 |
| Environment | Windows 11 Home 10.0.26200, Rust 1.96.0 (`rust-toolchain.toml`), local developer machine |
| Providers | fake CLI only (`crates/acceptance-tests/src/bin/fake-cli.rs`); no live provider calls, no metered usage |
| Real tools | git, cargo (check, test, clippy) on fixture copies, the dashboard's HTTP API (VE-08) |
| Command | `ACCEPTANCE_RUN_ID=2026-10-24-e72c2c7 cargo run -p xtask -- acceptance --implemented` and the same with `--profile security` |
| Evidence | `tests/reports/<run>/<case>/<attempt>/` (result.json, manifest.json with SHA-256, artifacts); summaries and reports published in `docs/acceptance-runs/` |
| CI | GitHub Actions on Ubuntu runs the same gates on every push |

## Gate checks

| Check | Command | Result |
| --- | --- | --- |
| Formatting | `cargo fmt --all -- --check` | passed |
| Static analysis | `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| Unit tests | `cargo test --workspace --locked --lib --bins` | passed: 48 (fermi-core 35, fermi lib 12, fermi bin 1) |
| Acceptance, deterministic | `xtask acceptance --implemented` | passed: 45 of 45 executed cases, runner exit 0 |
| Acceptance, security | `xtask acceptance --implemented --profile security` | passed: MA-06, MA-07, PM-07, PM-08, E2E-08, runner exit 0 |
| Coverage | `cargo llvm-cov --workspace --locked` | 61.9% of product lines (see `TEST_COVERAGE.md`) |

## Acceptance outcomes

| Outcome | Cases |
| --- | --- |
| passed | 45: AI-01, AI-02, AI-03, AI-05, AI-07, MA-01 (20 of 20 repetitions), MA-02, MA-03, MA-04, MA-06, MA-07, MA-08, CI-01, CI-02, CI-03, CI-05, CI-07, CI-08, PM-01 … PM-08, VE-01 … VE-08, OB-05, CO-01 … CO-08, LR-01, E2E-08 |
| failed | 0 |
| skipped | 0 |
| blocked | 0 |
| not_implemented | 61 (no executable test; the capability is missing or only partly present, see `ACCEPTANCE_MATRIX.md`) |

**Overall acceptance: 45 / 106 passed** (42.5%). Execution coverage: 45 / 106.

Every pass has a `result.json` whose checks all passed and whose artifacts match their recorded hashes. Each test declares what was real and what was mocked: model output is mocked (fake CLI); the engine, hub, team scheduler, code index, verification service, git, worktrees, compilers, test runners, files and processes are real.

**Three systems meet their completion gate (8 / 8): System 4, persistent project memory (including restart persistence and cross-project isolation, as its gate requires); System 5, integrated verification engine; System 9, cost and performance optimization.** One end-to-end workflow passes (E2E-08).

### Controls

| Control | Run | Result |
| --- | --- | --- |
| Negative: D-004 fix reverted | `negative-control` | AI-05 failed, runner exit 1 |
| Positive: fix restored | `positive-control` | AI-05 passed, runner exit 0 |
| Negative: merge gate removed (run ready without checks, merge ignores them) | `negative-control-ve08` | VE-08 failed (the API merged the run), runner exit 1 |
| Positive: gate restored | `2026-10-24-d44f68e` | VE-08 passed |
| Negative: budget reservations always admitted | `negative-control-budget` | CO-05, AI-07 and E2E-08 failed, runner exit 1 |
| Positive: reservations restored | `2026-10-24-e72c2c7` | all three passed |
| Negative: every project shares one memory store | `negative-control-memory` | PM-08 failed, runner exit 1 |
| Positive: one store per project | `2026-10-24-e72c2c7` | PM-08 passed |
| In-test control (PM-07): the same planted instruction without the deny_read policy | each PM-07 run | the canary reaches persisted history, so the scan is shown to detect a leak |

### Defects

No product defect is open. Product defects found and fixed so far: CLI outages without failover (D-004), orphaned processes after cancel (D-005), four code-index gaps (D-006); plus gate and environment issues D-001, D-002, D-003, D-007. A design gap in the new merge gate was caught before commit. Details in `KNOWN_DEFECTS.md`.

## Readiness verdict

**Not ready.** 44 of 96 system cases and 1 of 10 end-to-end workflows pass. The cases that exist pass reliably; the gap is missing capability, not failing behavior.

## Verified status of the 12 systems

*Mock-only verified* means the case passed with real fermi components and a simulated model; no live provider was used.

| System | Passed | Status | Basis |
| --- | --- | --- | --- |
| 1 Model-agnostic AI engine | 5 / 8 | partly verified (mock-only) | normalization (three CLI adapters), common interface, model switching, outage failover, budget exhaustion. Missing: 429 backoff, capability validation; API adapters untested (no HTTP mock) |
| 2 Multi-agent orchestration | 7 / 8 | partly verified (mock-only) | concurrency (20 repetitions), dependencies, independent and conflicting edits, crash isolation, cancellation, verified integration. Missing: scoped delegation |
| 3 Semantic codebase intelligence | 6 / 8 | partly verified (real, against F-CODE ground truth) | indexing, lookup, callers, incremental refresh, impact, identity. Missing: semantic rename, parse-error reporting |
| 4 Persistent project memory | 8 / 8 | **verified** (real store, restarts, HTTP API and agent tools; the agent's behavior and Claude's read-deny enforcement are mocked in PM-07) | durable decisions, fresh-session retrieval, versioning, stale sources, retrieval of paraphrases (lexical v1, not embeddings), contradictions, untrusted content, project isolation |
| 5 Integrated verification engine | 8 / 8 | **verified** (real compilers, test runners, linter and scanner; agent turns mock-only) | diagnostics, test feedback, repair loop, lint, advisories, structured edits, bounded repairs, completion gate |
| 6 Browser and application control | 0 / 8 | unverified | missing (the Preview tab is user-facing only) |
| 7 Composable harness and plugin system | 0 / 8 | unverified | 2 partial, 6 missing |
| 8 Agent observability | 1 / 8 | partly verified (mock-only) | trace persistence across restart |
| 9 Cost and performance optimization | 8 / 8 | **verified** (exact arithmetic against a hand-worked oracle; ledger, routing and attribution with mock usage) | price tables and cost, cache discounts, economy and quality routing, shared reservations, versioned prices, context warnings, usage attribution |
| 10 Security and isolation | 0 / 8 | unverified | 4 partial (SE-01 now has a deny_read policy for Claude agents), 4 missing |
| 11 Local and remote execution | 1 / 8 | partly verified (mock-only model, real cargo test) | local execution |
| 12 Evaluation and self-improvement | 0 / 8 | unverified | missing |
| End-to-end workflows | 1 / 10 | partly verified | E2E-08 (task exceeding a budget); 6 partial, 3 missing |

## Earlier runs

| Run | Revision | Result |
| --- | --- | --- |
| phase1-baseline | `522e356` | fmt failed (651 differences), clippy passed, 37 unit tests passed, coverage blocked, 0 / 106 acceptance |
| 2026-10-23-0221093 | `0221093` (flagged dirty: documentation edited during the run) | 18 / 18 executed passed; superseded |
| 2026-10-23-114516b | `114516b`, clean | 18 / 18 executed passed; superseded when AI-01 gained the Gemini adapter |
| 2026-10-23-faa705c | `faa705c`, clean | 18 / 18 executed passed (18 / 106); superseded by the verification engine |
| 2026-10-23-19f85c1 | `19f85c1`, clean | 27 / 27 executed passed (27 / 106); superseded by costs and budgets |
| 2026-10-24-d44f68e | `d44f68e`, clean (security run flagged dirty: documentation edited during it) | 37 / 37 executed passed (37 / 106); superseded by project memory |
