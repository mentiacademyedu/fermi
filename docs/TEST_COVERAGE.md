# Test coverage

## Requirement coverage

| Measure | Value |
| --- | --- |
| Acceptance cases in the registry | 106 (`tests/acceptance/systems.yaml`) |
| Cases with an executable acceptance test | 97 |
| Execution coverage (cases executed / 106) | 97 / 106 |
| Overall acceptance (cases passed / 106) | 97 / 106 (91 on Windows; PL-07, SE-01/04/05, LR-02 and LR-03 pass on Linux CI, blocked on Windows) |
| Capabilities found in the audit | 96 implemented, 7 partial, 3 missing |
| Systems at their completion gate (8 / 8) | all twelve: System 1 (AI engine), System 2 (orchestration), System 3 (code intelligence), System 4 (project memory), System 5 (verification engine), System 6 (browser control), System 7 (plugins; 8/8 on Linux CI), System 8 (observability), System 9 (cost and performance), System 10 (security; 8/8 on Linux CI), System 11 (execution; 8/8 on Linux CI), System 12 (evaluation) |

All 96 capabilities audited as *implemented* have a passing test (six of them on Linux CI). One case audited as *partial* also passes: MA-04 (conflicts go to a debugger agent instead of the user). The 6 other partial capabilities, and the 3 missing ones, are end-to-end workflows with no test yet.

## Code coverage

`cargo llvm-cov --workspace --locked` on Windows at `cab8247` (unit and acceptance tests together). The Linux-only sandbox code (Landlock) isn't exercised here; Linux CI's `lcov.info` for the same commit (run 40153065374) gives 66.6% of product lines:

| Scope | Lines | Functions |
| --- | --- | --- |
| fermi-core (…, verification, billing, memory, security, decks) | 77.6% (10080 / 12986) | 75.7% |
| fermi (CLI, TUI, server, setup, skills, slides) | 36.9% (1805 / 4889) | 34.7% |
| **Product total** | **66.5% (11885 / 17875)** | **63.7%** |

Excluded from the product total: `crates/acceptance-tests` (the harness), `xtask` (the runner) and `crates/fermi-desktop` (the Tauri shell, no tests). CI produces `lcov.info` on Ubuntu on every push and uploads it as an artifact.

Earlier measurements: 54.1% at `faa705c`, 59.1% at `19f85c1`, 61.9% at `e72c2c7`, 62.9% at `02130df`, 63.9% at `d0302dc`, 64.7% at `f56fd58`, 65.7% at `c3b1f83`, 66.4% at `45735d6`, 65.4% at `82eae66`. New code is well covered: `providers/api.rs` 70% (0% before the mock endpoints; 86% on Linux, where AI-08's separate process is recorded too), `engine.rs` 90%, `clock.rs` 87%, `code.rs` 79%, `remote.rs` 92% (the worker side, `worker.rs` and `worker_http.rs`, runs in separate processes the tests stop by killing them, so its coverage isn't recorded; LR-02 … LR-08 exercise it), `browser.rs` 83%, `sandbox.rs` 89%.

### Notable gaps

| Area | Lines covered | Why |
| --- | --- | --- |
| `fermi/src/server.rs` (dashboard API) | 29% | only the task, team, state, checks and memory endpoints used by VE-08, E2E-08 and PM-01/PM-08 are exercised |
| `fermi/src/main.rs` (CLI commands) | 0% | no CLI tests |
| `providers/image.rs` (image generation) | 0% | the mock endpoints don't serve images |
| `mcp.rs` (bridge tools) | 27% | the bridge runs as its own process in most tests (MA-05, BA, PL), whose coverage isn't recorded; in process, only `run_checks`, the memory and browser tools are called |
| `sessions.rs` (import from other CLIs) | 51% | unit tests cover the parsers, not discovery on disk |

## Unsupported paths and exclusions

- The desktop shell and the web UI have no automated tests (dashboard tests with Playwright are not built yet). Dashboard changes are checked by hand with a headless browser.
- Live providers (T4) are never called by the default or CI runs.
- Container and remote execution cases need Docker or remote workers, neither available on the development machine.
