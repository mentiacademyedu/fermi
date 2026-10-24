# Test coverage

## Requirement coverage

| Measure | Value |
| --- | --- |
| Acceptance cases in the registry | 106 (`tests/acceptance/systems.yaml`) |
| Cases with an executable acceptance test | 76 |
| Execution coverage (cases executed / 106) | 76 / 106 |
| Overall acceptance (cases passed / 106) | 76 / 106 (72 on Windows; PL-07 and SE-01/04/05 pass on Linux CI, blocked on Windows) |
| Capabilities found in the audit | 74 implemented, 10 partial, 22 missing |
| Systems at their completion gate (8 / 8) | System 4 (project memory), System 5 (verification engine), System 7 (plugins; 8/8 on Linux CI), System 8 (observability), System 9 (cost and performance), System 10 (security; 8/8 on Linux CI), System 12 (evaluation) |

All 74 capabilities audited as *implemented* have a passing test (the four OS-sandbox cases on Linux CI). Two cases audited as *partial* also pass: AI-01 (the three CLI adapters; the API adapters are untested) and MA-04 (conflicts go to a debugger agent instead of the user). The 8 other partial capabilities have no test yet: their missing piece is what the case checks, so the test waits for the capability.

## Code coverage

`cargo llvm-cov --workspace --locked` on Windows at `c3b1f83` (unit and acceptance tests together). The Linux-only sandbox code (Landlock) isn't exercised here; the egress proxy, environment allowlist and policy are:

| Scope | Lines | Functions |
| --- | --- | --- |
| fermi-core (…, verification, billing, memory, security, decks) | 77.5% (8741 / 11281) | 75.4% |
| fermi (CLI, TUI, server, setup, skills, slides) | 37.9% (1805 / 4765) | 36.1% |
| **Product total** | **65.7% (10546 / 16046)** | **63.1%** |

Excluded from the product total: `crates/acceptance-tests` (the harness), `xtask` (the runner) and `crates/fermi-desktop` (the Tauri shell, no tests). CI produces `lcov.info` on Ubuntu on every push and uploads it as an artifact.

Earlier measurements: 54.1% at `faa705c`, 59.1% at `19f85c1`, 61.9% at `e72c2c7`, 62.9% at `02130df`, 63.9% at `d0302dc`, 64.7% at `f56fd58`. New code is well covered: `eval.rs` 92%, `plugins.rs` 85%, `redact.rs` 95%, `trace.rs` 94%, `sandbox.rs` 95%, `policy.rs` 93%, `memory.rs` 94%, `billing.rs` 90%, `approvals.rs` 77%.

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
