# Acceptance matrix

Phase 1 audit of fermi (called APEX in the spec) against `docs/FERMI_ADE_Verification_and_Acceptance_Testing`.

Audited revision: `522e356` (working tree clean apart from `docs/`). Date: 2026-10-23.

**No acceptance test exists yet, so every case's published status is `not_implemented` (execution state `not_run`).**
The *capability* column is an audit finding from reading the code and from earlier manual runs; it is not verification.
Related unit tests are listed for reference; they exercise pieces of a capability but are not acceptance evidence.

Capability: **implemented** (the behavior exists end to end), **partial** (some of the expected behavior exists), **missing** (absent).

## Summary

| System | Cases | Implemented | Partial | Missing | Acceptance passed |
| --- | --- | --- | --- | --- | --- |
| 1 Model-agnostic AI engine | 8 | 2 | 3 | 3 | 0 / 8 |
| 2 Multi-agent orchestration | 8 | 3 | 4 | 1 | 0 / 8 |
| 3 Semantic codebase intelligence | 8 | 4 | 2 | 2 | 0 / 8 |
| 4 Persistent project memory | 8 | 0 | 0 | 8 | 0 / 8 |
| 5 Integrated verification engine | 8 | 0 | 0 | 8 | 0 / 8 |
| 6 Browser and application control | 8 | 0 | 0 | 8 | 0 / 8 |
| 7 Composable harness and plugin system | 8 | 0 | 2 | 6 | 0 / 8 |
| 8 Agent observability | 8 | 1 | 5 | 2 | 0 / 8 |
| 9 Cost and performance optimization | 8 | 0 | 3 | 5 | 0 / 8 |
| 10 Security and isolation | 8 | 0 | 3 | 5 | 0 / 8 |
| 11 Local and remote execution | 8 | 1 | 1 | 6 | 0 / 8 |
| 12 Evaluation and self-improvement | 8 | 0 | 0 | 8 | 0 / 8 |
| End-to-end workflows | 10 | 0 | 6 | 4 | 0 / 10 |
| **Total** | **106** | **11** | **29** | **66** | **0 / 106** |

## 1 Model-agnostic AI engine

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| AI-01 | Adapter normalization | T1 | F-PROVIDER | partial | crates/fermi-core/src/providers/*.rs, events.rs | Five adapters (Claude/Codex/Gemini CLIs, Anthropic and OpenAI APIs) normalize into one Event stream and RunResult. No deterministic provider simulator exists; CLI adapters need fake CLI binaries via [bins], API adapters a local server via base_url. | — |
| AI-02 | Common task interface | T1 | F-CODE F-PROVIDER | implemented | crates/fermi-core/src/engine.rs (Task, Engine::run) | Every provider runs through Task and Engine::run with the same lifecycle and Event schema. Unverified against a fixture oracle. | router::tests::* |
| AI-03 | Model switching with state | T1 | F-PROVIDER | implemented | crates/fermi-core/src/hub.rs (follow_up, continuation) | Model switch per reply; same provider resumes the session, other providers get the transcript; Routed events attribute each turn. | hub::tests::switching_models |
| AI-04 | Rate limit handling | T1 | F-PROVIDER | missing | crates/fermi-core/src/providers/api.rs, engine.rs | 429 maps to Limited (Retry-After read) and fails over to the next account; there is no backoff/retry policy or attempt ceiling. | — |
| AI-05 | Provider outage handling | T1 | F-PROVIDER | partial | crates/fermi-core/src/engine.rs (run_inner), providers/api.rs (send) | HTTP 5xx is retryable: the task moves to the profile's next account, then the next candidate model, and fails with the last error when all are exhausted. This fallback is always on (not explicitly configured or disableable) and the switch is recorded as a Failover event. CLI adapters classify errors from text. | — |
| AI-06 | Capability validation | T0 T1 | F-PROVIDER | missing | crates/fermi-core/src/models.rs (Cap) | Capabilities cover code/reasoning/general/fast/image; tool calling is not modeled or validated before dispatch. | — |
| AI-07 | Budget exhaustion | T0 T1 | F-PROVIDER | missing | — | No budget or reservation ledger. | — |
| AI-08 | Local endpoint support | T1 T2 | F-PROVIDER | partial | crates/fermi-core/src/providers/api.rs (base_url) | API accounts accept base_url (OpenAI-compatible endpoint), untested against a local model server; uses the Responses API, which local servers may not support. | — |

## 2 Multi-agent orchestration

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| MA-01 | Concurrent workers | T1 | F-CODE F-WORKER | implemented | crates/fermi-core/src/hub.rs (submit/launch), team.rs (schedule) | Tasks run concurrently; team runs start up to N agents in parallel, each in its own worktree. | — |
| MA-02 | Dependency ordering | T0 T1 | F-WORKER | implemented | crates/fermi-core/src/team.rs (schedule, topo) | Dependencies gate starts; failed or interrupted tasks block dependents (run goes to attention); cancel stops the run. | team::tests::plans_parse_and_order |
| MA-03 | Independent edits | T1 | F-CODE | implemented | crates/fermi-core/src/team.rs (start_task, integrate) | Separate worktrees and branches merged with --no-ff into an integration branch; manually verified on a fixture (2 runs). | — |
| MA-04 | Conflicting edits | T1 | F-CODE | partial | crates/fermi-core/src/team.rs (integrate) | Conflicts are detected (unmerged paths) and logged with both branches kept, but resolution is delegated automatically to a debugger agent rather than escalated to the user. Manually verified once. | — |
| MA-05 | Scoped delegation | T1 | F-WORKER F-SECURITY | missing | — | No delegate tool; agents cannot spawn scoped subtasks. | — |
| MA-06 | Worker crash isolation | T1 T3 | F-WORKER | partial | crates/fermi-core/src/hub.rs, team.rs (on_done) | A failed agent marks only its task failed and others continue; untested with a killed process. | — |
| MA-07 | Parent cancellation | T1 T3 | F-WORKER | partial | crates/fermi-core/src/team.rs (cancel), providers/proc.rs (kill_on_drop) | Cancel aborts running agents (child processes killed on drop) and queued tasks never start; grandchildren of the CLI are not tracked. | — |
| MA-08 | Verified integration | T1 T2 | F-CODE | partial | crates/fermi-core/src/team.rs (integrate, merge) | Merge into the user's branch requires approval and the final checks run on the integration branch, but checks are agent judgments: there is no mechanical verification command gating the merge. | — |

## 3 Semantic codebase intelligence

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CI-01 | Repository indexing | T1 | F-CODE | partial | crates/fermi-core/src/code.rs (Index::open/update) | Indexes Rust, Python, JS, TS/TSX, Go with .gitignore; unsupported files are skipped silently instead of being listed as unsupported. | code::tests::rust_project_graph, python_and_typescript |
| CI-02 | Exact symbol lookup | T1 | F-CODE | implemented | crates/fermi-core/src/code.rs (search, symbol) | Exact and prefix lookup with file and line span. | code::tests::* |
| CI-03 | Caller discovery | T1 | F-CODE | implemented | crates/fermi-core/src/code.rs (graph callers) | Static callers resolved same-file, then imported files; the text output declares that dynamic calls may be missing. | code::tests::rust_project_graph |
| CI-04 | Scope-safe rename | T1 | F-CODE | missing | — | No semantic rename / edit API. | — |
| CI-05 | Incremental refresh | T1 | F-CODE | implemented | crates/fermi-core/src/code.rs (update) | Incremental re-parse by mtime and size on every query; no completion event. | code::tests::rust_project_graph (incremental assertion) |
| CI-06 | Invalid syntax handling | T1 | F-CODE | missing | crates/fermi-core/src/code.rs | tree-sitter parses error-tolerantly; syntax errors are not surfaced and results are not marked stale. | — |
| CI-07 | Dependency impact | T1 | F-CODE | implemented | crates/fermi-core/src/code.rs (impact) | Transitive callers and importers with visited sets (cycles terminate) and tests to run; unsupported relationships are not explicitly disclosed. | code::tests::rust_project_graph |
| CI-08 | Symbol identity | T1 | F-CODE | partial | crates/fermi-core/src/code.rs (find, resolution) | Duplicate names resolve per importing file; lookup by Type::name or path:line, but not by module-qualified path (crate::a::helper). | — |

## 4 Persistent project memory

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PM-01 | Durable decisions | T1 | F-MEMORY | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). | — |
| PM-02 | Fresh-session retrieval | T1 | F-MEMORY | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). | — |
| PM-03 | Decision versioning | T1 | F-MEMORY | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). | — |
| PM-04 | Stale source detection | T1 | F-MEMORY F-CODE | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). | — |
| PM-05 | Semantic retrieval | T1 | F-MEMORY | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). | — |
| PM-06 | Contradiction handling | T1 | F-MEMORY | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). | — |
| PM-07 | Untrusted memory content | T1 T3 | F-MEMORY F-SECURITY | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). | — |
| PM-08 | Project memory isolation | T1 T3 | F-MEMORY | missing | — | fermi has no project memory store (only conversation history in ~/.fermi/history). | — |

## 5 Integrated verification engine

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| VE-01 | Compiler diagnostics | T1 | F-CODE | missing | — | No verification service; agents run compilers themselves. | — |
| VE-02 | Failing test feedback | T1 | F-CODE | missing | — | No verification tool; test output is only what the agent reports. | — |
| VE-03 | Repair and rerun | T1 T2 | F-CODE F-PROVIDER | missing | — | No repair-and-rerun loop owned by fermi. | — |
| VE-04 | Lint enforcement | T1 | F-CODE | missing | — | No lint integration. | — |
| VE-05 | Dependency advisory detection | T1 | F-CODE | missing | — | No dependency advisory scanner. | — |
| VE-06 | Structured edit integrity | T1 | F-CODE | missing | — | No structured (syntax-tree) edit API. | — |
| VE-07 | Bounded repair attempts | T0 T1 | F-CODE F-PROVIDER | missing | — | No bounded repair attempts. | — |
| VE-08 | Completion enforcement | T1 T2 | F-CODE | missing | crates/fermi-core/src/team.rs (merge) | No completion gate tied to mandatory checks (team merge needs user approval only). | — |

## 6 Browser and application control

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| BA-01 | Local site launch | T2 | F-WEB | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. | — |
| BA-02 | Form submission | T2 | F-WEB | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. | — |
| BA-03 | Navigation chain | T2 | F-WEB | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. | — |
| BA-04 | Screenshot artifact | T2 | F-WEB | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. | — |
| BA-05 | Console error capture | T2 | F-WEB | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. | — |
| BA-06 | Network failure detection | T2 | F-WEB | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. | — |
| BA-07 | Mobile layout assertions | T2 | F-WEB | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. | — |
| BA-08 | Dynamic readiness | T2 | F-WEB | missing | crates/fermi/src/preview.rs (user-facing only) | The Preview tab is a browser for the user; agents have no browser automation tools in fermi. | — |

## 7 Composable harness and plugin system

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| PL-01 | Plugin registration | T1 | F-PLUGIN | missing | — | No plugin manager or manifest format. | — |
| PL-02 | Plugin disabling | T1 | F-PLUGIN | missing | — | No plugin manager. | — |
| PL-03 | Command collisions | T0 T1 | F-PLUGIN | missing | — | No command registry. | — |
| PL-04 | Plugin fault isolation | T1 T3 | F-PLUGIN | missing | — | No plugin host. | — |
| PL-05 | MCP interoperability | T1 | F-PLUGIN | partial | crates/fermi-core/src/mcp.rs | fermi is itself an MCP server (bridge tools); connecting fermi to an external MCP server is left to the CLIs' own configs. | — |
| PL-06 | Adapter substitution | T1 | F-PLUGIN F-PROVIDER | partial | crates/fermi-core/src/providers/mod.rs, config.rs ([[models]], bins) | Models and CLI binaries can be overridden in config; no pluggable adapter interface. | — |
| PL-07 | Plugin boundary enforcement | T3 | F-PLUGIN F-SECURITY | missing | — | No plugin sandbox. | — |
| PL-08 | API incompatibility | T0 T1 | F-PLUGIN | missing | — | No plugin API versioning. | — |

## 8 Agent observability

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| OB-01 | Complete operation trace | T1 | F-PROVIDER | partial | crates/fermi-core/src/hub.rs (TaskRec.events) | Tool calls and turns are traced; individual model calls inside a CLI run are not visible. | — |
| OB-02 | Invocation detail | T1 | F-PROVIDER F-SECURITY | partial | crates/fermi-core/src/events.rs (Tool) | Tool name and a one-line input summary only; no outputs, duration or status. | — |
| OB-03 | Usage fidelity | T0 T1 | F-PROVIDER | partial | crates/fermi-core/src/state.rs (UsageRecord), providers/claude_cli.rs | Input (incl. cache), output tokens and CLI-reported cost recorded per run; cached tokens are folded into input, unknown fields recorded as 0. | — |
| OB-04 | Failure diagnostics | T1 | F-PROVIDER | partial | crates/fermi-core/src/events.rs (Done, Failover, Blocked) | Run failures and blocked approvals recorded; per-tool failures are not. | — |
| OB-05 | Trace persistence | T1 | F-MEMORY | implemented | crates/fermi-core/src/hub.rs (history files) | Every task's events persist in ~/.fermi/history/<id>.json and survive restarts. | hub::tests::scanner_sees_other_writers_and_stale_tasks |
| OB-06 | Resume correlation | T1 T3 | F-WORKER | partial | crates/fermi-core/src/hub.rs (follow_up) | Follow-ups keep the task id; there is no attempt id or durable checkpoint resume. | — |
| OB-07 | Secret redaction | T1 T3 | F-SECURITY | missing | — | No secret redaction before persistence. | — |
| OB-08 | Deterministic replay | T1 | F-PROVIDER | missing | — | No captured-response replay. | — |

## 9 Cost and performance optimization

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| CO-01 | Token cost arithmetic | T0 | F-PROVIDER | missing | — | Cost comes from the CLI's own total_cost_usd; no price tables. | — |
| CO-02 | Cached token discounts | T0 | F-PROVIDER | missing | — | No cached-input pricing. | — |
| CO-03 | Economy routing | T0 T1 | F-PROVIDER | partial | crates/fermi-core/src/router.rs | Auto routing sends light work to tier 1 models; no explicit economy policy with costs. | router::tests::auto_routes_by_need |
| CO-04 | Quality routing | T0 T1 | F-PROVIDER | partial | crates/fermi-core/src/router.rs | Heavy work goes to tier 3; no declared quality ranking policy. | router::tests::auto_routes_by_need |
| CO-05 | Shared budget reservations | T0 T1 | F-PROVIDER F-WORKER | missing | — | No shared budget reservations. | — |
| CO-06 | Versioned price updates | T0 T1 | F-PROVIDER | missing | — | No versioned price tables. | — |
| CO-07 | Large context warnings | T0 T1 | F-PROVIDER | missing | — | No context-size estimation or warnings. | — |
| CO-08 | Usage attribution | T1 | F-PROVIDER F-WORKER | partial | crates/fermi-core/src/state.rs (usage.jsonl) | Usage logged per run by profile, account and model; not grouped by team run, parent or child task. | — |

## 10 Security and isolation

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| SE-01 | Forbidden file read | T3 | F-SECURITY | missing | — | fermi enforces no file boundary; it relies on each CLI (Codex sandbox, Claude permission modes). | — |
| SE-02 | Secret isolation | T3 | F-SECURITY | missing | — | Child processes inherit the full environment. | — |
| SE-03 | Destructive action gating | T3 | F-SECURITY | partial | crates/fermi-core/src/providers/claude_cli.rs (permission_denials), hub.rs (allow) | Claude in acceptEdits/default modes blocks commands that need approval; fermi surfaces them and continues only after the user allows them. | — |
| SE-04 | Network restrictions | T3 | F-SECURITY | missing | — | No network policy. | — |
| SE-05 | Cross-task isolation | T3 | F-SECURITY F-WORKER | partial | crates/fermi-core/src/team.rs (worktrees) | Team agents get separate worktrees, but no OS-level sandbox or secret separation. | — |
| SE-06 | Repository injection resistance | T1 T3 | F-SECURITY F-CODE | missing | — | No provenance or untrusted-content policy. | — |
| SE-07 | Scoped approval audit | T1 T3 | F-SECURITY | partial | crates/fermi-core/src/hub.rs (FollowUp.allow), team.rs | Approvals are scoped to tool rules (Bash(node:*)) per conversation and stored; no actor, timestamp or replay protection. | — |
| SE-08 | Alternate route bypass | T3 | F-SECURITY F-PLUGIN | missing | — | No shared policy across entry points. | — |

## 11 Local and remote execution

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| LR-01 | Local execution | T1 T2 | F-CODE F-WORKER | implemented | crates/fermi-core/src/engine.rs, hub.rs | Tasks run locally and record changed files and artifacts. | — |
| LR-02 | Container equivalence | T1 T3 | F-CODE F-WORKER | missing | — | No container workers. | — |
| LR-03 | Remote dispatch | T4 | F-CODE F-WORKER | missing | — | No remote workers. | — |
| LR-04 | Desktop-independent jobs | T2 T4 | F-WORKER | missing | crates/fermi-core/src/hub.rs | Tasks run inside the UI process and are marked interrupted when it closes. | — |
| LR-05 | Connection recovery | T1 T4 | F-WORKER | partial | crates/fermi/web/index.html (WebSocket reconnect), hub.rs (Scanner) | Dashboard clients reconnect and re-sync from the shared history; no worker-side event log. | — |
| LR-06 | Remote worker death | T3 T4 | F-WORKER | missing | — | No remote workers or heartbeats beyond local stale detection. | — |
| LR-07 | Remote cancellation | T3 T4 | F-WORKER | missing | — | No remote cancellation. | — |
| LR-08 | Runtime compatibility | T0 T1 | F-WORKER | missing | — | No worker protocol versioning. | — |

## 12 Evaluation and self-improvement

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| EV-01 | Five-task smoke evaluation | T1 | F-EVAL | missing | — | No evaluation harness, datasets or scoring. | — |
| EV-02 | Cross-model reports | T1 T4 | F-EVAL F-PROVIDER | missing | — | No evaluation harness, datasets or scoring. | — |
| EV-03 | Known failure detection | T1 | F-EVAL | missing | — | No evaluation harness, datasets or scoring. | — |
| EV-04 | Configuration comparison | T1 T4 | F-EVAL | missing | — | No evaluation harness, datasets or scoring. | — |
| EV-05 | Reproducible evaluation setup | T1 | F-EVAL | missing | — | No evaluation harness, datasets or scoring. | — |
| EV-06 | Protected evaluation assets | T3 | F-EVAL F-SECURITY | missing | — | No evaluation harness, datasets or scoring. | — |
| EV-07 | Dataset history | T1 | F-EVAL | missing | — | No evaluation harness, datasets or scoring. | — |
| EV-08 | Evaluated improvement proposals | T1 | F-EVAL | missing | — | No evaluation harness, datasets or scoring. | — |

## End-to-end workflows

| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |
| --- | --- | --- | --- | --- | --- | --- | --- |
| E2E-01 | User creates coding task > model selected > file changed > tests run | T2 | F-CODE F-PROVIDER | partial | dashboard + engine | Task, model routing and file changes work; no fermi-run test step or verified-change gate. | — |
| E2E-02 | User starts three coding tasks in parallel | T2 T3 | F-CODE F-WORKER | partial | crates/fermi-core/src/team.rs | Team runs isolate agents in worktrees; plain parallel tasks share the workspace folder. | — |
| E2E-03 | Browser agent develops and tests a simple UI | T2 | F-CODE F-WEB F-PROVIDER | missing | — | Needs agent browser control (System 6). | — |
| E2E-04 | Provider fails halfway through execution | T2 T3 | F-CODE F-PROVIDER | partial | crates/fermi-core/src/engine.rs | Limits and auth failures restart on the next account; mid-run outage has no bounded retry or checkpoint. | — |
| E2E-05 | User cancels task during a tool operation | T2 T3 | F-WORKER | partial | crates/fermi-core/src/hub.rs (cancel) | Cancel aborts the run and kills the CLI process; grandchildren are not tracked. | — |
| E2E-06 | User closes desktop during remote execution | T2 T4 | F-CODE F-WORKER | missing | — | Needs remote execution (System 11). | — |
| E2E-07 | Plugin attempts forbidden operation | T2 T3 | F-PLUGIN F-SECURITY | missing | — | Needs a plugin sandbox (System 7). | — |
| E2E-08 | Agent completes task exceeding budget | T2 T3 | F-PROVIDER F-WORKER | missing | — | Needs budgets (System 9). | — |
| E2E-09 | User compares two models on identical task | T2 T4 | F-CODE F-EVAL F-PROVIDER | partial | crates/fermi-core/src/team.rs, hub.rs | Two models can be run separately; no paired comparison with isolation and a shared oracle. | — |
| E2E-10 | Agent finishes a multi-file refactor | T2 | F-CODE F-PROVIDER | partial | crates/fermi-core/src/code.rs, team.rs | Impact analysis and agent edits exist; no mandatory-check gate on the final revision. | — |

