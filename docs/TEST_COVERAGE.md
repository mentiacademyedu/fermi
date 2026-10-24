# Test coverage

## Requirement coverage

| Measure | Value |
| --- | --- |
| Acceptance cases in the registry | 106 (`tests/acceptance/systems.yaml`) |
| Cases with an executable acceptance test | 53 |
| Execution coverage (cases executed / 106) | 53 / 106 |
| Overall acceptance (cases passed / 106) | 53 / 106 (50 on Windows; SE-01/04/05 pass on Linux CI, blocked on Windows) |
| Capabilities found in the audit | 51 implemented, 17 partial, 38 missing |
| Systems at their completion gate (8 / 8) | System 4 (project memory), System 5 (verification engine), System 9 (cost and performance), System 10 (security; 8/8 on Linux CI) |

All 51 capabilities audited as *implemented* have a passing test (the three OS-sandbox cases on Linux CI). Two cases audited as *partial* also pass: AI-01 (the three CLI adapters; the API adapters are untested) and MA-04 (conflicts go to a debugger agent instead of the user). The 15 other partial capabilities have no test yet: their missing piece is what the case checks, so the test waits for the capability.

## Code coverage

`cargo llvm-cov --workspace --locked` on Windows at `02130df` (unit and acceptance tests together). The Linux-only sandbox code (Landlock) isn't exercised here; the egress proxy, environment allowlist and policy are:

| Scope | Lines | Functions |
| --- | --- | --- |
| fermi-core (…, verification, billing, memory, security, decks) | 74.2% (6719 / 9058) | 72.7% |
| fermi (CLI, TUI, server, setup, skills, slides) | 40.0% (1772 / 4435) | 37.6% |
| **Product total** | **62.9% (8491 / 13493)** | **60.4%** |

Excluded from the product total: `crates/acceptance-tests` (the harness), `xtask` (the runner) and `crates/fermi-desktop` (the Tauri shell, no tests). CI produces `lcov.info` on Ubuntu on every push and uploads it as an artifact.

Earlier measurements: 54.1% at `faa705c`, 59.1% at `19f85c1`, 61.9% at `e72c2c7`. New code is well covered: `sandbox.rs` 95%, `policy.rs` 93%, `memory.rs` 94%, `billing.rs` 90%, `approvals.rs` 77%.

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
