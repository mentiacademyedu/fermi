# Test coverage

## Requirement coverage

| Measure | Value |
| --- | --- |
| Acceptance cases in the registry | 106 (`tests/acceptance/systems.yaml`) |
| Cases with an executable acceptance test | 106 |
| Execution coverage (cases executed / 106) | 106 / 106 |
| Overall acceptance (cases passed / 106) | 106 / 106 (99 on Windows; PL-07, SE-01/04/05, E2E-07, LR-02 and LR-03 pass on Linux CI, blocked on Windows; Linux CI passes all 106) |
| Capabilities found in the audit | 106 implemented, 0 partial, 0 missing |
| Systems at their completion gate (8 / 8) | all twelve: System 1 (AI engine), System 2 (orchestration), System 3 (code intelligence), System 4 (project memory), System 5 (verification engine), System 6 (browser control), System 7 (plugins; 8/8 on Linux CI), System 8 (observability), System 9 (cost and performance), System 10 (security; 8/8 on Linux CI), System 11 (execution; 8/8 on Linux CI), System 12 (evaluation); and the end-to-end workflows, 10 / 10 (E2E-07 on Linux CI) |

All 106 capabilities are audited as *implemented* and have a passing test (seven of them on Linux CI). MA-04 was the last *partial* one: team-run conflicts went straight to a debugger agent, and now wait for the user. Every case runs on simulated model output in the deterministic and security profiles; EV-02, EV-04 and E2E-09 also ran on real Claude models (Sonnet 5.5 and Haiku 5.5) in the live profile and passed. OpenAI and Gemini have not run live.

## Code coverage

`cargo llvm-cov --workspace --locked` on Windows at `291c237` (the published revision `ee16092` plus a fix to the test harness's outage proxy; unit and acceptance tests together). The Linux-only sandbox code (Landlock) isn't exercised here; Linux CI's `lcov.info` gives 67.2% of product lines for `ee16092` (run 40164598030) and for the live tier's merge `7dc936c` (run 40599642988), 67.3% for `f985976` (run 40607108227), 68.0% for `6822a64` (run 40619945857), 68.0% for `4e46727` (run 40638282321), and 69.0% for `55be539` (run 42095520073; fermi-core 78.9%, fermi 42.1%, `server.rs` 38.6% with the dashboard tests, `team.rs` 78%):

| Scope | Lines | Functions |
| --- | --- | --- |
| fermi-core (…, verification, billing, memory, security, decks, workspaces) | 78.1% (10320 / 13211) | 76.1% |
| fermi (CLI, TUI, server, setup, skills, slides) | 37.4% (1833 / 4905) | 35.7% |
| **Product total** | **67.1% (12153 / 18116)** | **64.3%** |

Excluded from the product total: `crates/acceptance-tests` (the harness), `xtask` (the runner) and `crates/fermi-desktop` (the Tauri shell, no tests). CI produces `lcov.info` on Ubuntu on every push and uploads it as an artifact.

Earlier measurements: 54.1% at `faa705c`, 59.1% at `19f85c1`, 61.9% at `e72c2c7`, 62.9% at `02130df`, 63.9% at `d0302dc`, 64.7% at `f56fd58`, 65.7% at `c3b1f83`, 66.4% at `45735d6`, 65.4% at `82eae66`, 66.5% at `cab8247`. New code is well covered: `workspace.rs` 93%, `trace.rs` 95%, `providers/api.rs` 70% (0% before the mock endpoints; 86% on Linux, where AI-08's separate process is recorded too), `engine.rs` 90%, `clock.rs` 87%, `code.rs` 79%, `remote.rs` 92% (the worker side, `worker.rs` and `worker_http.rs`, runs in separate processes the tests stop by killing them, so its coverage isn't recorded; LR-02 … LR-08 exercise it), `browser.rs` 83%, `sandbox.rs` 95%.

### Notable gaps

| Area | Lines covered | Why |
| --- | --- | --- |
| `fermi/src/server.rs` (dashboard API) | 39% | the task, trace, diff, cancel, state, team, eval, checks, memory, code map and file endpoints the E2E, VE-08, PM and dashboard tests use are exercised, and the page itself with its live connection; accounts, profiles, skills, slides and terminals aren't |
| `fermi/src/main.rs` (CLI commands) | 0% | no CLI tests |
| `providers/image.rs` (image generation) | 0% | the mock endpoints don't serve images |
| `mcp.rs` (bridge tools) | 27% | the bridge runs as its own process in most tests (MA-05, BA, PL), whose coverage isn't recorded; in process, only `run_checks`, the memory and browser tools are called |
| `sessions.rs` (import from other CLIs) | 51% | unit tests cover the parsers, not discovery on disk |

## Frontend coverage

The web page's own script (the inline `<script>` of `crates/fermi/web/index.html`, about 4,000 lines), measured by the browser during the dashboard tests and merged by `xtask dashboard-report` (`frontend-coverage.json`, `frontend.lcov`). At `55be539`, with UI-01 … UI-08:

| Where | Lines | Functions |
| --- | --- | --- |
| Windows (Edge), `2026-11-05-55be539-dashboard` | 1677 / 3976 (42.2%) | 241 / 587 (41.1%) |
| Linux CI (Chrome), run 42095520073 | 1677 / 3976 (42.2%) | 240 / 587 (40.9%) |

With UI-01 … UI-05 alone (`4e46727`) it was 1292 / 3976 lines (32.5%) and 163 / 545 functions (29.9%) on both. The function total moves with what runs: the browser reports a nested function only once the function around it has been compiled, so lines are the figure to compare between runs.

A line counts as run when any of its code ran, a function when it was called; the vendored terminal scripts (xterm.js) are excluded. The eight cases open the window, send tasks, cancel one, run one team run, resize to phone width, and use the code map, memory, files and editor, so the views they never open (settings, accounts and profiles, slides, plugins, evaluations, skills, import, trending and terminals) hold most of what's uncovered; `functionsNotRun` in the report lists them by name and line. No threshold is enforced for either coverage figure.

## Unsupported paths and exclusions

- The web UI has eight dashboard tests (UI-01 … UI-08: opening and the key, a task from the composer with its trace, cancel, a team run with a conflict, phone width, the code map, memory, files and the editor), run in a real browser on every CI run, with the page script's coverage measured (see above). Its other views (settings, accounts and profiles, slides, plugins, evaluations, skills, import, trending and terminals) and the desktop shell have no automated tests.
- Live providers (T4) are never called by the default or CI runs.
- The container cases (LR-02, LR-03) need Docker, which the development machine lacks; they run on Linux CI.
