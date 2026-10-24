# Acceptance matrix

Phase 1 audit of fermi (called APEX in the spec) against `docs/FERMI_ADE_Verification_and_Acceptance_Testing`.

Published run `2026-10-23-19f85c1` on revision `19f85c160489`, profile deterministic, windows. Full report: `docs/acceptance-runs/2026-10-23-19f85c1/REPORT.md`.

*Result* is the published acceptance outcome: `passed` or `failed` from an executed test with validated evidence; `not_implemented` when no test exists (usually because the capability is missing); `not_run` when a test exists but wasn't in the published run.
*Capability* is an audit finding from reading the code; it is not verification.

Capability: **implemented** (the behavior exists end to end), **partial** (some of the expected behavior exists), **missing** (absent).

## Summary

| System | Cases | Capability implemented | Partial | Missing | Acceptance passed |
| --- | --- | --- | --- | --- | --- |
| 1 Model-agnostic AI engine | 8 | 3 | 2 | 3 | 4 / 8 |
| 2 Multi-agent orchestration | 8 | 6 | 1 | 1 | 7 / 8 |
| 3 Semantic codebase intelligence | 8 | 6 | 0 | 2 | 6 / 8 |
| 4 Persistent project memory | 8 | 0 | 0 | 8 | 0 / 8 |
| 5 Integrated verification engine | 8 | 8 | 0 | 0 | 8 / 8 |
| 6 Browser and application control | 8 | 0 | 0 | 8 | 0 / 8 |
| 7 Composable harness and plugin system | 8 | 0 | 2 | 6 | 0 / 8 |
| 8 Agent observability | 8 | 1 | 5 | 2 | 1 / 8 |
| 9 Cost and performance optimization | 8 | 0 | 3 | 5 | 0 / 8 |
| 10 Security and isolation | 8 | 0 | 3 | 5 | 0 / 8 |
| 11 Local and remote execution | 8 | 1 | 1 | 6 | 1 / 8 |
| 12 Evaluation and self-improvement | 8 | 0 | 0 | 8 | 0 / 8 |
| End-to-end workflows | 10 | 0 | 6 | 4 | 0 / 10 |
| **Total** | **106** | **25** | **23** | **58** | **27 / 106** |

## 1 Model-agnostic AI engine

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| AI-01 | Adapter normalization | T1 | **passed** | ai_01_adapter_normalization | partial | crates/fermi-core/src/providers/*.rs, events.rs | Five adapters (Claude/Codex/Gemini CLIs, Anthropic and OpenAI APIs) normalize into one Event stream and RunResult. The acceptance test covers the three CLI adapters through the fake CLI; the two API adapters need an HTTP mock (not built yet), so the case stays partial. |
| AI-02 | Common task interface | T1 | **passed** | ai_02_common_task_interface | implemented | crates/fermi-core/src/engine.rs (Task, Engine::run) | Every provider runs through Task and Engine::run with the same lifecycle and Event schema. Unverified against a fixture oracle. |
| AI-03 | Model switching with state | T1 | **passed** | ai_03_model_switching_with_state | implemented | crates/fermi-core/src/hub.rs (follow_up, continuation) | Model switch per reply; same provider resumes the session, other providers get the transcript; Routed events attribute each turn. |
| AI-04 | Rate limit handling | T1 | **not_implemented** | — | missing | crates/fermi-core/src/providers/api.rs, engine.rs | 429 maps to Limited (Retry-After read) and fails over to the next account; there is no backoff/retry policy or attempt ceiling. |
| AI-05 | Provider outage handling | T1 | **passed** | ai_05_provider_outage_handling | implemented | crates/fermi-core/src/engine.rs (run_inner), providers/mod.rs (classify_error) | Provider outages (HTTP 5xx from the APIs; 5xx, overloaded or unavailable from the CLIs) fail over to the profile's next account, then the next candidate model, recorded as Failover events; pinning the account and model disables it. Fixed 2026-10-23: CLI outages failed without fallback (D-004). |
| AI-06 | Capability validation | T0 T1 | **not_implemented** | — | missing | crates/fermi-core/src/models.rs (Cap) | Capabilities cover code/reasoning/general/fast/image; tool calling is not modeled or validated before dispatch. |
| AI-07 | Budget exhaustion | T0 T1 | **not_implemented** | — | missing | — | No budget or reservation ledger. |
| AI-08 | Local endpoint support | T1 T2 | **not_implemented** | — | partial | crates/fermi-core/src/providers/api.rs (base_url) | API accounts accept base_url (OpenAI-compatible endpoint), untested against a local model server; uses the Responses API, which local servers may not support. |

## 2 Multi-agent orchestration

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| MA-01 | Concurrent workers | T1 | **passed (20/20)** | ma_01_concurrent_workers | implemented | crates/fermi-core/src/hub.rs (submit/launch), team.rs (schedule) | Tasks run concurrently; team runs start up to N agents in parallel, each in its own worktree. |
| MA-02 | Dependency ordering | T0 T1 | **passed** | ma_02_dependency_ordering | implemented | crates/fermi-core/src/team.rs (schedule, topo) | Dependencies gate starts; failed or interrupted tasks block dependents (run goes to attention); cancel stops the run. |
| MA-03 | Independent edits | T1 | **passed** | ma_03_independent_edits | implemented | crates/fermi-core/src/team.rs (start_task, integrate) | Separate worktrees and branches merged with --no-ff into an integration branch; manually verified on a fixture (2 runs). |
| MA-04 | Conflicting edits | T1 | **passed** | ma_04_conflicting_edits | partial | crates/fermi-core/src/team.rs (integrate) | Conflicts are detected (unmerged paths) and logged with both branches kept, but resolution is delegated automatically to a debugger agent rather than escalated to the user. Manually verified once. |
| MA-05 | Scoped delegation | T1 | **not_implemented** | — | missing | — | No delegate tool; agents cannot spawn scoped subtasks. |
| MA-06 | Worker crash isolation | T1 T3 | **passed** | ma_06_worker_crash_isolation | implemented | crates/fermi-core/src/hub.rs, team.rs (on_done) | A killed agent process marks only its task failed; concurrent tasks finish with their results. In team runs a failed task blocks its dependents (MA-02). |
| MA-07 | Parent cancellation | T1 T3 | **passed** | ma_07_parent_cancellation | implemented | crates/fermi-core/src/team.rs (cancel), providers/proc.rs (kill_tree) | Cancel stops running agents and every process they started (process tree), and queued tasks never start. Fixed 2026-10-23: processes started by the CLI were orphaned (D-005). |
| MA-08 | Verified integration | T1 T2 | **passed** | ma_08_verified_integration | implemented | crates/fermi-core/src/team.rs (start_verification, merge) | The integrated branch must pass the project's mandatory checks before merging; a failing run gets bounded debugger fixes and re-checks, and merge refuses anything but the exact commit that passed. Added 2026-10-23. |

## 3 Semantic codebase intelligence

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CI-01 | Repository indexing | T1 | **passed** | ci_01_repository_indexing | implemented | crates/fermi-core/src/code.rs (Index::open/update) | Indexes Rust, Python, JS, TS/TSX, Go with .gitignore; lists unsupported files and the indexed git revision (added 2026-10-23, D-006). |
| CI-02 | Exact symbol lookup | T1 | **passed** | ci_02_exact_symbol_lookup | implemented | crates/fermi-core/src/code.rs (search, symbol) | Exact and prefix lookup with file and line span. |
| CI-03 | Caller discovery | T1 | **passed** | ci_03_caller_discovery | implemented | crates/fermi-core/src/code.rs (graph callers) | Static callers resolved same-file, then imported files; results declare the analysis scope (dynamic dispatch is not covered; added 2026-10-23, D-006). |
| CI-04 | Scope-safe rename | T1 | **not_implemented** | — | missing | — | No semantic rename / edit API. |
| CI-05 | Incremental refresh | T1 | **passed** | ci_05_incremental_refresh | implemented | crates/fermi-core/src/code.rs (update) | Incremental re-parse by mtime and size on every query; no completion event. |
| CI-06 | Invalid syntax handling | T1 | **not_implemented** | — | missing | crates/fermi-core/src/code.rs | tree-sitter parses error-tolerantly; syntax errors are not surfaced and results are not marked stale. |
| CI-07 | Dependency impact | T1 | **passed** | ci_07_dependency_impact | implemented | crates/fermi-core/src/code.rs (impact) | Transitive callers and importers with visited sets (cycles terminate), tests to run, and a declared scope of what isn't covered. |
| CI-08 | Symbol identity | T1 | **passed** | ci_08_symbol_identity | implemented | crates/fermi-core/src/code.rs (find, resolution) | Duplicate names resolve per importing file; lookup by Type::name, module path (util::text::helper, added 2026-10-23) or path:line. |

## 4 Persistent project memory

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PM-01 | Durable decisions | T1 | **not_implemented** | — | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). |
| PM-02 | Fresh-session retrieval | T1 | **not_implemented** | — | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). |
| PM-03 | Decision versioning | T1 | **not_implemented** | — | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). |
| PM-04 | Stale source detection | T1 | **not_implemented** | — | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). |
| PM-05 | Semantic retrieval | T1 | **not_implemented** | — | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). |
| PM-06 | Contradiction handling | T1 | **not_implemented** | — | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). |
| PM-07 | Untrusted memory content | T1 T3 | **not_implemented** | — | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). |
| PM-08 | Project memory isolation | T1 T3 | **not_implemented** | — | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). |

## 5 Integrated verification engine

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| VE-01 | Compiler diagnostics | T1 | **passed** | ve_01_compiler_diagnostics | implemented | crates/fermi-core/src/verify.rs (run, parse_output) | fermi runs the project's checks (.fermi/checks.toml or detected from manifests) and parses diagnostics from cargo JSON and common text formats (rustc, gcc, go, tsc, eslint, ruff, Python) with file, line and column. Added 2026-10-23. |
| VE-02 | Failing test feedback | T1 | **passed** | ve_02_failing_test_feedback | implemented | crates/fermi-core/src/verify.rs, mcp.rs (run_checks) | Agents call the run_checks tool and get each failing test with its output and the exit code; verified tasks record a Verify event. Added 2026-10-23. |
| VE-03 | Repair and rerun | T1 T2 | **passed** | ve_03_repair_and_rerun | implemented | crates/fermi-core/src/verify.rs (run_task) | Verified tasks: after the agent, fermi runs the checks and sends failures back to the same session; protected files (protect globs) must stay unchanged. Added 2026-10-23. |
| VE-04 | Lint enforcement | T1 | **passed** | ve_04_lint_enforcement | implemented | crates/fermi-core/src/verify.rs (Kind::Lint) | Lint checks run like any other; clippy and text-format rule ids and locations are reported and a mandatory lint failure fails the task. Added 2026-10-23. |
| VE-05 | Dependency advisory detection | T1 | **passed** | ve_05_dependency_advisory_detection | implemented | crates/fermi-core/src/verify.rs (audit) | Built-in scanner matches Cargo.lock, package-lock.json and requirements.txt against OSV records; records scanner version and dataset SHA-256; nothing is installed or run. Added 2026-10-23. |
| VE-06 | Structured edit integrity | T1 | **passed** | ve_06_structured_edit_integrity | implemented | crates/fermi-core/src/code.rs (replace_symbol), mcp.rs (code_replace) | Replaces one definition by its syntax-tree byte range, refuses ambiguous targets and edits that don't parse or drop the definition. Added 2026-10-23. |
| VE-07 | Bounded repair attempts | T0 T1 | **passed** | ve_07_bounded_repair_attempts | implemented | crates/fermi-core/src/verify.rs (run_task) | Repair turns are bounded by max_repairs (task option, else the project's, else 3); the final failure is explicit. Added 2026-10-23. |
| VE-08 | Completion enforcement | T1 T2 | **passed** | ve_08_completion_enforcement | implemented | crates/fermi-core/src/verify.rs, team.rs (merge), hub.rs | A verified task ends done only if every mandatory check passed; a team run is mergeable only with passing checks on exactly the commit being merged (service and API). Added 2026-10-23. |

## 6 Browser and application control

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| BA-01 | Local site launch | T2 | **not_implemented** | — | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. |
| BA-02 | Form submission | T2 | **not_implemented** | — | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. |
| BA-03 | Navigation chain | T2 | **not_implemented** | — | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. |
| BA-04 | Screenshot artifact | T2 | **not_implemented** | — | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. |
| BA-05 | Console error capture | T2 | **not_implemented** | — | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. |
| BA-06 | Network failure detection | T2 | **not_implemented** | — | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. |
| BA-07 | Mobile layout assertions | T2 | **not_implemented** | — | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. |
| BA-08 | Dynamic readiness | T2 | **not_implemented** | — | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. |

## 7 Composable harness and plugin system

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PL-01 | Plugin registration | T1 | **not_implemented** | — | missing | — | No plugin manager or manifest format. |
| PL-02 | Plugin disabling | T1 | **not_implemented** | — | missing | — | No plugin manager. |
| PL-03 | Command collisions | T0 T1 | **not_implemented** | — | missing | — | No command registry. |
| PL-04 | Plugin fault isolation | T1 T3 | **not_implemented** | — | missing | — | No plugin host. |
| PL-05 | MCP interoperability | T1 | **not_implemented** | — | partial | crates/fermi-core/src/mcp.rs | fermi is itself an MCP server (bridge tools); connecting fermi to an external MCP server is left to the CLIs' own configs. |
| PL-06 | Adapter substitution | T1 | **not_implemented** | — | partial | crates/fermi-core/src/providers/mod.rs, config.rs ([[models]], bins) | Models and CLI binaries can be overridden in config; no pluggable adapter interface. |
| PL-07 | Plugin boundary enforcement | T3 | **not_implemented** | — | missing | — | No plugin sandbox. |
| PL-08 | API incompatibility | T0 T1 | **not_implemented** | — | missing | — | No plugin API versioning. |

## 8 Agent observability

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| OB-01 | Complete operation trace | T1 | **not_implemented** | — | partial | crates/fermi-core/src/hub.rs (TaskRec.events) | Tool calls and turns are traced; individual model calls inside a CLI run are not visible. |
| OB-02 | Invocation detail | T1 | **not_implemented** | — | partial | crates/fermi-core/src/events.rs (Tool) | Tool name and a one-line input summary only; no outputs, duration or status. |
| OB-03 | Usage fidelity | T0 T1 | **not_implemented** | — | partial | crates/fermi-core/src/state.rs (UsageRecord), providers/claude_cli.rs | Input (incl. cache), output tokens and CLI-reported cost recorded per run; cached tokens are folded into input, unknown fields recorded as 0. |
| OB-04 | Failure diagnostics | T1 | **not_implemented** | — | partial | crates/fermi-core/src/events.rs (Done, Failover, Blocked) | Run failures and blocked approvals recorded; per-tool failures are not. |
| OB-05 | Trace persistence | T1 | **passed** | ob_05_trace_persistence | implemented | crates/fermi-core/src/hub.rs (history files) | Every task's events persist in ~/.fermi/history/<id>.json and survive restarts. |
| OB-06 | Resume correlation | T1 T3 | **not_implemented** | — | partial | crates/fermi-core/src/hub.rs (follow_up) | Follow-ups keep the task id; there is no attempt id or durable checkpoint resume. |
| OB-07 | Secret redaction | T1 T3 | **not_implemented** | — | missing | — | No secret redaction before persistence. |
| OB-08 | Deterministic replay | T1 | **not_implemented** | — | missing | — | No captured-response replay. |

## 9 Cost and performance optimization

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CO-01 | Token cost arithmetic | T0 | **not_implemented** | — | missing | — | Cost comes from the CLI's own total_cost_usd; no price tables. |
| CO-02 | Cached token discounts | T0 | **not_implemented** | — | missing | — | No cached-input pricing. |
| CO-03 | Economy routing | T0 T1 | **not_implemented** | — | partial | crates/fermi-core/src/router.rs | Auto routing sends light work to tier 1 models; no explicit economy policy with costs. |
| CO-04 | Quality routing | T0 T1 | **not_implemented** | — | partial | crates/fermi-core/src/router.rs | Heavy work goes to tier 3; no declared quality ranking policy. |
| CO-05 | Shared budget reservations | T0 T1 | **not_implemented** | — | missing | — | No shared budget reservations. |
| CO-06 | Versioned price updates | T0 T1 | **not_implemented** | — | missing | — | No versioned price tables. |
| CO-07 | Large context warnings | T0 T1 | **not_implemented** | — | missing | — | No context-size estimation or warnings. |
| CO-08 | Usage attribution | T1 | **not_implemented** | — | partial | crates/fermi-core/src/state.rs (usage.jsonl) | Usage logged per run by profile, account and model; not grouped by team run, parent or child task. |

## 10 Security and isolation

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| SE-01 | Forbidden file read | T3 | **not_implemented** | — | missing | — | fermi enforces no file boundary; it relies on each CLI (Codex sandbox, Claude permission modes). |
| SE-02 | Secret isolation | T3 | **not_implemented** | — | missing | — | Child processes inherit the full environment. |
| SE-03 | Destructive action gating | T3 | **not_implemented** | — | partial | crates/fermi-core/src/providers/claude_cli.rs (permission_denials), hub.rs (allow) | Claude in acceptEdits/default modes blocks commands that need approval; fermi surfaces them and continues only after the user allows them. |
| SE-04 | Network restrictions | T3 | **not_implemented** | — | missing | — | No network policy. |
| SE-05 | Cross-task isolation | T3 | **not_implemented** | — | partial | crates/fermi-core/src/team.rs (worktrees) | Team agents get separate worktrees, but no OS-level sandbox or secret separation. |
| SE-06 | Repository injection resistance | T1 T3 | **not_implemented** | — | missing | — | No provenance or untrusted-content policy. |
| SE-07 | Scoped approval audit | T1 T3 | **not_implemented** | — | partial | crates/fermi-core/src/hub.rs (FollowUp.allow), team.rs | Approvals are scoped to tool rules (Bash(node:*)) per conversation and stored; no actor, timestamp or replay protection. |
| SE-08 | Alternate route bypass | T3 | **not_implemented** | — | missing | — | No shared policy across entry points. |

## 11 Local and remote execution

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| LR-01 | Local execution | T1 T2 | **passed** | lr_01_local_execution | implemented | crates/fermi-core/src/engine.rs, hub.rs | Tasks run locally and record changed files and artifacts. |
| LR-02 | Container equivalence | T1 T3 | **not_implemented** | — | missing | — | No container workers. |
| LR-03 | Remote dispatch | T4 | **not_implemented** | — | missing | — | No remote workers. |
| LR-04 | Desktop-independent jobs | T2 T4 | **not_implemented** | — | missing | crates/fermi-core/src/hub.rs | Tasks run inside the UI process and are marked interrupted when it closes. |
| LR-05 | Connection recovery | T1 T4 | **not_implemented** | — | partial | crates/fermi/web/index.html (WebSocket reconnect), hub.rs (Scanner) | Dashboard clients reconnect and re-sync from the shared history; no worker-side event log. |
| LR-06 | Remote worker death | T3 T4 | **not_implemented** | — | missing | — | No remote workers or heartbeats beyond local stale detection. |
| LR-07 | Remote cancellation | T3 T4 | **not_implemented** | — | missing | — | No remote cancellation. |
| LR-08 | Runtime compatibility | T0 T1 | **not_implemented** | — | missing | — | No worker protocol versioning. |

## 12 Evaluation and self-improvement

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| EV-01 | Five-task smoke evaluation | T1 | **not_implemented** | — | missing | — | No evaluation harness, datasets or scoring. |
| EV-02 | Cross-model reports | T1 T4 | **not_implemented** | — | missing | — | No evaluation harness, datasets or scoring. |
| EV-03 | Known failure detection | T1 | **not_implemented** | — | missing | — | No evaluation harness, datasets or scoring. |
| EV-04 | Configuration comparison | T1 T4 | **not_implemented** | — | missing | — | No evaluation harness, datasets or scoring. |
| EV-05 | Reproducible evaluation setup | T1 | **not_implemented** | — | missing | — | No evaluation harness, datasets or scoring. |
| EV-06 | Protected evaluation assets | T3 | **not_implemented** | — | missing | — | No evaluation harness, datasets or scoring. |
| EV-07 | Dataset history | T1 | **not_implemented** | — | missing | — | No evaluation harness, datasets or scoring. |
| EV-08 | Evaluated improvement proposals | T1 | **not_implemented** | — | missing | — | No evaluation harness, datasets or scoring. |

## End-to-end workflows

| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |
| --- | --- | --- | --- | --- | --- | --- | --- |
| E2E-01 | User creates coding task > model selected > file changed > tests run | T2 | **not_implemented** | — | partial | dashboard + engine | Task, model routing and file changes work; no fermi-run test step or verified-change gate. |
| E2E-02 | User starts three coding tasks in parallel | T2 T3 | **not_implemented** | — | partial | crates/fermi-core/src/team.rs | Team runs isolate agents in worktrees; plain parallel tasks share the workspace folder. |
| E2E-03 | Browser agent develops and tests a simple UI | T2 | **not_implemented** | — | missing | — | Needs agent browser control (System 6). |
| E2E-04 | Provider fails halfway through execution | T2 T3 | **not_implemented** | — | partial | crates/fermi-core/src/engine.rs | Limits and auth failures restart on the next account; mid-run outage has no bounded retry or checkpoint. |
| E2E-05 | User cancels task during a tool operation | T2 T3 | **not_implemented** | — | partial | crates/fermi-core/src/hub.rs (cancel) | Cancel aborts the run and kills the CLI process; grandchildren are not tracked. |
| E2E-06 | User closes desktop during remote execution | T2 T4 | **not_implemented** | — | missing | — | Needs remote execution (System 11). |
| E2E-07 | Plugin attempts forbidden operation | T2 T3 | **not_implemented** | — | missing | — | Needs a plugin sandbox (System 7). |
| E2E-08 | Agent completes task exceeding budget | T2 T3 | **not_implemented** | — | missing | — | Needs budgets (System 9). |
| E2E-09 | User compares two models on identical task | T2 T4 | **not_implemented** | — | partial | crates/fermi-core/src/team.rs, hub.rs | Two models can be run separately; no paired comparison with isolation and a shared oracle. |
| E2E-10 | Agent finishes a multi-file refactor | T2 | **not_implemented** | — | partial | crates/fermi-core/src/code.rs, team.rs | Impact analysis and agent edits exist; no mandatory-check gate on the final revision. |

