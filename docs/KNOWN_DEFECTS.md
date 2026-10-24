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
| PM-08 | the canary scan of project B's results flagged the search query the test itself sent, which the API echoes back | scan the results, not the echoed query |
| PM-05 (found while checking the oracle by hand) | the stemmer didn't reduce "logs" or "traces" to their synonym group | tightened the stemmer rules before the run |
| E2E-08 | counted every fake-CLI invocation as a model call, but the dashboard runs the CLI once at start to check the login (`auth status`) | count only calls that carry a prompt |
| VE-08 control | the first negative-control mutation didn't compile, so the case "failed" for the wrong reason; discarded | a compiling mutation (no checks, merge without the gate): VE-08 failed, runner exit 1 |

## Test-implementation issues found while building System 10 (no product defects)

| Case | Issue | Fix |
| --- | --- | --- |
| SE-01 | the fake CLI emulating Claude's read-deny rules matched the raw `file_path`, so a relative `../` path slipped past the rule, while Claude Code resolves it first; and identical deny rules group into one Blocked event, so counting events undercounted | resolve a Read's path against the working directory (and normalize Windows drives) before matching, as Claude does; assert the denial and the canary scan, and prove every path at the OS boundary |
| SE security CI step | a `blocked` case (a runner without Landlock) failed the step | `xtask --allow-blocked` counts a blocked case as acceptable, not a pass, for that step |

## Found while building System 12 (no open defects)

| Kind | Issue | Fix |
| --- | --- | --- |
| Product race, caught by OB-06 when the machine ran low on memory | a window that cancelled a task does one last save; if another window had resumed the task meanwhile, that late save put the old, cancelled run back over the resumed one | a hub or recorder never saves over a record with a later attempt; `hub::tests::late_save_keeps_a_resumed_attempt` reproduces it deterministically and fails without the fix |
| Design gap, caught by EV-06 on Windows | a read-only file can still be deleted or replaced on Windows (Rust's `remove_file` and `rename` ignore the attribute), and a folder's read-only flag doesn't stop new files in it | while the candidate runs, the evaluator holds protected files open without delete or write sharing, and protected folders get a deny ACL (add, delete, rename) for Everyone; Unix keeps read-only files and folders, and a change the owner forces anyway invalidates the task (seen on Linux CI) |
| Test implementation | the F-EVAL crates sit inside fermi's own Cargo workspace, so cargo refused to build them; one hidden test file was written in the Windows code page; the first wrong `add` also failed the visible test | each fixture crate declares an empty `[workspace]`; files are UTF-8; the wrong `add` overflows in 32 bits, passing the visible test and failing the hidden one |
| Environment | the first local regression run was stopped by Claude Code because the machine was low on memory | runs use `CARGO_BUILD_JOBS=2`; Linux CI covers the security profile |

## Found while building System 7 (no open defects)

| Kind | Issue | Fix |
| --- | --- | --- |
| Regression, caught by SE-04 on Linux CI before merge | the egress proxy was changed to stop when dropped (so per-call plugin proxies don't leak), but the engine kept only the proxy's port, so agents' proxy stopped at once and sandboxed agents lost their allowed network | the engine keeps the proxy itself for its whole life; SE-04 passes again on Linux |
| Test harness | `Case::block` recorded a case as blocked even when one of its checks had already failed, so a real failure on a platform without the OS sandbox could hide behind "blocked" | a case with a failed check finishes as failed; the PL-07 negative control confirms it |
| Test implementation, caught on Linux CI | the test plugins wrote their marker and log files, and read the adapter's script, outside their sandbox scope; Landlock correctly refused, so PL-01, PL-05 and PL-06 failed on Linux while Windows (no OS boundary) passed | the test plugins' manifests grant one probe folder with `read_write`; the PL-07 sentinel stays outside every grant |

## Found while building System 8 (no open defects)

| Kind | Issue | Fix |
| --- | --- | --- |
| Product gap, caught by OB-04 before release | redaction's `password=…` pattern needed a word boundary before the name, so `DATABASE_PASSWORD=…` (an underscore is a word character) kept its value | names with prefixes (`DATABASE_PASSWORD`, `aws_access_key`, `GITHUB_TOKEN`) are matched; the unit test covers it |
| Design gap, caught in review before commit | events were redacted after the adapters built them, so a one-line preview cut at 120 characters could keep the start of a secret too short to match | each CLI line is redacted as it is read, before any parsing; events are redacted again before storage |
| Documentation | five characters in the README's layout block were mojibake (`â†’` for `→`) since `0b14e07` | repaired |
| Test implementation | AI-01/AI-02 compared every event kind across adapters, including the new per-call usage events, which each CLI reports at its own granularity (Claude per model call, Codex per turn, Gemini per run) | the lifecycle comparison leaves out `call`, as it already left out streamed chunks |
| Test implementation | the first OB-07 expected `[REDACTED]` in every file under the tapes folder, including saved task files that hold no secret | the check looks at the recordings (`*.jsonl`) |

## Design gap caught before release

While adding the merge gate, the first version told the user to fix failing checks "in a task's conversation", but a task branch already merged into the integration branch is never merged again, so such a fix could not reach what gets merged. Fixed before commit: failing checks on the integrated branch now start a debugger step in the integration worktree (bounded by `max_repairs`) and the checks run again; MA-08 covers it.

## Environment notes

- Release and full-workspace builds on the development machine were killed for low memory; use `CARGO_BUILD_JOBS=4` (2 for release builds).
