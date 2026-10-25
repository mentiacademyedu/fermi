# Test results

## Run manifest

| Field | Value |
| --- | --- |
| Run | `2026-10-25-cab8247` (deterministic), `2026-10-25-cab8247-security` and `2026-10-25-cab8247-live-workers`, plus the same three on Linux CI for the same commit |
| Revision | `cab8247` (full SHA in the run summaries); clean working tree |
| Date | 2026-10-24 |
| Environment | Windows 11 Home 10.0.26200, Rust 1.96.0 (`rust-toolchain.toml`), local developer machine |
| Providers | fake CLI (`crates/acceptance-tests/src/bin/fake-cli.rs`) and mock HTTP model endpoints (`src/provider.rs`); no live provider calls, no metered usage |
| Real tools | git, cargo (check, test, clippy) on fixture copies, the dashboard's HTTP API (VE-08) |
| Command | `cargo run -p xtask -- acceptance --implemented --allow-blocked`, the same with `--profile security --allow-blocked`, and `--system SE --system MA --profile security` on Linux CI |
| Evidence | `tests/reports/<run>/<case>/<attempt>/` (result.json, manifest.json with SHA-256, artifacts); summaries and reports published in `docs/acceptance-runs/` |
| CI | GitHub Actions on Ubuntu runs the same gates on every push |

## Gate checks

| Check | Command | Result |
| --- | --- | --- |
| Formatting | `cargo fmt --all -- --check` | passed |
| Static analysis | `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| Unit tests | `cargo test --workspace --locked --lib --bins` | passed: 56 (fermi-core 43, fermi lib 12, fermi bin 1) |
| Acceptance, deterministic | `xtask acceptance --implemented` | 85 passed, 1 blocked (LR-02 needs Docker), runner exit 0 (Linux CI: 86 of 86, with Chrome and Docker) |
| Acceptance, security (Windows) | `xtask acceptance --implemented --profile security --allow-blocked` | 16 passed, 5 blocked (PL-07, SE-01/04/05 need Linux Landlock; LR-02 needs Docker), runner exit 0 |
| Acceptance, security (Linux CI) | `xtask acceptance --system SE --system MA --system PL --system EV --system LR --profile security` on Ubuntu | passed: all 16 (SE-01 … SE-08, MA-06, MA-07, PL-04, PL-07, EV-06, LR-02, LR-06, LR-07) on the real OS sandbox and Docker; 5 of these blocked on Windows (`docs/acceptance-runs/linux-ci-security/`) |
| Acceptance, live (workers only) | `xtask acceptance --system LR --profile live --allow-billable --allow-blocked` (no provider is called) | Windows: 4 passed, LR-03 blocked (Docker); Linux CI: all 5 passed |
| Coverage | `cargo llvm-cov --workspace --locked` | 66.5% of product lines on Windows, 66.6% on Linux CI (see `TEST_COVERAGE.md`) |

## Acceptance outcomes

| Outcome | Cases |
| --- | --- |
| passed | 91 on Windows: the 85 before, plus AI-04, AI-06, AI-08, MA-05, CI-04 and CI-06. PL-07, SE-01, SE-04, SE-05 (Landlock), LR-02 and LR-03 (Docker) pass on Linux CI (they are blocked on Windows), so 97 counting the Linux runs |
| failed | 0 |
| skipped | 0 |
| blocked | 6 on Windows (PL-07, SE-01, SE-04, SE-05: the OS sandbox needs Linux Landlock; LR-02, LR-03: no Docker here); 0 on Linux CI |
| not_implemented | 9 (the end-to-end workflows other than E2E-08: no executable test; the capability is missing or only partly present, see `ACCEPTANCE_MATRIX.md`) |

**Overall acceptance: 97 / 106 passed** (91.5%), counting the Linux CI runs for the six cases that need Landlock or Docker (91 / 106 on the Windows run, with those six blocked). Execution coverage: 97 / 106.

Every pass has a `result.json` whose checks all passed and whose artifacts match their recorded hashes. Each test declares what was real and what was mocked: model output is mocked (fake CLI, or mock HTTP endpoints for the API adapters); the engine, hub, team scheduler, code index, verification service, git, worktrees, compilers, test runners, files and processes are real.

**All twelve systems meet their completion gate (8 / 8).** Systems 1, 2 and 3 joined with this run: System 1 (model-agnostic AI engine — all five adapters, the three CLIs through the fake CLI and the two HTTP APIs against mock endpoints, 429 retries on a manual clock, tool-capability checks and a local OpenAI-compatible endpoint), System 2 (multi-agent orchestration — scoped delegation through the real bridge process) and System 3 (semantic codebase intelligence — scope-safe rename and stale results for files with syntax errors, against the F-CODE ground truth). **The others: System 4 (persistent project memory), System 5 (integrated verification engine), System 7 (composable plugin system — PL-07 on Linux CI with the real OS sandbox, as its gate requires), System 8 (agent observability), System 9 (cost and performance optimization), System 10 (security and isolation — all eight on Linux CI with the real OS sandbox), System 12 (evaluation and self-improvement), and System 6 (browser and application control — real headless Edge on Windows and Chrome on Linux CI; the gate's evidence is fermi's own JSON traces, screenshots and browser logs rather than Playwright's trace format), and System 11 (local and remote execution — worker processes everywhere, and a worker in a Docker container on Linux CI for the container and remote cases).** One end-to-end workflow passes (E2E-08).

### Controls

| Control | Run | Result |
| --- | --- | --- |
| Negative: D-004 fix reverted | `negative-control` | AI-05 failed, runner exit 1 |
| Positive: fix restored | `positive-control` | AI-05 passed, runner exit 0 |
| Negative: merge gate removed (run ready without checks, merge ignores them) | `negative-control-ve08` | VE-08 failed (the API merged the run), runner exit 1 |
| Positive: gate restored | `2026-10-24-d44f68e` | VE-08 passed |
| Negative: budget reservations always admitted | `negative-control-budget` | CO-05, AI-07 and E2E-08 failed, runner exit 1 |
| Positive: reservations restored | `2026-10-24-d44f68e` | all three passed |
| In-test control (SE-06): the same injection without the deny_read policy | within the run | the canary reaches persisted output, so the scan is shown to catch a leak |
| Negative: every project shares one memory store | `negative-control-memory` | PM-08 failed, runner exit 1 |
| Positive: one store per project | `2026-10-24-e72c2c7` | PM-08 passed |
| In-test control (PM-07): the same planted instruction without the deny_read policy | each PM-07 run | the canary reaches persisted history, so the scan is shown to detect a leak |
| Negative: redaction turned off | `negative-control-redaction` | OB-02, OB-04 and OB-07 failed (canaries in history, tapes, exports and the live stream), runner exit 1 |
| Negative: model calls counted per stream line instead of per message id | `negative-control-calls` | OB-01 and OB-03 failed (extra calls, double-counted usage), runner exit 1 |
| Positive: both restored | `2026-10-24-d0302dc` | all eight OB cases passed |
| Negative: plugin argument scope check removed | `negative-control-plugin-scope` | PL-07 failed (the plugin read the sentinel through a path argument; canary in its output), runner exit 1 |
| Negative: a name collision resolved by silent overwrite | `negative-control-collisions` | PL-03 failed (one plugin's `greet` vanished, the bare name resolved), runner exit 1 |
| Positive: both restored | `2026-10-24-f56fd58` | all PL cases passed (PL-07 on Linux CI) |
| Negative: failed and empty runs keep the credit of the checks they pass | `negative-control-eval-credit` | EV-01 failed (the untouched stub and the failed run each earned 1), runner exit 1 |
| Negative: runs of different dataset versions compared | `negative-control-eval-versions` | EV-07 failed, runner exit 1 |
| Negative: the late-save guard removed | `hub::tests::late_save_keeps_a_resumed_attempt` | the unit test failed (the resumed attempt was overwritten) |
| Positive: all restored | `2026-10-24-c3b1f83` | all EV cases and OB-06 passed (EV-06 also on Linux CI, where the owner bypass is invalidated) |
| Negative: clicks no longer wait for the element to be enabled and uncovered | `negative-control-browser-readiness` | BA-08 failed (the premature click on the disabled Confirm was reported as done), runner exit 1 |
| Negative: an HTTP error status no longer fails `browser_open` | `negative-control-browser-status` | BA-01 failed (the 404 page was reported as opened), runner exit 1 |
| Positive: both restored | `2026-10-24-45735d6` | all BA cases passed, on Windows and on Linux CI |
| Negative: cancelling a job no longer stops the run it started | `negative-control-worker-cancel` | LR-07 failed (the child process lived on and the folder stayed), runner exit 1 |
| Negative: reconnecting refetches every event | `negative-control-reconnect` | LR-05 failed (a duplicated tool event), runner exit 1 |
| Positive: both restored | `2026-10-25-82eae66` | all LR cases passed (LR-02 and LR-03 on Linux CI, with Docker) |
| Negative: no attempt ceiling for rate-limited retries | `negative-control-gaps` | AI-04 failed (a fourth call was made and answered; no terminal failure), runner exit 1 |
| Negative: delegated files no longer checked against deny_read | `negative-control-gaps` | MA-05 failed (the deny_read sentinel reached the child agent's input) |
| Negative: a file with a syntax error isn't marked stale | `negative-control-gaps` | CI-06 failed (its last valid symbols were presented as current) |
| Positive: all restored | `2026-10-25-cab8247` | AI-04, MA-05 and CI-06 passed, on Windows and on Linux CI |
| In-test control (OB-08): a tampered recording | each OB-08 run | the replay reports the difference and the state hashes differ |

### Defects

No product defect is open. Product defects found and fixed so far: CLI outages without failover (D-004), orphaned processes after cancel (D-005), four code-index gaps (D-006); plus gate and environment issues D-001, D-002, D-003, D-007. Design gaps in the merge gate, in redaction and in protecting evaluation assets on Windows, an egress-proxy regression, a worker cancellation that left the job running, a history race, and a browser cold start slower than its launch wait on a busy CI runner (a late save overwriting a resumed task) were caught before release. Details in `KNOWN_DEFECTS.md`.

## Readiness verdict

**Not ready.** All 96 system cases pass (counting Linux CI for the six cases that need Landlock or Docker), but only 1 of 10 end-to-end workflows does. The cases that exist pass reliably; what remains is the end-to-end workflows.

## Verified status of the 12 systems

*Mock-only verified* means the case passed with real fermi components and a simulated model; no live provider was used.

| System | Passed | Status | Basis |
| --- | --- | --- | --- |
| 1 Model-agnostic AI engine | 8 / 8 | **verified** (real adapters, engine, routing and config; model output from the fake CLI or mock HTTP endpoints, time from a manual clock) | normalization (five adapters), common interface, model switching, rate-limit retries within a ceiling, outage failover, tool-capability validation, budget exhaustion, a local endpoint with no hosted account |
| 2 Multi-agent orchestration | 8 / 8 | **verified** (real hub, team scheduler, git worktrees, bridge process and permission modes; agents mock-only) | concurrency (20 repetitions), dependencies, independent and conflicting edits, scoped delegation, crash isolation, cancellation, verified integration. MA-04 sends conflicts to a debugger agent rather than the user (audited partial) |
| 3 Semantic codebase intelligence | 8 / 8 | **verified** (real, against the F-CODE ground truth; the renamed fixture still passes cargo test) | indexing, lookup, callers, scope-safe rename, incremental refresh, invalid syntax, impact, identity |
| 4 Persistent project memory | 8 / 8 | **verified** (real store, restarts, HTTP API and agent tools; the agent's behavior and Claude's read-deny enforcement are mocked in PM-07) | durable decisions, fresh-session retrieval, versioning, stale sources, retrieval of paraphrases (lexical v1, not embeddings), contradictions, untrusted content, project isolation |
| 5 Integrated verification engine | 8 / 8 | **verified** (real compilers, test runners, linter and scanner; agent turns mock-only) | diagnostics, test feedback, repair loop, lint, advisories, structured edits, bounded repairs, completion gate |
| 6 Browser and application control | 8 / 8 | **verified** (real headless browsers over the DevTools protocol, the F-WEB site over HTTP, the MCP bridge in and out of process; what the agent decides to call is scripted by the test) | local site launch, form submission, navigation chain, screenshot artifact, console error capture, network failure detection, mobile layout assertions, dynamic readiness |
| 7 Composable harness and plugin system | 8 / 8 on Linux CI (7 / 8 on Windows) | **verified** (real plugin manager, MCP client in both protocol eras, sandboxed plugin processes and Landlock on Linux; the plugin programs and model replies are test doubles) | plugin registration, disabling, command collisions, fault isolation, MCP interoperability, adapter substitution, boundary enforcement, API incompatibility |
| 8 Agent observability | 8 / 8 | **verified** (real adapters parsing each CLI's stream format, hub, history, redaction, tapes and replay; model output mock-only) | complete operation trace, invocation detail, usage fidelity, failure diagnostics, trace persistence, resume correlation, secret redaction, deterministic replay |
| 9 Cost and performance optimization | 8 / 8 | **verified** (exact arithmetic against a hand-worked oracle; ledger, routing and attribution with mock usage) | price tables and cost, cache discounts, economy and quality routing, shared reservations, versioned prices, context warnings, usage attribution |
| 10 Security and isolation | 8 / 8 on Linux CI (5 / 8 on Windows) | **verified** (real OS sandbox on Linux via Landlock; fermi's policy, approvals, environment allowlist and egress proxy on every platform) | forbidden reads, secret isolation, destructive-action gating, network restrictions, cross-task isolation, repository-injection resistance, scoped approvals, alternate-route denial |
| 11 Local and remote execution | 8 / 8 on Linux CI (6 / 8 on Windows) | **verified** (real worker processes, a real worker in a Docker container, authenticated HTTP, process kills; model output from the fake CLI) | local execution, container equivalence, remote dispatch, desktop-independent jobs, connection recovery, worker death, remote cancellation, runtime compatibility |
| 12 Evaluation and self-improvement | 8 / 8 | **verified** (real evaluator scoring with cargo on pristine copies, protected assets enforced by the OS, real dashboard API; the candidate models and the proposing agent are test doubles) | five-task smoke evaluation, cross-model reports, known failure detection, configuration comparison, reproducible setup, protected evaluation assets, dataset history, evaluated improvement proposals |
| End-to-end workflows | 1 / 10 | partly verified | E2E-08 (task exceeding a budget); 6 partial, 3 missing |

## Earlier runs

| Run | Revision | Result |
| --- | --- | --- |
| phase1-baseline | `522e356` | fmt failed (651 differences), clippy passed, 37 unit tests passed, coverage blocked, 0 / 106 acceptance |
| 2026-10-23-0221093 | `0221093` (flagged dirty: documentation edited during the run) | 18 / 18 executed passed; superseded |
| 2026-10-23-114516b | `114516b`, clean | 18 / 18 executed passed; superseded when AI-01 gained the Gemini adapter |
| 2026-10-23-faa705c | `faa705c`, clean | 18 / 18 executed passed (18 / 106); superseded by the verification engine |
| 2026-10-23-19f85c1 | `19f85c1`, clean | 27 / 27 executed passed (27 / 106); superseded by costs and budgets |
| 2026-10-24-d44f68e | `d44f68e`, clean (security run flagged dirty: documentation edited during it) | 37 / 37 executed passed (37 / 106); superseded by project memory |
| 2026-10-24-e72c2c7 | `e72c2c7`, clean | 45 / 45 executed passed (45 / 106); superseded by security and isolation |
| 2026-10-24-02130df | `02130df`, clean | 47 / 47 deterministic, 50 / 106 on Windows, 53 with Linux CI; superseded by agent observability |
| 2026-10-24-d0302dc | `d0302dc`, clean | 54 / 54 deterministic, 57 / 106 on Windows, 60 with Linux CI; superseded by the plugin system |
| 2026-10-24-f56fd58 | `f56fd58`, clean | 61 / 61 deterministic, 64 / 106 on Windows, 68 with Linux CI; superseded by evaluation |
| 2026-10-24-c3b1f83 | `c3b1f83`, clean | 68 / 68 deterministic, 72 / 106 on Windows, 76 with Linux CI; superseded by browser control |
| 2026-10-24-45735d6 | `45735d6`, clean | 76 / 76 deterministic, 80 / 106 on Windows, 84 with Linux CI; superseded by workers |
| 2026-10-25-82eae66 | `82eae66`, clean | 79 passed and 1 blocked deterministic, 85 / 106 on Windows, 91 with Linux CI; superseded by the System 1–3 gaps |
