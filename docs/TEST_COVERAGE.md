# Test coverage

## Requirement coverage

| Measure | Value |
| --- | --- |
| Acceptance cases in the registry | 106 (`tests/acceptance/systems.yaml`) |
| Cases with an executable acceptance test | 27 |
| Execution coverage (cases executed / 106) | 27 / 106 |
| Overall acceptance (cases passed / 106) | 27 / 106 |
| Capabilities found in the audit | 25 implemented, 23 partial, 58 missing |
| Systems at their completion gate (8 / 8) | System 5, integrated verification engine |

All 25 capabilities audited as *implemented* have a passing test. Two cases audited as *partial* also pass: AI-01 (the three CLI adapters; the API adapters are untested) and MA-04 (conflicts go to a debugger agent instead of the user). The 21 other partial capabilities have no test yet: their missing piece is what the case checks, so the test waits for the capability.

## Code coverage

`cargo llvm-cov --workspace --locked` on Windows at `19f85c1` (unit tests and acceptance tests together; the only uncommitted changes were documentation and the dashboard's HTML):

| Scope | Lines | Functions |
| --- | --- | --- |
| fermi-core (engine, providers, hub, teams, code index, verification, decks) | 70.3% (5235 / 7445) | 67.8% |
| fermi (CLI, TUI, server, setup, skills, slides) | 39.3% (1651 / 4199) | 36.1% |
| **Product total** | **59.1% (6886 / 11644)** | **55.6%** |

Excluded from the product total: `crates/acceptance-tests` (the harness), `xtask` (the runner) and `crates/fermi-desktop` (the Tauri shell, no tests). CI produces `lcov.info` on Ubuntu on every push and uploads it as an artifact.

Previous measurement (`faa705c`): 54.1% of product lines. The verification engine (`verify.rs`, 78% of lines) and the dashboard API exercised by VE-08 (`server.rs`, 0% → 21%) account for most of the change.

### Notable gaps

| Area | Lines covered | Why |
| --- | --- | --- |
| `fermi/src/server.rs` (dashboard API) | 21% | only the task, team and checks endpoints used by VE-08 are exercised |
| `fermi/src/main.rs` (CLI commands) | 0% | no CLI tests |
| `providers/api.rs`, `providers/image.rs` (API adapters, image generation) | 0% | no HTTP provider mock yet |
| `mcp.rs` (bridge tools) | 23% | only `run_checks` is called by a test (VE-02); the other tools are exercised by agents, not tests |
| `sessions.rs` (import from other CLIs) | 51% | unit tests cover the parsers, not discovery on disk |

## Unsupported paths and exclusions

- The desktop shell and the web UI have no automated tests (dashboard tests with Playwright are not built yet). Dashboard changes are checked by hand with a headless browser.
- Live providers (T4) are never called by the default or CI runs.
- Container and remote execution cases need Docker or remote workers, neither available on the development machine.
