# Known defects

Defects found by the baseline and the acceptance tests. Missing capabilities are tracked in `ACCEPTANCE_MATRIX.md`, not here.

**Open: none.** Every defect below was fixed and its case rerun; each product fix was confirmed with a negative control (the fix reverted, the case fails) where noted.

| ID | Summary | Classification | Found by | State |
| --- | --- | --- | --- | --- |
| D-001 | formatting gate failed | product (code style) | baseline | fixed in `0221093` |
| D-002 | coverage tool unavailable | environment | baseline | fixed (tooling installed, CI step) |
| D-003 | no pinned toolchain or CI | environment | baseline | fixed in `0221093` |
| D-004 | CLI provider outages did not fail over | product | AI-05 | fixed in `0221093` |
| D-005 | cancel left processes started by the agent running | product | MA-07 | fixed in `0221093` |
| D-006 | code index gaps: unsupported files, revision, scope, qualified names | product | CI-01, CI-03, CI-07, CI-08 | fixed in `0221093` |
| D-007 | fixtures could check out with CRLF on Windows | environment | MA-03 | fixed (`.gitattributes`) |

## D-001: formatting gate failed

| Field | Value |
| --- | --- |
| Severity | medium (blocked the mandatory PR gate) |
| Reproduction | `cargo fmt --all -- --check` at `522e356` exited 1 with 651 differences |
| Cause | the code base had never been formatted with rustfmt |
| Fix | `rustfmt.toml` (`max_width = 140`, `use_small_heuristics = "Max"`), one mechanical `cargo fmt --all`, check in CI |
| Rerun | `cargo fmt --all -- --check` passes |

## D-002: coverage tool unavailable

| Field | Value |
| --- | --- |
| Severity | low |
| Reproduction | `cargo llvm-cov --version` returned "no such command" |
| Fix | `llvm-tools-preview` in `rust-toolchain.toml`, cargo-llvm-cov installed locally and in CI (`taiki-e/install-action`) |
| Rerun | `cargo llvm-cov --workspace --locked --lcov --output-path lcov.info` |

## D-003: no pinned toolchain or CI

| Field | Value |
| --- | --- |
| Severity | medium |
| Fix | `rust-toolchain.toml` (1.96.0 with rustfmt, clippy, llvm-tools-preview) and `.github/workflows/ci.yml` running fmt, clippy `-D warnings`, unit tests, the acceptance runner and coverage, uploading `tests/reports/` and `lcov.info` |

## D-004: CLI provider outages did not fail over

| Field | Value |
| --- | --- |
| Severity | high (a provider outage ended the task instead of moving to the next account) |
| Case | AI-05 provider outage handling |
| Reproduction | two Claude accounts in one profile; the first account's CLI prints `API Error: 503 Service Unavailable` and exits 1 |
| Expected | Failover event, the second account completes the task |
| Actual | the task failed with the 503 text; the second account was never called |
| Cause | the CLI adapters classified errors only as limit or auth; anything else was non-retryable. The API adapters already treated 5xx as retryable. |
| Fix | `providers/mod.rs`: `classify_error` treats 5xx, 529, overloaded, unavailable, gateway and connection-reset text as retryable for all CLI adapters (unit test `outages_fail_over_and_bugs_do_not`) |
| Controls | negative control run `negative-control` (fix reverted): AI-05 failed, runner exit 1. Positive control `positive-control`: passed, exit 0. |

## D-005: cancel left processes started by the agent running

| Field | Value |
| --- | --- |
| Severity | high (orphaned processes keep running commands after the user cancels) |
| Case | MA-07 parent cancellation |
| Reproduction | a team run whose agent CLI starts a long-running child (`fake-cli sleep 120`), then the run is cancelled |
| Expected | every agent and every process it started is gone; queued tasks never start |
| Actual | the CLI process was killed but its child kept running |
| Cause | `kill_on_drop` kills only the direct child |
| Fix | `providers/proc.rs`: on Unix the CLI starts in its own process group and `Drop` kills the group; on Windows `Drop` runs `taskkill /T /F` on the tree |

## D-006: code index gaps

| Field | Value |
| --- | --- |
| Severity | medium |
| Cases | CI-01, CI-03, CI-07, CI-08 against the F-CODE ground truth |
| Actual | unsupported files were skipped silently (CI-01); the index didn't record which git revision it reflects (CI-01); callers and impact didn't declare what the static analysis can't see (CI-03, CI-07); `util::text::helper` didn't resolve, only `helper` or a path (CI-08) |
| Fix | `code.rs`: the index stores unsupported files and the HEAD revision; `stats`, `symbol` and `impact` include a `scope` statement (static calls and imports; dynamic dispatch, macros and reflection not covered); module-qualified lookup matches the path segments against the file's module path. Cache format bumped to 5. |

## D-007: fixtures could check out with CRLF on Windows

| Field | Value |
| --- | --- |
| Severity | low (false failures, no product impact) |
| Case | MA-03 (merged tree hash differed from the oracle) |
| Cause | Git for Windows `core.autocrlf=true` converted line endings when the harness committed fixture copies, and would also convert them in a fresh clone |
| Fix | the harness sets `core.autocrlf=false` in fixture repositories; `.gitattributes` pins `tests/fixtures/**` to LF |

## Test-implementation issues (not product defects)

Found and fixed while writing the tests; listed so the history is complete.

| Case | Issue | Fix |
| --- | --- | --- |
| AI-03, AI-05 | accounts reported "not logged in": the harness wrote no credential files, so the engine skipped them | `Fermi::new` writes stand-in credential files per provider |
| AI-01 | the check required identical event sequences, but optional text deltas differ by provider | compare the lifecycle (routed, tools, usage, done) without deltas |
| MA-07 | the fake CLI started its child after waiting on the barrier, so cancel arrived before the child existed | start the child first, then wait |
| VE-08 | read the task's history file right after the task left `running` in memory; the hub writes the file a moment later | poll the file until it shows the final state |
| MA-08 | asked for a task's diff without a file, which returns a diffstat, then looked for code in it | ask for each worker's file |
| VE-08 control | the first negative-control mutation didn't compile, so the case "failed" for the wrong reason; discarded | a compiling mutation (no checks, merge without the gate): VE-08 failed, runner exit 1 |

## Design gap caught before release

While adding the merge gate, the first version told the user to fix failing checks "in a task's conversation", but a task branch already merged into the integration branch is never merged again, so such a fix could not reach what gets merged. Fixed before commit: failing checks on the integrated branch now start a debugger step in the integration worktree (bounded by `max_repairs`) and the checks run again; MA-08 covers it.

## Environment notes

- Release and full-workspace builds on the development machine were killed for low memory; use `CARGO_BUILD_JOBS=4` (2 for release builds).
