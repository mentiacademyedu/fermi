# Test coverage

## Requirement coverage

| Measure | Value |
| --- | --- |
| Acceptance cases in the registry | 106 (`tests/acceptance/systems.yaml`) |
| Cases with an executable acceptance test | 45 |
| Execution coverage (cases executed / 106) | 45 / 106 |
| Overall acceptance (cases passed / 106) | 45 / 106 |
| Capabilities found in the audit | 43 implemented, 21 partial, 42 missing |
| Systems at their completion gate (8 / 8) | System 4 (project memory), System 5 (verification engine), System 9 (cost and performance) |

All 43 capabilities audited as *implemented* have a passing test. Two cases audited as *partial* also pass: AI-01 (the three CLI adapters; the API adapters are untested) and MA-04 (conflicts go to a debugger agent instead of the user). The 19 other partial capabilities have no test yet: their missing piece is what the case checks, so the test waits for the capability.

## Code coverage

`cargo llvm-cov --workspace --locked` on Windows at `e72c2c7` (unit tests and acceptance tests together; the only uncommitted changes were documentation):

| Scope | Lines | Functions |
| --- | --- | --- |
| fermi-core (engine, providers, hub, teams, code index, verification, billing, memory, decks) | 73.1% (6266 / 8573) | 71.5% |
| fermi (CLI, TUI, server, setup, skills, slides) | 40.2% (1771 / 4406) | 37.9% |
| **Product total** | **61.9% (8037 / 12979)** | **59.3%** |

Excluded from the product total: `crates/acceptance-tests` (the harness), `xtask` (the runner) and `crates/fermi-desktop` (the Tauri shell, no tests). CI produces `lcov.info` on Ubuntu on every push and uploads it as an artifact.

Earlier measurements: 54.1% at `faa705c`, 59.1% at `19f85c1`, 60.9% at `d44f68e`. New code is well covered: `memory.rs` 94%, `policy.rs` 94%, `billing.rs` 90%, `router.rs` 95%, `verify.rs` 78%.

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
