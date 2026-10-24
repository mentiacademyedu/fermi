# Test results

## Run manifest

| Field | Value |
| --- | --- |
| Run | `2026-10-24-f56fd58` (deterministic) and `2026-10-24-f56fd58-security`, plus the Linux CI security run on the same commit |
| Revision | `f56fd58` (full SHA in the run summaries); clean working tree |
| Date | 2026-10-24 |
| Environment | Windows 11 Home 10.0.26200, Rust 1.96.0 (`rust-toolchain.toml`), local developer machine |
| Providers | fake CLI only (`crates/acceptance-tests/src/bin/fake-cli.rs`); no live provider calls, no metered usage |
| Real tools | git, cargo (check, test, clippy) on fixture copies, the dashboard's HTTP API (VE-08) |
| Command | `cargo run -p xtask -- acceptance --implemented`, the same with `--profile security --allow-blocked`, and `--system SE --system MA --profile security` on Linux CI |
| Evidence | `tests/reports/<run>/<case>/<attempt>/` (result.json, manifest.json with SHA-256, artifacts); summaries and reports published in `docs/acceptance-runs/` |
| CI | GitHub Actions on Ubuntu runs the same gates on every push |

## Gate checks

| Check | Command | Result |
| --- | --- | --- |
| Formatting | `cargo fmt --all -- --check` | passed |
| Static analysis | `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| Unit tests | `cargo test --workspace --locked --lib --bins` | passed: 54 (fermi-core 41, fermi lib 12, fermi bin 1) |
| Acceptance, deterministic | `xtask acceptance --implemented` | passed: 61 of 61 executed cases, runner exit 0 |
| Acceptance, security (Windows) | `xtask acceptance --implemented --profile security --allow-blocked` | 13 passed, 4 blocked (PL-07, SE-01/04/05 need Linux Landlock), runner exit 0 |
| Acceptance, security (Linux CI) | `xtask acceptance --system SE --system MA --system PL --profile security` on Ubuntu | passed: all 12 (SE-01 … SE-08, MA-06, MA-07, PL-04, PL-07) on the real OS sandbox; 4 of these blocked on Windows (`docs/acceptance-runs/linux-ci-security/`) |
| Coverage | `cargo llvm-cov --workspace --locked` | 64.7% of product lines (see `TEST_COVERAGE.md`) |

## Acceptance outcomes

| Outcome | Cases |
| --- | --- |
| passed | 64 on Windows: the 57 before, plus PL-01, PL-02, PL-03, PL-04, PL-05, PL-06, PL-08. PL-07, SE-01, SE-04 and SE-05 pass on Linux CI (they are blocked on Windows), so 68 counting the Linux security run |
| failed | 0 |
| skipped | 0 |
| blocked | 4 on Windows (PL-07, SE-01, SE-04, SE-05: the OS sandbox needs Linux Landlock); 0 on Linux CI |
| not_implemented | 38 (no executable test; the capability is missing or only partly present, see `ACCEPTANCE_MATRIX.md`) |

**Overall acceptance: 68 / 106 passed** (64.2%), counting the Linux CI security run for the four OS-sandbox cases (64 / 106 on the Windows run, with those four blocked). Execution coverage: 68 / 106.

Every pass has a `result.json` whose checks all passed and whose artifacts match their recorded hashes. Each test declares what was real and what was mocked: model output is mocked (fake CLI); the engine, hub, team scheduler, code index, verification service, git, worktrees, compilers, test runners, files and processes are real.

**Six systems meet their completion gate (8 / 8): System 4 (persistent project memory), System 5 (integrated verification engine), System 7 (composable plugin system — PL-07 on Linux CI with the real OS sandbox, as its gate requires), System 8 (agent observability), System 9 (cost and performance optimization), and System 10 (security and isolation — all eight on Linux CI with the real OS sandbox).** One end-to-end workflow passes (E2E-08).

### Controls

| Control | Run | Result |
| --- | --- | --- |
| Negative: D-004 fix reverted | `negative-control` | AI-05 failed, runner exit 1 |
| Positive: fix restored | `positive-control` | AI-05 passed, runner exit 0 |
| Negative: merge gate removed (run ready without checks, merge ignores them) | `negative-control-ve08` | VE-08 failed (the API merged the run), runner exit 1 |
| Positive: gate restored | `2026-10-24-d44f68e` | VE-08 passed |
| Negative: budget reservations always admitted | `negative-control-budget` | CO-05, AI-07 and E2E-08 failed, runner exit 1 |
| Positive: reservations restored | `2026-10-24-d44f68e` | all three passed |
| In-test control (SE-06): the same injection without the deny_read policy | within the run | the canary reaches persisted output, so the scan is shown to catch a leak |
| Negative: every project shares one memory store | `negative-control-memory` | PM-08 failed, runner exit 1 |
| Positive: one store per project | `2026-10-24-e72c2c7` | PM-08 passed |
| In-test control (PM-07): the same planted instruction without the deny_read policy | each PM-07 run | the canary reaches persisted history, so the scan is shown to detect a leak |
| Negative: redaction turned off | `negative-control-redaction` | OB-02, OB-04 and OB-07 failed (canaries in history, tapes, exports and the live stream), runner exit 1 |
| Negative: model calls counted per stream line instead of per message id | `negative-control-calls` | OB-01 and OB-03 failed (extra calls, double-counted usage), runner exit 1 |
| Positive: both restored | `2026-10-24-d0302dc` | all eight OB cases passed |
| Negative: plugin argument scope check removed | `negative-control-plugin-scope` | PL-07 failed (the plugin read the sentinel through a path argument; canary in its output), runner exit 1 |
| Negative: a name collision resolved by silent overwrite | `negative-control-collisions` | PL-03 failed (one plugin's `greet` vanished, the bare name resolved), runner exit 1 |
| Positive: both restored | `2026-10-24-f56fd58` | all PL cases passed (PL-07 on Linux CI) |
| In-test control (OB-08): a tampered recording | each OB-08 run | the replay reports the difference and the state hashes differ |

### Defects

No product defect is open. Product defects found and fixed so far: CLI outages without failover (D-004), orphaned processes after cancel (D-005), four code-index gaps (D-006); plus gate and environment issues D-001, D-002, D-003, D-007. Design gaps in the merge gate and in redaction, and an egress-proxy regression while building plugins, were caught before release. Details in `KNOWN_DEFECTS.md`.

## Readiness verdict

**Not ready.** 67 of 96 system cases (counting Linux CI for the four OS-sandbox cases) and 1 of 10 end-to-end workflows pass. The cases that exist pass reliably; the gap is missing capability, not failing behavior.

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
| 7 Composable harness and plugin system | 8 / 8 on Linux CI (7 / 8 on Windows) | **verified** (real plugin manager, MCP client in both protocol eras, sandboxed plugin processes and Landlock on Linux; the plugin programs and model replies are test doubles) | plugin registration, disabling, command collisions, fault isolation, MCP interoperability, adapter substitution, boundary enforcement, API incompatibility |
| 8 Agent observability | 8 / 8 | **verified** (real adapters parsing each CLI's stream format, hub, history, redaction, tapes and replay; model output mock-only) | complete operation trace, invocation detail, usage fidelity, failure diagnostics, trace persistence, resume correlation, secret redaction, deterministic replay |
| 9 Cost and performance optimization | 8 / 8 | **verified** (exact arithmetic against a hand-worked oracle; ledger, routing and attribution with mock usage) | price tables and cost, cache discounts, economy and quality routing, shared reservations, versioned prices, context warnings, usage attribution |
| 10 Security and isolation | 8 / 8 on Linux CI (5 / 8 on Windows) | **verified** (real OS sandbox on Linux via Landlock; fermi's policy, approvals, environment allowlist and egress proxy on every platform) | forbidden reads, secret isolation, destructive-action gating, network restrictions, cross-task isolation, repository-injection resistance, scoped approvals, alternate-route denial |
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
| 2026-10-24-e72c2c7 | `e72c2c7`, clean | 45 / 45 executed passed (45 / 106); superseded by security and isolation |
| 2026-10-24-02130df | `02130df`, clean | 47 / 47 deterministic, 50 / 106 on Windows, 53 with Linux CI; superseded by agent observability |
| 2026-10-24-d0302dc | `d0302dc`, clean | 54 / 54 deterministic, 57 / 106 on Windows, 60 with Linux CI; superseded by the plugin system |
