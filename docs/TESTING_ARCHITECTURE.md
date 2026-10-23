# Testing architecture (proposed for Phase 2)

Status: **proposal**. Nothing below exists yet except the registry (`tests/acceptance/systems.yaml`) and the audit documents. It applies the acceptance plan (`FERMI_ADE_Verification_and_Acceptance_Testing`, written for "APEX", which is fermi) to fermi's actual stack.

## Stack being tested

| Part | Where | Notes |
| --- | --- | --- |
| Engine, router, providers, hub, teams, code index, deck writer | `crates/fermi-core` | Rust, tokio. Providers drive the official CLIs (`claude`, `codex`, `gemini`) as subprocesses and call HTTP APIs. |
| CLI, TUI, dashboard server | `crates/fermi` | axum server on 127.0.0.1 with token auth; single-file web UI in `web/index.html`. |
| Desktop shell | `crates/fermi-desktop` | Tauri window around the same server. |
| State | `~/.fermi` (`FERMI_HOME`) | config.toml, history/*.json, teams/, index/, state.json, usage.jsonl. |

## Harness boundaries

- **Every case gets its own `FERMI_HOME`**, workspace copy, ports and run/case/attempt IDs. Nothing touches the developer's real `~/.fermi`.
- **Deterministic providers without mocking the engine.** fermi talks to providers across process and HTTP boundaries, so the simulators sit outside the engine:
  - *Fake CLIs*: a test binary (`acceptance-tests` bin target) that speaks the same stream formats as `claude -p --output-format stream-json`, `codex exec --json` and the Gemini CLI. A scenario file chooses what it emits: text, tool calls, usage, permission denials, limit and auth errors, malformed lines, stalls and crashes, and optional scripted file edits in its working folder. Tests point fermi at it through the existing `[bins]` config, so spawning, stream parsing, failover and history all run for real.
  - *Mock HTTP provider*: an axum server for the API adapters (via `base_url`) with scripted 429 + Retry-After, 500/503, timeouts, interrupted bodies, latency and usage fields.
  - Both record every request they receive as evidence.
- **Real boundaries for T3**: real processes, git worktrees and files in disposable temp folders; sentinel and canary files; process inventory before and after.
- **The candidate never writes the oracle**: fixtures and ground truth are read from the repository's `tests/fixtures`, copied into the case workspace, and hashed before and after.

## Fixtures

| Fixture | Plan |
| --- | --- |
| F-CODE | 3-crate Rust workspace with duplicate symbol names in separate modules, cross-crate imports, a shared utility with known callers and tests; `ground-truth/{symbols,references,dependency-graph}.json`; expected diffs. |
| F-PROVIDER | Scenario files for the fake CLIs and the HTTP mock; versioned price table. |
| F-WORKER | Scripted long-running agents with barriers (files the fake CLI waits on) and child processes. |
| F-SECURITY | Sentinel files outside allowed roots, synthetic secrets, canary strings. |
| F-WEB, F-MEMORY, F-PLUGIN, F-EVAL | Added with the systems that need them (all missing today). |

## Layout

```text
crates/acceptance-tests/        # workspace member, tests only
  src/lib.rs                    # harness: fermi home, fixtures, ports, processes, evidence, canary scan
  src/bin/fake-cli.rs           # deterministic Claude/Codex/Gemini CLI stand-in
  tests/model_providers.rs      # AI-*
  tests/agent_execution.rs      # MA-*
  tests/code_intelligence.rs    # CI-*
  tests/observability.rs        # OB-*
  tests/security.rs             # SE-*
  tests/execution.rs            # LR-*
  ...
xtask/                          # acceptance runner
tests/acceptance/systems.yaml   # registry of all 106 IDs (exists)
tests/fixtures/                 # F-CODE, F-PROVIDER, ...
tests/reports/<runId>/<testId>/<attemptId>/   # result.json, events.jsonl, logs, diff.patch, manifest.json
ui-tests/                       # Playwright specs for the dashboard (T2)
```

## Runner

`cargo run --locked -p xtask -- acceptance --id MA-04 | --system MA | --all [--profile security|live --allow-billable]` and `-- report --run-id <id>`.

- Reads `systems.yaml`, rejects unknown, duplicate or unmapped IDs, runs each mapped test with `cargo test -p acceptance-tests --test <file> <name> -- --exact`.
- Tests write their own result record and artifacts; the runner validates schema, artifact existence and SHA-256 hashes before counting a pass.
- A case without a test is `not_implemented`; missing credentials or Docker are `blocked`; nothing missing ever counts as passed. Exit code is nonzero if any selected required case is not passed.
- Concurrency cases repeat 20 times; every attempt is kept.
- `report` regenerates `TEST_RESULTS.md`, `ACCEPTANCE_MATRIX.md` statuses and a per-system dashboard.

## Profiles

| Profile | Tiers | What runs |
| --- | --- | --- |
| deterministic (default) | T0, T1, local T2 | unit logic, engine with fake CLIs and HTTP mock, dashboard with Playwright |
| security | T3 | real processes and worktrees, kill and cancel, sentinel and canary cases; container cases need Docker |
| live | T4 | opt-in real provider smoke tests; requires `--allow-billable` |

## Environment notes

- Development machine is Windows 11. Container cases (LR-02, parts of T3) are `blocked` until Docker is available.
- Coverage needs cargo-llvm-cov (defect D-002).
- CI on the private GitHub repository uses metered Actions minutes; the PR gate should stay on the deterministic profile.

## Proposed order

1. Fix the baseline gates: rustfmt (D-001), pinned toolchain and CI (D-003), coverage tooling (D-002).
2. Build the harness, fake CLI, HTTP mock, F-CODE and F-PROVIDER, and the runner; prove one positive and one negative control.
3. Write acceptance tests for the 40 implemented or partial capabilities first (systems 1, 2, 3, 8, 10, 11 and the related E2E cases), record real outcomes and defects.
4. Then build the missing systems in an agreed priority order, each with its eight cases.
