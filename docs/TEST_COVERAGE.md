# Test coverage

## Requirement coverage

| Measure | Value |
| --- | --- |
| Acceptance cases in the registry | 106 (`tests/acceptance/systems.yaml`) |
| Cases with an executable acceptance test | 0 |
| Execution coverage (cases executed / 106) | 0 / 106 |
| Overall acceptance (cases passed / 106) | 0 / 106 |
| Capabilities found in the audit | 11 implemented, 29 partial, 66 missing |

## Code coverage

Not available: cargo-llvm-cov is not installed (defect D-002). The frontend (`crates/fermi/web/index.html`) has no automated tests.

## Existing tests (not acceptance evidence)

37 unit tests across `fermi-core` (code index, deck writer, hub history and model switching, router, sessions import, team plans, permission rules) and `fermi` (file access checks, preview server, setup, skills, slides, trending, TUI). They exercise pieces of some capabilities with small inline fixtures; none uses the catalog's fixtures or oracles, records evidence, or maps to a case ID.

## Unsupported paths and exclusions

To be declared in Phase 2 with the harness (for example: the desktop shell `crates/fermi-desktop`, live-provider adapters outside the T4 profile).
