# Test coverage

## Requirement coverage

| Measure | Value |
| --- | --- |
| Acceptance cases in the registry | 106 (`tests/acceptance/systems.yaml`) |
| Cases with an executable acceptance test | 37 |
| Execution coverage (cases executed / 106) | 37 / 106 |
| Overall acceptance (cases passed / 106) | 37 / 106 |
| Capabilities found in the audit | 35 implemented, 20 partial, 51 missing |
| Systems at their completion gate (8 / 8) | System 5 (verification engine), System 9 (cost and performance) |

All 35 capabilities audited as *implemented* have a passing test. Two cases audited as *partial* also pass: AI-01 (the three CLI adapters; the API adapters are untested) and MA-04 (conflicts go to a debugger agent instead of the user). The 18 other partial capabilities have no test yet: their missing piece is what the case checks, so the test waits for the capability.

## Code coverage

`cargo llvm-cov --workspace --locked` on Windows at `d44f68e` (unit tests and acceptance tests together; the only uncommitted changes were documentation):

| Scope | Lines | Functions |
| --- | --- | --- |
| fermi-core (engine, providers, hub, teams, code index, verification, billing, decks) | 72.0% (5795 / 8046) | 69.7% |
| fermi (CLI, TUI, server, setup, skills, slides) | 40.2% (1729 / 4299) | 37.3% |
| **Product total** | **60.9% (7524 / 12345)** | **57.7%** |

Excluded from the product total: `crates/acceptance-tests` (the harness), `xtask` (the runner) and `crates/fermi-desktop` (the Tauri shell, no tests). CI produces `lcov.info` on Ubuntu on every push and uploads it as an artifact.

Earlier measurements: 54.1% at `faa705c`, 59.1% at `19f85c1`. New code is well covered: `billing.rs` 90%, `router.rs` 95%, `verify.rs` 78%.

### Notable gaps

| Area | Lines covered | Why |
| --- | --- | --- |
| `fermi/src/server.rs` (dashboard API) | 27% | only the task, team, state and checks endpoints used by VE-08 and E2E-08 are exercised |
| `fermi/src/main.rs` (CLI commands) | 0% | no CLI tests |
| `providers/api.rs`, `providers/image.rs` (API adapters, image generation) | 0% | no HTTP provider mock yet |
| `mcp.rs` (bridge tools) | 23% | only `run_checks` is called by a test (VE-02); the other tools are exercised by agents, not tests |
| `sessions.rs` (import from other CLIs) | 51% | unit tests cover the parsers, not discovery on disk |

## Unsupported paths and exclusions

- The desktop shell and the web UI have no automated tests (dashboard tests with Playwright are not built yet). Dashboard changes are checked by hand with a headless browser.
- Live providers (T4) are never called by the default or CI runs.
- Container and remote execution cases need Docker or remote workers, neither available on the development machine.
