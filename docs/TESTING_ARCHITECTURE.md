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
- **Deterministic providers without mocking the engine.** `crates/acceptance-tests/src/bin/fake-cli.rs` stands in for all three CLIs (and emulates Claude's `--disallowedTools "Read(//…)"` enforcement for scripted reads). Tests point fermi at it through the normal `[bins]` config, so process spawning, stream parsing, failover, approvals, history and team scheduling all run for real. It tells the CLIs apart by their arguments, emits each one's stream format, and plays the steps of a `scenario.json` placed in the account's config folder: text, tool calls, usage, file edits in the working folder, permission denials, provider errors, usage limits, barrier files, delays, child processes, crashes and exit codes; and model calls with message ids and per-call usage, each tool with its id, result, failure, exit code or error type and duration, in each CLI's own format (Claude streams one line per content block sharing the message id, as Claude Code does), plus a `hold` that keeps a run busy until it is interrupted. Every call is appended to `calls.jsonl` as evidence of what fermi sent (prompt, arguments, working folder).
- **Real boundaries where the case is about them**: real git repositories and worktrees, real `cargo test` on the fixture, real process trees for crash and cancel.
- **The candidate never writes the oracle**: fixtures and ground truth are read from `tests/fixtures`, copied into the case workspace, committed with a fixed identity and dates (`core.autocrlf=false`), and the source tree is hashed before and after.

## Fixtures

| Fixture | State |
| --- | --- |
| F-CODE | built: 3-crate Rust workspace (`util`, `engine`, `app`) with duplicate `helper` functions in separate modules, a call cycle (`ping`/`pong`), cross-crate imports, a test, and sentinel strings in a comment and a string literal. Ground truth: `manifest.json`, `symbols.json`, `references.json`, `dependency-graph.json`, `expected/engine-lib-greet.rs`. |
| F-CODE (verification) | `repo/.fermi/checks.toml` pins the checks (offline `cargo check` and `cargo test`, `max_repairs = 3`); `repo/Cargo.lock` and `.gitignore` keep runs reproducible. `advisories/` is a pinned two-record OSV dataset; `expected/vulnerable-Cargo.lock` pins an affected `smallvec`; `expected/util-text-helper-edit.rs` is the oracle for the structured edit. |
| F-PROVIDER | `cost-cases.json`: a pinned price table and usage with costs worked out by hand (the CO-01 oracle, including zero tokens and rounding boundaries). Price tables, budgets and models for the other cost cases are declared in each test's config. |
| F-MEMORY | `records.json`: ten decisions with sources, six paraphrase queries with the expected record, two contradictory notes. The two project scopes are separate F-CODE copies; the private record, planted comment, secret file and canaries are created by each test. |
| F-SECURITY | built inline: canary files outside the task root, a synthetic controller secret, disposable sentinels, two task scopes, and real local endpoints (allowed and forbidden) for the network cases. |
| F-WORKER | built inline in the tests as fake-CLI scenarios (outages, limits, crashes, barriers, children, scripted repairs, scripted usage including cached tokens). |
| F-PLUGIN | `tests/fixtures/F-PLUGIN/`: eight sample plugins (manifests whose program is the harness's `fake-plugin`, an MCP server over stdio that can speak either protocol era, crash, hang, report tool errors, read files and play scripted adapter replies). |
| F-EVAL | `tests/fixtures/F-EVAL/smoke`: dataset version 1, five small Rust crates with stubs, protected visible tests and hidden tests (score 5 each); `solutions/` holds the correct and the known-wrong implementations the fake candidate applies. |
| F-WEB | not built yet; it comes with browser control. |

## Layout

```text
crates/acceptance-tests/
  src/lib.rs                    # Case (evidence), Fermi (engine with fake accounts), fixtures, git, process helpers
  src/bin/fake-cli.rs           # deterministic Claude/Codex/Gemini CLI stand-in
  src/bin/fake-plugin.rs        # deterministic plugin / MCP server
  tests/model_providers.rs      # AI-01 02 03 05
  tests/agent_execution.rs      # MA-01 02 03 04 06 07 08
  tests/verification.rs         # VE-01 … VE-08
  tests/cost.rs                 # CO-01 … CO-08, AI-07, E2E-08
  tests/memory.rs               # PM-01 … PM-08
  tests/security.rs             # SE-01 … SE-08
  tests/code_intelligence.rs    # CI-01 02 03 05 07 08
  tests/observability.rs        # OB-01 … OB-08
  tests/plugins.rs              # PL-01 … PL-08
  tests/evaluation.rs           # EV-01 … EV-08
  tests/execution.rs            # LR-01
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
                                          [--profile deterministic|security|live] [--allow-billable] [--repeat N]
cargo run --locked -p xtask -- report --run-id <id>
```

- Reads `systems.yaml`, rejects unknown IDs, and selects cases by ID, system and profile. `--implemented` selects every case with a test.
- Builds the tests once, then runs each case in its own process: `cargo test -p acceptance-tests --test <file> <name> -- --exact`, with `ACCEPTANCE_RUN_ID` and `ACCEPTANCE_ATTEMPT` set.
- A pass counts only if the process succeeded **and** `result.json` exists, names the right case and attempt, says passed, and every artifact matches its manifest hash.
- A case without a test is recorded as `not_implemented`; nothing missing counts as passed. Cases with `repetitions` (MA-01: 20) run that many attempts and pass only if all do; every attempt is kept.
- Writes `summary.json` (run, profile, revision, dirty flag, OS, every case with its attempts) and `REPORT.md` (overall *passed / 106*, per-system table). Exit code 0 only if every selected case passed, 1 otherwise, 2 for usage or build errors.
- The live profile refuses to start without `--allow-billable`.

Controls: with the D-004 fix reverted AI-05 failed and the runner exited 1; with it restored AI-05 passed and the runner exited 0.

## Profiles

| Profile | Tiers | What runs |
| --- | --- | --- |
| deterministic (default) | T0, T1, local T2 | engine, hub, teams, code index and verification with the fake CLI and fixtures; real cargo, clippy and git; the dashboard API over HTTP (VE-08) |
| security | T3 | real process trees: crash isolation (MA-06) and cancellation (MA-07); sandbox and secret cases come with System 10 |
| live | T4 | opt-in real provider smoke tests; requires `--allow-billable` (no live tests yet) |

## CI

`.github/workflows/ci.yml` runs on pushes to `main` and on pull requests (Ubuntu): formatting, clippy with warnings as errors, unit tests, the acceptance runner with `--implemented`, and coverage with cargo-llvm-cov; `tests/reports/` and `lcov.info` are uploaded as artifacts. The toolchain is pinned by `rust-toolchain.toml`.

## Not built yet

- **HTTP provider mock** for the API adapters (`base_url`): needed for AI-04 (429 backoff), AI-08 and the API side of AI-05.
- **Dashboard tests** (Playwright against `fermi ui`): the web UI has no automated tests.
- **Container and remote workers** (LR-02 onwards): Docker is not available on the development machine, so those cases would be `blocked`.
- Fixtures for systems that don't exist yet (browser control).

## Next

Build the missing systems in an agreed order, each with its eight cases. Done: project memory (System 4), the verification engine (System 5), the plugin system (System 7), agent observability (System 8), cost and performance (System 9, with AI-07 and E2E-08), security and isolation (System 10) and evaluation (System 12). Next suggested: browser and application control (System 6), then local and remote execution (System 11). The OS sandbox (PL-07, SE-01/04/05) is Linux-only, so a Linux CI step runs the security profile for SE, MA, PL and EV; on Windows those four report blocked.
