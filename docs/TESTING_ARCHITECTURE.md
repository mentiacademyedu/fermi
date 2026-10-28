# Testing architecture

Status: **implemented** for the deterministic profile (T0–T2 without a browser) and the process-level security cases. It applies the acceptance plan (`FERMI_ADE_Verification_and_Acceptance_Testing`, written for "APEX", which is fermi) to fermi's stack. Parts marked *not built yet* are listed at the end.

## Stack being tested

| Part | Where | Notes |
| --- | --- | --- |
| Engine, router, providers, hub, teams, code index, deck writer | `crates/fermi-core` | Rust, tokio. Providers drive the official CLIs (`claude`, `codex`, `gemini`) as subprocesses and call HTTP APIs. |
| CLI, TUI, dashboard server | `crates/fermi` | axum server on 127.0.0.1 with token auth; single-file web UI in `web/index.html`. |
| Desktop shell | `crates/fermi-desktop` | Tauri window around the same server. |
| State | `~/.fermi` | config.toml, history/*.json, teams/, index/, state.json, usage.jsonl. |

## Harness boundaries

- **Every case gets its own fermi home** (`target/acceptance/<run>/<case>/<attempt>/home`), workspace copy and IDs. The engine is built from a generated `config.toml`; nothing touches the developer's real `~/.fermi` or real provider accounts.
- **Deterministic providers without mocking the engine.** `crates/acceptance-tests/src/bin/fake-cli.rs` stands in for all three CLIs (and emulates Claude's `--disallowedTools "Read(//…)"` enforcement for scripted reads). Tests point fermi at it through the normal `[bins]` config, so process spawning, stream parsing, failover, approvals, history and team scheduling all run for real. It tells the CLIs apart by their arguments, emits each one's stream format, and plays the steps of a `scenario.json` placed in the account's config folder: text, tool calls, usage, file edits in the working folder, permission denials, provider errors, usage limits, barrier files, delays, child processes, crashes and exit codes; and model calls with message ids and per-call usage, each tool with its id, result, failure, exit code or error type and duration, in each CLI's own format (Claude streams one line per content block sharing the message id, as Claude Code does), plus a `hold` that keeps a run busy until it is interrupted, and `ops` that act for real (files, HTTP requests, calls to fermi's MCP bridge, which the Claude flavor streams as tool calls carrying the bridge's real answers). It answers the dashboard's `auth status` login check as Claude Code does, without playing a step. Every call is appended to `calls.jsonl` as evidence of what fermi sent (prompt, arguments, working folder).
- **Real boundaries where the case is about them**: real git repositories and worktrees, real `cargo test` on the fixture, real process trees for crash and cancel.
- **The candidate never writes the oracle**: fixtures and ground truth are read from `tests/fixtures`, copied into the case workspace, committed with a fixed identity and dates (`core.autocrlf=false`), and the source tree is hashed before and after.

## Fixtures

| Fixture | State |
| --- | --- |
| F-CODE | built: 3-crate Rust workspace (`util`, `engine`, `app`) with duplicate `helper` functions in separate modules, a call cycle (`ping`/`pong`), cross-crate imports, a test, and sentinel strings in a comment and a string literal. Ground truth: `manifest.json`, `symbols.json`, `references.json`, `dependency-graph.json`, `expected/engine-lib-greet.rs`. |
| F-CODE (verification) | `repo/.fermi/checks.toml` pins the checks (offline `cargo check` and `cargo test`, `max_repairs = 3`); `repo/Cargo.lock` and `.gitignore` keep runs reproducible. `advisories/` is a pinned two-record OSV dataset; `expected/vulnerable-Cargo.lock` pins an affected `smallvec`; `expected/util-text-helper-edit.rs` is the oracle for the structured edit. |
| F-PROVIDER | `cost-cases.json`: a pinned price table and usage with costs worked out by hand (the CO-01 oracle, including zero tokens and rounding boundaries). Price tables, budgets and models for the other cost cases are declared in each test's config. `src/provider.rs` is a mock model endpoint (Anthropic Messages, OpenAI Responses, OpenAI-compatible chat completions) that answers scripted replies in order, including 429s with Retry-After, and logs every request (path, credentials sent, body, and the time on the engine's manual clock); it stands in for the hosted APIs and for a local model server. An outbound recorder (an HTTP proxy that lets nothing through) shows whether a process tried to reach anything else. |
| F-MEMORY | `records.json`: ten decisions with sources, six paraphrase queries with the expected record, two contradictory notes. The two project scopes are separate F-CODE copies; the private record, planted comment, secret file and canaries are created by each test. |
| F-SECURITY | built inline: canary files outside the task root, a synthetic controller secret, disposable sentinels, two task scopes, and real local endpoints (allowed and forbidden) for the network cases. |
| F-WORKER | built inline in the tests as fake-CLI scenarios (outages, limits, crashes, barriers, children, scripted repairs, scripted usage including cached tokens); for the execution cases, `src/workers.rs` starts `fermi worker` as its own process (the `fermi-worker` test binary) or in a Docker container (`ubuntu:24.04`, Linux CI), and a TCP proxy that refuses connections or loses answers. |
| F-PLUGIN | `tests/fixtures/F-PLUGIN/`: eight sample plugins (manifests whose program is the harness's `fake-plugin`, an MCP server over stdio that can speak either protocol era, crash, hang, report tool errors, read files and play scripted adapter replies). |
| F-EVAL | `tests/fixtures/F-EVAL/smoke`: dataset version 1, five small Rust crates with stubs, protected visible tests and hidden tests (score 5 each); `solutions/` holds the correct and the known-wrong implementations the fake candidate applies. |
| F-WEB | `tests/fixtures/F-WEB/site`: eight small pages (home, a three-page tour, a form, an error page, a failing-data page, a responsive page and a delayed modal), served with a small API by `acceptance_tests::web_fixture` (`src/web.rs`) on a free port; the server keeps what it receives and a log of every request. |

## Layout

```text
crates/acceptance-tests/
  src/lib.rs                    # Case (evidence), Fermi (engine with fake accounts), fixtures, git, process helpers
  src/bin/fake-cli.rs           # deterministic Claude/Codex/Gemini CLI stand-in
  src/bin/fake-plugin.rs        # deterministic plugin / MCP server
  src/bin/fermi-run.rs          # one task through the engine in its own process (AI-08)
  src/provider.rs               # F-PROVIDER mock endpoints and the outbound recorder
  tests/model_providers.rs      # AI-01 … AI-08 (AI-07 is in cost.rs)
  tests/agent_execution.rs      # MA-01 … MA-08
  tests/verification.rs         # VE-01 … VE-08
  tests/cost.rs                 # CO-01 … CO-08, AI-07, E2E-08
  tests/memory.rs               # PM-01 … PM-08
  tests/security.rs             # SE-01 … SE-08
  tests/code_intelligence.rs    # CI-01 … CI-08
  tests/observability.rs        # OB-01 … OB-08
  tests/plugins.rs              # PL-01 … PL-08
  tests/evaluation.rs           # EV-01 … EV-08
  tests/browser.rs              # BA-01 … BA-08 (a headless Chromium-based browser; blocked where none is installed)
  src/web.rs                    # the F-WEB fixture server
  src/workers.rs                # worker processes, Docker workers and an outage proxy (LR)
  tests/execution.rs            # LR-01 … LR-08 (LR-02/03 need Docker; blocked where it is missing)
  tests/end_to_end.rs           # E2E-01 … E2E-07, E2E-09, E2E-10 (E2E-08 is in cost.rs), through the dashboard's HTTP API
  tests/dashboard.rs            # UI-01 … UI-05: the web UI in a headless browser (not catalog cases)
xtask/src/main.rs               # acceptance runner
tests/acceptance/systems.yaml   # registry of all 106 IDs, with test mappings and published status
tests/acceptance/phase1_audit.py  # writes ACCEPTANCE_MATRIX.md (and the registry with --registry)
tests/fixtures/F-CODE/          # fixture and ground truth
tests/reports/<run>/<case>/<attempt>/   # result.json, manifest.json and artifacts (git-ignored)
docs/acceptance-runs/<run>/     # published runs: summary.json and REPORT.md
```

## Evidence

Each test calls `Case::start(id, tier)`, declares which components are real and which are mocked, records named checks with details, writes artifacts (JSON, patches, logs), and `finish()` writes:

- `result.json`: test ID, run, attempt, tier, start and end times, revision, OS, real and mocked components, every check, artifacts, status `passed` only if every check passed.
- `manifest.json`: SHA-256 of each artifact.

A failing check makes the test panic after the evidence is written, so `cargo test` and the evidence agree.

## Runner

```text
cargo run --locked -p xtask -- acceptance [--id MA-04]… [--system MA]… [--implemented | --all]
                                          [--profile deterministic|security|live] [--allow-billable] [--live-accounts a,b] [--repeat N]
cargo run --locked -p xtask -- report --run-id <id>
```

- Reads `systems.yaml`, rejects unknown IDs, and selects cases by ID, system and profile. `--implemented` selects every case with a test.
- Builds the tests once, then runs each case in its own process: `cargo test -p acceptance-tests --test <file> <name> -- --exact`, with `ACCEPTANCE_RUN_ID`, `ACCEPTANCE_ATTEMPT` and `ACCEPTANCE_PROFILE` set (and `ACCEPTANCE_LIVE_ACCOUNTS` from `--live-accounts`).
- A pass counts only if the process succeeded **and** `result.json` exists, names the right case and attempt, says passed, and every artifact matches its manifest hash.
- A case without a test is recorded as `not_implemented`; nothing missing counts as passed. Cases with `repetitions` (MA-01: 20) run that many attempts and pass only if all do; every attempt is kept.
- Writes `summary.json` (run, profile, revision, dirty flag, OS, every case with its attempts) and `REPORT.md` (overall *passed / 106*, per-system table). Exit code 0 only if every selected case passed, 1 otherwise, 2 for usage or build errors.
- The live profile refuses to start without `--allow-billable`. Its model cases (EV-02, EV-04, E2E-09) run on the accounts given with `--live-accounts`, copied from the user's own fermi config by `Fermi::live` (the real CLI, in the account's own folder; history, usage and tapes stay in the case's folder); without accounts they report blocked, never passed on fake models.

Controls: with the D-004 fix reverted AI-05 failed and the runner exited 1; with it restored AI-05 passed and the runner exited 0.

## Profiles

| Profile | Tiers | What runs |
| --- | --- | --- |
| deterministic (default) | T0, T1, local T2 | engine, hub, teams, code index and verification with the fake CLI and fixtures; real cargo, clippy and git; the dashboard API over HTTP (VE-08) |
| security | T3 | real process trees: crash isolation (MA-06) and cancellation (MA-07); sandbox and secret cases come with System 10 |
| live | T4 | the worker cases (LR-03 … LR-07, E2E-06) and the model comparisons (EV-02, EV-04, E2E-09) on real models of the given accounts; requires `--allow-billable`, and `--live-accounts` for the model cases |

## CI

`.github/workflows/ci.yml` runs on pushes to `main` and on pull requests (Ubuntu): formatting, clippy with warnings as errors, unit tests, the acceptance runner with `--implemented`, and coverage with cargo-llvm-cov; the dashboard tests; `tests/reports/` and `lcov.info` are uploaded as artifacts, kept 30 days for a green run and 90 for a failed one. The toolchain is pinned by `rust-toolchain.toml`. The plan's nightly run (repeated concurrency, resilience and security profiles) isn't scheduled: on this private repository it would spend the account's Actions minutes every day, which is the owner's decision.

## Dashboard tests

`tests/dashboard.rs` loads the web UI (`crates/fermi/web/index.html`) in a headless Chromium-based browser (fermi's own DevTools-protocol driver, `fermi_core::browser`, as System 6 uses; Edge on Windows, Chrome on Linux CI) against the real dashboard server, and clicks through it as a user does. Only the model's output comes from the fake CLI. They aren't catalog cases, so the runner's registry doesn't list them; each still writes `result.json`, a manifest and evidence (browser trace, screenshots, what the window showed) under `tests/reports/<run>/UI-xx/`, and every case fails on any page error or uncaught exception other than a refusal it causes on purpose.

| Case | What the window does |
| --- | --- |
| UI-01 | opens with its key, connects and lists the workspace; the key leaves the address bar; without the key the page says how to open it and the API refuses |
| UI-02 | a task typed in the composer with **Isolated** ticked: the agent gets the prompt, the outcome shows, the change is on the task's own branch and the user's folder is untouched; Trace → Changes shows the patch |
| UI-03 | Cancel on a running task: the task is cancelled, the conversation says so, the button goes |
| UI-04 | Teams → New team run → Plan it → Approve and start; the merge conflict shows both patches and both choices; continuing before resolving is refused with the reason; after the user resolves it, "Checks passed: ready to merge" and the integrated file is the user's resolution, with no agent asked |
| UI-05 | at phone width (375 px, touch) the page has no horizontal overflow and the prompt and Send are on screen and uncovered |

```text
cargo test -p acceptance-tests --locked --test dashboard -- --test-threads=1
```

## Not built yet

- **Desktop shell tests**: the Tauri window around the same page has no automated tests.
- **The rest of the web UI**: settings, accounts and profiles, files and editor, slides, code map, memory, plugins, evaluations, skills, import, trending and terminals are checked by hand only.

## Next

Build the missing systems in an agreed order, each with its eight cases. Done: project memory (System 4), the verification engine (System 5), the plugin system (System 7), agent observability (System 8), cost and performance (System 9, with AI-07 and E2E-08), security and isolation (System 10), evaluation (System 12), browser control (System 6), local and remote execution (System 11) and the remaining cases of Systems 1 to 3 (AI-04/06/08, MA-05, CI-04/06), so every system is at its gate. The end-to-end workflows followed (E2E-01 … E2E-07, E2E-09, E2E-10, beside E2E-08), each driven through the dashboard's HTTP API as a window drives fermi. The OS sandbox (PL-07, SE-01/04/05, and the last step of E2E-07) is Linux-only and the container cases (LR-02/03) need Docker, so Linux CI runs the security profile for SE, MA, PL, EV, LR and E2E and the live profile for LR and E2E; on Windows those seven report blocked.
