# Test coverage

## Requirement coverage

| Measure | Value |
| --- | --- |
| Acceptance cases in the registry | 106 (`tests/acceptance/systems.yaml`) |
| Cases with an executable acceptance test | 91 |
| Execution coverage (cases executed / 106) | 91 / 106 |
| Overall acceptance (cases passed / 106) | 91 / 106 (85 on Windows; PL-07, SE-01/04/05, LR-02 and LR-03 pass on Linux CI, blocked on Windows) |
| Capabilities found in the audit | 89 implemented, 9 partial, 8 missing |
| Systems at their completion gate (8 / 8) | System 4 (project memory), System 5 (verification engine), System 6 (browser control), System 7 (plugins; 8/8 on Linux CI), System 8 (observability), System 9 (cost and performance), System 10 (security; 8/8 on Linux CI), System 11 (execution; 8/8 on Linux CI), System 12 (evaluation) |

All 89 capabilities audited as *implemented* have a passing test (six of them on Linux CI). Two cases audited as *partial* also pass: AI-01 (the three CLI adapters; the API adapters are untested) and MA-04 (conflicts go to a debugger agent instead of the user). The 7 other partial capabilities have no test yet: their missing piece is what the case checks, so the test waits for the capability.

## Code coverage

`cargo llvm-cov --workspace --locked` on Windows at `82eae66` (unit and acceptance tests together). The Linux-only sandbox code (Landlock) isn't exercised here; the egress proxy, environment allowlist and policy are:

| Scope | Lines | Functions |
| --- | --- | --- |
| fermi-core (…, verification, billing, memory, security, decks) | 76.5% (9598 / 12547) | 74.6% |
| fermi (CLI, TUI, server, setup, skills, slides) | 37.0% (1805 / 4884) | 34.7% |
| **Product total** | **65.4% (11403 / 17431)** | **62.7%** |

Excluded from the product total: `crates/acceptance-tests` (the harness), `xtask` (the runner) and `crates/fermi-desktop` (the Tauri shell, no tests). CI produces `lcov.info` on Ubuntu on every push and uploads it as an artifact.

Earlier measurements: 54.1% at `faa705c`, 59.1% at `19f85c1`, 61.9% at `e72c2c7`, 62.9% at `02130df`, 63.9% at `d0302dc`, 64.7% at `f56fd58`, 65.7% at `c3b1f83`, 66.4% at `45735d6`. New code is well covered: `remote.rs` 92% (the worker side, `worker.rs` and `worker_http.rs`, runs in separate processes the tests stop by killing them, so its coverage isn't recorded; LR-02 … LR-08 exercise it), `browser.rs` 83%, `eval.rs` 92%, `plugins.rs` 85%, `redact.rs` 95%, `trace.rs` 94%, `sandbox.rs` 95%, `policy.rs` 93%, `memory.rs` 94%, `billing.rs` 90%, `approvals.rs` 77%.

### Notable gaps

| Area | Lines covered | Why |
| --- | --- | --- |
| `fermi/src/server.rs` (dashboard API) | 29% | only the task, team, state, checks and memory endpoints used by VE-08, E2E-08 and PM-01/PM-08 are exercised |
| `fermi/src/main.rs` (CLI commands) | 0% | no CLI tests |
| `providers/api.rs`, `providers/image.rs` (API adapters, image generation) | 0% | no HTTP provider mock yet |
| `mcp.rs` (bridge tools) | 24% | only `run_checks` and the memory tools are called by tests; the others are exercised by agents, not tests |
| `sessions.rs` (import from other CLIs) | 51% | unit tests cover the parsers, not discovery on disk |

## Unsupported paths and exclusions

- The desktop shell and the web UI have no automated tests (dashboard tests with Playwright are not built yet). Dashboard changes are checked by hand with a headless browser.
- Live providers (T4) are never called by the default or CI runs.
- Container and remote execution cases need Docker or remote workers, neither available on the development machine.
