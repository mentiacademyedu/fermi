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

## Found while closing MA-04 (no open defects)

| Kind | Issue | Fix |
| --- | --- | --- |
| Design gap, the last *partial* capability | a merge conflict in a team run went straight to a debugger agent; nothing showed the user both changes or checked that the agent kept both, so a resolution could quietly favour one side | the run stops with a conflict report (the files, each side's patch, the integration worktree mid-merge); the user resolves it and continues (refused while files are unmerged or carry conflict markers) or hands it to an agent; `conflicts = "agent"` keeps automatic resolution for unattended runs |
| Test implementation, caught by the negative control | MA-04 first created its run with `conflicts = "ask"` spelled out, so it passed even with automatic resolution as the default; and when the control removed the escalation, a later step panicked before the evidence was written | run 1 uses the default a user gets; the resolution step runs only when a conflict waits, so a failure keeps its evidence |

## Found by the live tier (no open defects)

| Kind | Issue | Fix |
| --- | --- | --- |
| Design gap, caught by the first live run of E2E-09 | a real agent built the code in its evaluation folder, and the patch fermi keeps as evidence took in Cargo's whole `target/` folder (60,380 lines for one task); scoring already skipped `target`, so scores were right, but the evidence was buried and large | the evaluation folder's local git exclude leaves out `target/` and the build junk team runs already ignore; E2E-09 now checks the patch in both its forms, and the negative control (the exclusion removed) fails it; the live patches are 22 lines |
| Test harness | an account without `home` lives under the user's fermi home (`~/.fermi/accounts/<id>`); copied as is into a case's config it would have resolved there only because the test process shares `FERMI_HOME` | `Fermi::live` writes the folder the account really uses |

## Found while building the end-to-end workflows (no open defects)

| Kind | Issue | Fix |
| --- | --- | --- |
| Product defect, caught by E2E-05 | a call cancelled mid-run (its future dropped) never gave its account back: the account stayed counted as busy, which the dashboard showed and routing used to spread work, until fermi restarted | the slot is a guard released when dropped, however the call ends; the negative control (the old begin/end pair) leaves the slot at 1 and fails E2E-05 |
| Product defect, caught by E2E-06 | the dashboard refused a new task for a profile whose accounts are on workers ("no account that can run any of its models"): it checked local routing first, which the CLI and the hub skip for worker tasks | local routing is checked only when the task stays on this machine; the negative control fails E2E-06 with the old refusal |
| Design gap, caught by E2E-04 | after a provider failed partway through a call that had already run tools, the next account started the task over without knowing what was done, so edits and side effects (a deploy hook, say) could run twice | the adapters report the tools each call ran; on failover the work stays and the next account is told which calls ran and what the folder holds; the negative control repeats the hook call and fails E2E-04 |
| Design gap, caught by E2E-02 | tasks started together on one repository all worked in the same folder (only team runs had worktrees) | a task can be isolated: its own worktree and branch from the repository's commit, its diff available for review, removed with the task; the negative control (isolation ignored) fails E2E-02 |
| Design gap, caught by E2E-01 and E2E-07 | the exported trace had tool and model operations only, with nothing linking them to the request, the route, the checks and the tested revision; plugin log entries didn't name the task that made the call | the trace has a request-to-outcome timeline and the tested revision; plugin log entries carry the task id; negative controls fail E2E-01 and E2E-07 |
| Test harness | the fake CLI played its default scenario step for the dashboard's `auth status` login check, so a step's side effects ran once more per account (E2E-04 first counted three hook calls), and one run wrote a scripted edit into the test crate's own folder | the fake answers the login check as Claude Code does and plays no step; the stray file was removed |
| Test harness race, caught by the coverage run | under coverage instrumentation LR-05 saw events during its outage: the proxy's cut refused new connections but kept forwarding on one already open, so a follow request in flight at the cut came back with the job's later events (the published runs and Linux CI had passed) | a cut also stops forwarding on open connections, as a real outage would; LR-05 passed 5 of 5 repetitions and the coverage run afterwards |
| Test implementation | E2E-01 first required the model call before its tools in the timeline, but a call is recorded when it closes; E2E-10 first matched names too literally (the symbol view says `process at engine/src/lib.rs`; the test `normalize_trims` is another identity and keeps its name) | the edit is linked to its call by id; E2E-10 checks references, not substrings |

## Found while building the System 1–3 gaps (no open defects)

| Kind | Issue | Fix |
| --- | --- | --- |
| Test environment, caught on Linux CI | BA-01 failed once on the branch: Chrome's cold start on a busy runner took longer than the 20 s fermi waited for its debugging port (the rerun passed) | fermi waits up to 60 s and reports at once a browser that exits while starting |
| Test implementation | MA-05 first expected the child's input to start with its instructions, but the memory brief comes first (as for every task), and expected memory on every delegation although the second subtask matches none | the assertions check what the child received, wherever it is, and memory where it applies |
| Test implementation | CI-06 first read `stale` and `diagnostic` at the top of the symbol result; they are per symbol | the test reads them per symbol |

## Found while building System 11 (no open defects)

| Kind | Issue | Fix |
| --- | --- | --- |
| Product defect, caught by LR-07 before release | cancelling a job on a worker stopped the job's own task, but the engine run it had started kept going: the agent's CLI and the processes under it lived on, and the job's folder couldn't be freed | the job's task owns its run and aborts it when dropped; the negative control (the guard removed) fails LR-07 |
| Test harness, caught on Linux CI | LR-03 saved the worker's artifact manifest as evidence under the name `manifest.json`, overwriting the case's own evidence manifest, so the runner saw "an artifact changed after the test" (LR-03 is blocked on Windows, so only CI ran it) | the evidence is `worker-manifest.json`; `Case::text` now refuses the reserved names `manifest.json` and `result.json` |
| Test implementation | the outage proxy counted submissions only on the first request of each connection, but the HTTP client reuses connections | it counts every request line |

## Found while building System 6 (no open defects)

| Kind | Issue | Fix |
| --- | --- | --- |
| Test flake, caught by CI's coverage step | OB-04 read a task's saved history right after the run finished in memory; the hub writes the final record a moment later, and under coverage instrumentation the read came first | `wait_saved` waits for the history file to show the task finished; it replaces the fixed 300 ms pauses in the OB cases |
| Test implementation | BA-01's unreachable URL used port 9, which Chromium refuses as an unsafe port instead of trying; BA-08 first measured readiness from the click's trace entry, which is written after the click's short wait for a navigation | a freshly released ordinary port gives a real connection refusal; BA-08 checks the recorded state changes and the order the server saw the requests |

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
