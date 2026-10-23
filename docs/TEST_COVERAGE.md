# Test coverage

## Requirement coverage

| Measure | Value |
| --- | --- |
| Acceptance cases in the registry | 106 (`tests/acceptance/systems.yaml`) |
| Cases with an executable acceptance test | 18 |
| Execution coverage (cases executed / 106) | 18 / 106 |
| Overall acceptance (cases passed / 106) | 18 / 106 |
| Capabilities found in the audit | 16 implemented, 24 partial, 66 missing |

All 16 capabilities audited as *implemented* have a passing test. Two cases audited as *partial* also pass: AI-01 (the three CLI adapters; the API adapters are untested) and MA-04 (conflicts go to a debugger agent instead of the user). The 22 other partial capabilities have no test yet: their missing piece is what the case checks, so the test waits for the capability.

## Code coverage

`cargo llvm-cov --workspace --locked` on Windows at `faa705c` (unit tests and acceptance tests together):

| Scope | Lines | Functions |
| --- | --- | --- |
| fermi-core (engine, providers, hub, teams, code index, decks) | 67.8% (4208 / 6210) | 64.5% |
| fermi (CLI, TUI, server, setup, skills, slides) | 33.6% (1390 / 4139) | 29.9% |
| **Product total** | **54.1% (5598 / 10349)** | **49.9%** |

Excluded from the product total: `crates/acceptance-tests` (the harness), `xtask` (the runner) and `crates/fermi-desktop` (the Tauri shell, 0%, no tests). CI produces `lcov.info` on Ubuntu on every push and uploads it as an artifact.

### Notable gaps

| Area | Lines covered | Why |
| --- | --- | --- |
| `fermi/src/server.rs` (dashboard API) | 0% | no HTTP-level tests; the web UI has none either |
| `fermi/src/main.rs` (CLI commands) | 0% | no CLI tests |
| `providers/api.rs`, `providers/image.rs` (API adapters, image generation) | 0% | no HTTP provider mock yet |
| `mcp.rs` (bridge tools) | 21% | bridge tools are exercised only by agents, not by tests |
| `sessions.rs` (import from other CLIs) | 51% | unit tests cover the parsers, not discovery on disk |

## Unsupported paths and exclusions

- The desktop shell and the web UI have no automated tests (dashboard tests with Playwright are not built yet).
- Live providers (T4) are never called by the default or CI runs.
- Container and remote execution cases need Docker or remote workers, neither available on the development machine.
