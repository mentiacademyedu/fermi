"""Acceptance matrix: the catalog joined with the capability audit and the published results.

    python tests/acceptance/phase1_audit.py <repo> [--registry] [--run tests/reports/<run>/summary.json]

Writes docs/ACCEPTANCE_MATRIX.md. With --registry it also regenerates
tests/acceptance/systems.yaml from the catalog and audit, keeping each case's
testFile/testName/repetitions and taking status from the --run summary.
"""
import re, sys, os, json

ROOT = sys.argv[1]
WRITE_REGISTRY = "--registry" in sys.argv
RUN = sys.argv[sys.argv.index("--run") + 1] if "--run" in sys.argv else None
doc = open(os.path.join(ROOT, "docs", "FERMI_ADE_Verification_and_Acceptance_Testing"), encoding="utf8").read().replace("\r\n", "\n")

SYSTEMS = [
    ("AI", "model-agnostic-ai-engine", "1 Model-agnostic AI engine"),
    ("MA", "multi-agent-orchestration", "2 Multi-agent orchestration"),
    ("CI", "semantic-codebase-intelligence", "3 Semantic codebase intelligence"),
    ("PM", "persistent-project-memory", "4 Persistent project memory"),
    ("VE", "integrated-verification-engine", "5 Integrated verification engine"),
    ("BA", "browser-and-application-control", "6 Browser and application control"),
    ("PL", "composable-harness-and-plugins", "7 Composable harness and plugin system"),
    ("OB", "agent-observability", "8 Agent observability"),
    ("CO", "cost-and-performance", "9 Cost and performance optimization"),
    ("SE", "security-and-isolation", "10 Security and isolation"),
    ("LR", "local-and-remote-execution", "11 Local and remote execution"),
    ("EV", "evaluation-and-self-improvement", "12 Evaluation and self-improvement"),
    ("E2E", "end-to-end", "End-to-end workflows"),
]

cases = []
for m in re.finditer(r"^### ((?:[A-Z]{2}|E2E)-\d\d) (.+?)\n(.*?)(?=^### |^## |\Z)", doc, re.M | re.S):
    cid, title, body = m.group(1), m.group(2).strip(), m.group(3)
    def field(name):
        f = re.search(r"\*\*" + re.escape(name) + r":\*\*\s*(.+)", body)
        return f.group(1).strip() if f else ""
    pf = field("Profile and fixtures")
    profiles, _, fixtures = pf.partition("|")
    cases.append({
        "id": cid,
        "title": title,
        "profiles": profiles.split(),
        "fixtures": fixtures.split(),
        "action": field("Action"),
        "expected": field("Expected"),
        "assertions": field("Pass assertions"),
        "evidence": field("Evidence"),
    })
ids = [c["id"] for c in cases]
assert len(ids) == len(set(ids)) == 106, (len(ids), len(set(ids)))

# ── the audit: capability status in fermi today ─────────────────────────────
# status: implemented | partial | missing ; where: files ; tests: related unit tests (not acceptance evidence)
A = {}
def a(cid, status, where, note, tests=""):
    A[cid] = {"capability": status, "where": where, "note": note, "related_tests": tests}

core = "crates/fermi-core/src"
# System 1
a("AI-01", "implemented", f"{core}/providers/*.rs, events.rs", "Five adapters (Claude/Codex/Gemini CLIs, Anthropic and OpenAI APIs) normalize into one Event stream and RunResult. The acceptance test covers the three CLI adapters through the fake CLI and the two API adapters against F-PROVIDER mock endpoints (added 2026-10-24); the API adapters carry text only, no tool calls.")
a("AI-02", "implemented", f"{core}/engine.rs (Task, Engine::run)", "Every provider runs through Task and Engine::run with the same lifecycle and Event schema. Unverified against a fixture oracle.", "router::tests::*")
a("AI-03", "implemented", f"{core}/hub.rs (follow_up, continuation)", "Model switch per reply; same provider resumes the session, other providers get the transcript; Routed events attribute each turn.", "hub::tests::switching_models")
a("AI-04", "implemented", f"{core}/engine.rs (run_inner retry), providers/api.rs (send), config.rs (RetryCfg), clock.rs", "An API's HTTP 429 is retried on the same account after its Retry-After (or a doubling backoff), up to [retry] attempts (default 3) and only for waits within max_wait_secs; then the account is marked limited and the task fails over or fails. Waits run on the engine's clock, which tests make manual. Added 2026-10-24.")
a("AI-05", "implemented", f"{core}/engine.rs (run_inner), providers/mod.rs (classify_error)", "Provider outages (HTTP 5xx from the APIs; 5xx, overloaded or unavailable from the CLIs) fail over to the profile's next account, then the next candidate model, recorded as Failover events; pinning the account and model disables it. Fixed 2026-10-23: CLI outages failed without fallback (D-004).")
a("AI-06", "implemented", f"{core}/models.rs (tools), engine.rs (plan, pick_account_for), config.rs (require_tools)", "Models declare tool calling (`tools`, default yes for the built-ins); a task or profile that needs tools never reaches a model that can't (rejected before dispatch when it's the only choice, otherwise skipped with a log line) or an HTTP API account; a profile requiring tools with no capable model is a config error. Added 2026-10-24.")
a("AI-07", "implemented", f"{core}/billing.rs (Ledger), engine.rs", "Profiles with a budget reserve each call's estimate atomically in a shared ledger before dispatch; a call that doesn't fit is refused with the numbers behind it; actual costs settle later and overruns stay visible. Added 2026-10-24.")
a("AI-08", "implemented", f"{core}/providers/api.rs (chat, base), config.rs (api_format)", "openai API accounts with api_format = \"chat\" speak OpenAI-compatible chat completions (llama.cpp, Ollama, vLLM, LM Studio); with a base_url no key is needed. Tested in its own process with hosted credentials removed and outbound traffic recorded. Added 2026-10-24.")
# System 2
a("MA-01", "implemented", f"{core}/hub.rs (submit/launch), team.rs (schedule)", "Tasks run concurrently; team runs start up to N agents in parallel, each in its own worktree.")
a("MA-02", "implemented", f"{core}/team.rs (schedule, topo)", "Dependencies gate starts; failed or interrupted tasks block dependents (run goes to attention); cancel stops the run.", "team::tests::plans_parse_and_order")
a("MA-03", "implemented", f"{core}/team.rs (start_task, integrate)", "Separate worktrees and branches merged with --no-ff into an integration branch; manually verified on a fixture (2 runs).")
a("MA-04", "partial", f"{core}/team.rs (integrate)", "Conflicts are detected (unmerged paths) and logged with both branches kept, but resolution is delegated automatically to a debugger agent rather than escalated to the user. Manually verified once.")
a("MA-05", "implemented", f"{core}/mcp.rs (delegate)", "The bridge's delegate tool runs a subtask as its own agent with only the instructions, the files given (inside the folder, allowed by deny_read; anything else refuses the delegation) and agent-visible memory; its permission is the one asked, never broader than the delegator's. Each envelope is kept in delegations/<parent>/. Added 2026-10-24.")
a("MA-06", "implemented", f"{core}/hub.rs, team.rs (on_done)", "A killed agent process marks only its task failed; concurrent tasks finish with their results. In team runs a failed task blocks its dependents (MA-02).")
a("MA-07", "implemented", f"{core}/team.rs (cancel), providers/proc.rs (kill_tree)", "Cancel stops running agents and every process they started (process tree), and queued tasks never start. Fixed 2026-10-23: processes started by the CLI were orphaned (D-005).")
a("MA-08", "implemented", f"{core}/team.rs (start_verification, merge)", "The integrated branch must pass the project's mandatory checks before merging; a failing run gets bounded debugger fixes and re-checks, and merge refuses anything but the exact commit that passed. Added 2026-10-23.")
# System 3
a("CI-01", "implemented", f"{core}/code.rs (Index::open/update)", "Indexes Rust, Python, JS, TS/TSX, Go with .gitignore; lists unsupported files and the indexed git revision (added 2026-10-23, D-006).", "code::tests::rust_project_graph, python_and_typescript")
a("CI-02", "implemented", f"{core}/code.rs (search, symbol)", "Exact and prefix lookup with file and line span.", "code::tests::*")
a("CI-03", "implemented", f"{core}/code.rs (graph callers)", "Static callers resolved same-file, then imported files; results declare the analysis scope (dynamic dispatch is not covered; added 2026-10-23, D-006).", "code::tests::rust_project_graph")
a("CI-04", "implemented", f"{core}/code.rs (rename_symbol), mcp.rs (code_rename)", "Renames one resolved symbol: its definition, resolved callers and importing lines, through the syntax tree (identifiers only); refuses ambiguous targets, names already taken and edits that don't parse; lists same-named text it left alone. Added 2026-10-24.")
a("CI-05", "implemented", f"{core}/code.rs (update)", "Incremental re-parse by mtime and size on every query; no completion event.", "code::tests::rust_project_graph (incremental assertion)")
a("CI-06", "implemented", f"{core}/code.rs (update, FileIdx error/stale/history)", "A file that stops parsing keeps the symbols of its last valid version, marked stale on every result, with a diagnostic (file, line, column) and a parse history; fixing it clears the flag. Added 2026-10-24.")
a("CI-07", "implemented", f"{core}/code.rs (impact)", "Transitive callers and importers with visited sets (cycles terminate), tests to run, and a declared scope of what isn't covered.", "code::tests::rust_project_graph")
a("CI-08", "implemented", f"{core}/code.rs (find, resolution)", "Duplicate names resolve per importing file; lookup by Type::name, module path (util::text::helper, added 2026-10-23) or path:line.")
# System 4
m = f"{core}/memory.rs"
a("PM-01", "implemented", m, "Records live in a per-project append-only log with stable ids, authors and sources; they survive restarts and a write cut short by a crash. Added 2026-10-24.")
a("PM-02", "implemented", f"{m}, hub.rs (with_memory), mcp.rs", "New tasks get the most relevant records for agents, with author and sources; private records never reach agents. Agents also get memory_search/get/record tools. Added 2026-10-24.")
a("PM-03", "implemented", m, "Revisions add versions under the same id with the reason; current retrieval uses the latest and history keeps every version. Added 2026-10-24.")
a("PM-04", "implemented", m, "Sources keep the git revision and a content hash; a deleted or changed source marks the record stale wherever it's shown, including agent context. Added 2026-10-24.")
a("PM-05", "implemented", m, "Deterministic retrieval (fermi-lexical-v1): stemming, a pinned synonym table and BM25. It finds paraphrases that share vocabulary or listed synonyms; it is not an embedding model. Added 2026-10-24.")
a("PM-06", "implemented", m, "Topic summaries list every current record with its author and source and flag a possible conflict; nothing is merged. Added 2026-10-24.")
a("PM-07", "implemented", f"{m}, policy.rs, providers/claude_cli.rs", "Comments become provenance=source records framed as untrusted data in agent context; deny_read paths go to Claude as --disallowedTools Read rules. Enforcement inside the CLI is Claude's and was emulated in the test. Added 2026-10-24.")
a("PM-08", "implemented", f"{m}, crates/fermi/src/server.rs", "One store per project; the API only opens the dashboard's workspaces; a record of another project and a missing id get the same answer. Added 2026-10-24.")
# System 5
a("VE-01", "implemented", f"{core}/verify.rs (run, parse_output)", "fermi runs the project's checks (.fermi/checks.toml or detected from manifests) and parses diagnostics from cargo JSON and common text formats (rustc, gcc, go, tsc, eslint, ruff, Python) with file, line and column. Added 2026-10-23.")
a("VE-02", "implemented", f"{core}/verify.rs, mcp.rs (run_checks)", "Agents call the run_checks tool and get each failing test with its output and the exit code; verified tasks record a Verify event. Added 2026-10-23.")
a("VE-03", "implemented", f"{core}/verify.rs (run_task)", "Verified tasks: after the agent, fermi runs the checks and sends failures back to the same session; protected files (protect globs) must stay unchanged. Added 2026-10-23.")
a("VE-04", "implemented", f"{core}/verify.rs (Kind::Lint)", "Lint checks run like any other; clippy and text-format rule ids and locations are reported and a mandatory lint failure fails the task. Added 2026-10-23.")
a("VE-05", "implemented", f"{core}/verify.rs (audit)", "Built-in scanner matches Cargo.lock, package-lock.json and requirements.txt against OSV records; records scanner version and dataset SHA-256; nothing is installed or run. Added 2026-10-23.")
a("VE-06", "implemented", f"{core}/code.rs (replace_symbol), mcp.rs (code_replace)", "Replaces one definition by its syntax-tree byte range, refuses ambiguous targets and edits that don't parse or drop the definition. Added 2026-10-23.")
a("VE-07", "implemented", f"{core}/verify.rs (run_task)", "Repair turns are bounded by max_repairs (task option, else the project's, else 3); the final failure is explicit. Added 2026-10-23.")
a("VE-08", "implemented", f"{core}/verify.rs, team.rs (merge), hub.rs", "A verified task ends done only if every mandatory check passed; a team run is mergeable only with passing checks on exactly the commit being merged (service and API). Added 2026-10-23.")
# System 6
a("BA-01", "implemented", f"{core}/browser.rs (open), mcp.rs (browser tools)", "Agents open pages in a headless Chromium-based browser through fermi's bridge; the result names the URL, title and page identity, and an unreachable page or an HTTP error is a failure.")
a("BA-02", "implemented", f"{core}/browser.rs (fill, click)", "Fields are typed into once visible and enabled; clicks use real mouse events on the element's centre once it is visible, enabled and uncovered.")
a("BA-03", "implemented", f"{core}/browser.rs (click, navigations)", "A click that starts a navigation returns once the new page has loaded; every main-frame navigation is recorded in the trace.")
a("BA-04", "implemented", f"{core}/browser.rs (screenshot)", "PNG screenshots of the viewport or full page are saved with their size and SHA-256, referenced from the trace.")
a("BA-05", "implemented", f"{core}/browser.rs (observe)", "Console errors, warnings and uncaught exceptions are captured with their source, redacted, and kept in the task's trace.")
a("BA-06", "implemented", f"{core}/browser.rs (observe)", "Failed requests (HTTP 4xx/5xx and network errors) are recorded with URL, method and status.")
a("BA-07", "implemented", f"{core}/browser.rs (viewport, check, state)", "Viewport and mobile emulation; element checks report visibility, enablement, coverage and viewport placement; the page state reports horizontal overflow.")
a("BA-08", "implemented", f"{core}/browser.rs (wait, click)", "Actions wait for readiness with recorded state changes; a wait that runs out is an error and a click never fires on a disabled or covered element.")
# System 7
a("PL-01", "implemented", f"{core}/plugins.rs (install, enable, registry)", "A plugin folder with a plugin.toml (name, version, plugin API, command, declared capabilities: tool, command or adapter) is installed and enabled; only declared capabilities register, each attributed to the plugin version.")
a("PL-02", "implemented", f"{core}/plugins.rs (disable)", "Disabling removes a plugin's capabilities from the registry and the agents' bridge; other plugins keep working.")
a("PL-03", "implemented", f"{core}/plugins.rs (build, Registry::resolve), config.rs ([plugins] collisions)", "Plugins are registered in name order; a shared name is namespaced (<plugin>.<name>, the bare name ambiguous) or rejected for all claimants, by policy, independent of install order.")
a("PL-04", "implemented", f"{core}/plugins.rs (Client)", "Every call runs the plugin in its own process with a timeout; crashes, hangs and protocol errors fail that call only and are logged against the plugin.")
a("PL-05", "implemented", f"{core}/plugins.rs (Client, validate, add_mcp)", "An MCP client for current servers (server/discover, per-request metadata) and older ones (initialize); tool schemas are kept as discovered and arguments are validated against them before a call.")
a("PL-06", "implemented", f"{core}/plugins.rs (run_adapter), providers/mod.rs", "An account can name a plugin adapter; its runs go through the normal engine path and come back as the usual events, usage records and classified errors (limits fail over).")
a("PL-07", "implemented", f"{core}/plugins.rs (scope), sandbox.rs", "Plugins run sandboxed (environment allowlist and egress proxy everywhere; Landlock files on Linux); path arguments outside the call's folder and the plugin's own, or under deny_read, are refused before the call, also when an agent calls through the bridge.")
a("PL-08", "implemented", f"{core}/plugins.rs (Manifest::check)", "A manifest needing an unsupported plugin API version is refused with the required and supported versions before anything is copied, registered or started.")
# System 8
a("OB-01", "implemented", f"{core}/trace.rs (Ops, export), providers/*_cli.rs", "Every model call (Claude per message id, Codex per turn, Gemini per run) and tool invocation is an operation with its id, parent call, attempt, try (failover) and sequence number; exported by `fermi trace` and /api/tasks/<id>/trace.")
a("OB-02", "implemented", f"{core}/events.rs (Tool, ToolResult), trace.rs", "Tool inputs, redacted and shortened outputs, status, error code and a duration measured on fermi's monotonic clock.")
a("OB-03", "implemented", f"{core}/events.rs (Call), providers/mod.rs (Usage::reported), state.rs", "Each model call keeps the provider's usage object field for field; the run's record marks counts the provider didn't report as null; Claude messages split across lines count once.")
a("OB-04", "implemented", f"{core}/trace.rs (Ops::result, finish)", "Failed tools keep status, error code (exit code or the CLI's error type) and redacted diagnostics, correlated to task, attempt and call; a tool that never reports back is no_result, never success.")
a("OB-05", "implemented", f"{core}/hub.rs (history files, stamps)", "Every task's events and their stamps persist in ~/.fermi/history/<id>.json and survive restarts.", "hub::tests::scanner_sees_other_writers_and_stale_tasks")
a("OB-06", "implemented", f"{core}/hub.rs (Hub::resume, TaskRec::reopen)", "An interrupted task resumes in its provider session as the next attempt of the same task; stamps keep one gapless sequence across attempts.")
a("OB-07", "implemented", f"{core}/redact.rs", "Credential formats, secret-looking environment values and [trace] redact patterns are removed from CLI output as it is read and from every event before history, tapes, exports or the live stream.")
a("OB-08", "implemented", f"{core}/trace.rs (tapes, replay)", "Each CLI's responses are recorded (redacted) with the task each attempt ran; `fermi replay` re-runs the task from them on a fixed clock with no CLI or network and compares normalized events and a state hash.")
# System 9
a("CO-01", "implemented", f"{core}/billing.rs (cost)", "Versioned price tables in config; exact picodollar arithmetic rounded half up to USD 0.000001 per call. Added 2026-10-24.")
a("CO-02", "implemented", f"{core}/billing.rs (cost), providers", "Cached and cache-write input tokens are tracked separately; the cache rate applies only where the model's price declares one. Added 2026-10-24.")
a("CO-03", "implemented", f"{core}/router.rs (policy)", "routing = \"economy\": the cheapest eligible model by estimate (capability required), declared tie-breakers, recorded decision. Added 2026-10-24.")
a("CO-04", "implemented", f"{core}/router.rs (policy)", "routing = \"quality\": the highest declared quality rank among models with the capability that fit the budget; recorded decision. Added 2026-10-24.")
a("CO-05", "implemented", f"{core}/billing.rs (Ledger::reserve)", "Reservations are atomic across threads and fermi processes (lock file); refusals leave no charge; settlement reconciles actual cost. Added 2026-10-24.")
a("CO-06", "implemented", f"{core}/billing.rs (price_at)", "Each call records the price version in effect and its calculation; new versions apply from their effective time without rewriting history. Added 2026-10-24.")
a("CO-07", "implemented", f"{core}/billing.rs (estimate), engine.rs", "Documented estimator (4 bytes per token); warnings from a configurable share of the context window; over-window requests aren't sent. Added 2026-10-24.")
a("CO-08", "implemented", f"{core}/billing.rs (report), state.rs", "Usage records carry task, delegating task, team run, agent and attempt; reports group by any of them and reconcile, showing failed attempts and unallocated calls. Added 2026-10-24.")
# System 10
a("SE-01", "implemented", f"{core}/policy.rs, sandbox.rs", "deny_read is enforced on every route (Claude read rules, shell commands that name the path, fermi's own tools); with sandbox on, Linux Landlock draws the file boundary at the OS. The OS part is Linux-only. Added 2026-10-24.")
a("SE-02", "implemented", f"{core}/sandbox.rs (Jail::env)", "A sandboxed agent gets only an allowlist of environment variables (system ones plus env_allow); fermi's own secrets, including API keys, are left out. Every platform. Added 2026-10-24.")
a("SE-03", "implemented", f"{core}/policy.rs, approvals.rs, mcp.rs", "deny_commands are refused and confirm_commands wait for the user through fermi's permission hook; nothing runs before a decision. Added 2026-10-24.")
a("SE-04", "implemented", f"{core}/sandbox.rs (Proxy)", "A sandboxed agent's traffic goes through fermi's egress proxy, which admits only allowed hosts and logs each decision; Linux Landlock also pins TCP to the proxy. The bypass block is Linux-only; UDP isn't covered. Added 2026-10-24.")
a("SE-05", "implemented", f"{core}/sandbox.rs, hub.rs", "Each task runs with its own folder, private temp and filtered environment; with sandbox on, Linux Landlock keeps one task from reading another's files. The OS file boundary is Linux-only. Added 2026-10-24.")
a("SE-06", "implemented", f"{core}/memory.rs, policy.rs", "Repository text reaches agents as untrusted memory (provenance=source), and the deny_read/egress policy holds against an injected read or exfiltration. Added 2026-10-24.")
a("SE-07", "implemented", f"{core}/approvals.rs", "Every request, decision and use is logged with actor, task, exact command and time; an approval covers one exact command once, so a replay, a broader command or another task is held again. Added 2026-10-24.")
a("SE-08", "implemented", f"{core}/policy.rs, mcp.rs", "One policy decides every route a tool call can take (the file tool, the shell, and the bridge's own tools), so a denied objective can't be reached another way. Added 2026-10-24.")
# System 11
a("LR-01", "implemented", f"{core}/engine.rs, hub.rs", "Tasks run locally and record changed files and artifacts.")
a("LR-02", "implemented", f"{core}/worker.rs, remote.rs, crates/fermi/src/worker_http.rs", "The same worker runs in a container (fermi worker in ubuntu:24.04); its result is applied and compared with a local run of the same fixture.")
a("LR-03", "implemented", f"{core}/remote.rs (Client, run)", "Tasks dispatch to a worker over authenticated HTTP: a handshake, an idempotent job, then artifacts downloaded and checked against the worker's checksums and the patch applied.")
a("LR-04", "implemented", f"{core}/hub.rs (shutdown, reattach), worker.rs", "Jobs run in the worker, not the window: closing it detaches, and the next window reattaches, catching up from the last event seen.")
a("LR-05", "implemented", f"{core}/remote.rs (follow)", "Events are numbered on the worker; a client that loses its connection waits (up to heartbeat_secs) and resumes after the last event it saw, without resubmitting.")
a("LR-06", "implemented", f"{core}/remote.rs (run, follow), worker.rs (Service::open)", "A worker silent past heartbeat_secs is lost: the attempt is interrupted and retried on the next worker, at most [remote] retries times; only a finished job's result is applied, and a restarted worker marks its lost jobs interrupted.")
a("LR-07", "implemented", f"{core}/worker.rs (cancel), remote.rs (CancelOnDrop)", "Cancelling stops the job's run and every process it started and frees its folder; the controller keeps asking until the worker confirms, so lost acknowledgements are reconciled.")
a("LR-08", "implemented", f"{core}/worker.rs (PROTOCOL, admit), remote.rs (hello)", "Both sides check the worker protocol version before any job runs; incompatible workers and clients are refused with both versions named.")
# System 12
a("EV-01", "implemented", f"{core}/eval.rs (run, run_task)", "Each task runs in a fresh fixture copy through the normal engine and is scored by fermi's evaluator in its own folder; every task gets a score, status and evidence (patch, check outputs); failed runs and no change earn no credit.")
a("EV-02", "implemented", f"{core}/eval.rs (Report)", "Every run produces the same report schema bound to the dataset hash, evaluator and limits, with the model and account, latency, tokens and cost (unknown when no price applies).")
a("EV-03", "implemented", f"{core}/eval.rs (hidden files, scoring folder)", "Hidden tests are laid over a pristine copy only when scoring, so a wrong implementation that passes the visible tests still fails; failure output is kept.")
a("EV-04", "implemented", f"{core}/eval.rs (compare), crates/fermi/src/server.rs (/api/evals), web/index.html (Evaluations)", "Runs of the same dataset version are compared per task and in total (quality, latency, cost), reconciling to the raw records, with missing measurements and the sample size shown and no significance claimed.")
a("EV-05", "implemented", f"{core}/eval.rs (reproducibility, task_hash)", "Reports pin dataset, fixture and evaluator hashes, the configuration fingerprint and limits; two runs are checked for the same setup and scores, and differing outputs are reported as variation.")
a("EV-06", "implemented", f"{core}/eval.rs (protect, lock)", "Protected assets are read-only to the candidate (read-only files and folders on Unix; locked files and a deny ACL on Windows), hashed before and after, and scoring uses pristine copies; any change invalidates the task.")
a("EV-07", "implemented", f"{core}/eval.rs (register, history)", "Datasets are registered by version and content hash; reports stay bound to theirs, changed contents need a new version, and runs of different versions aren't compared.")
a("EV-08", "implemented", f"{core}/eval.rs (propose, evaluate, adopt)", "An agent's proposal cites the failures it addresses and the evaluation rule; it can't become the active configuration until evaluated without regressions and approved by a named person.")
# E2E
a("E2E-01", "implemented", f"crates/fermi/src/server.rs (/api/tasks), {core}/verify.rs (run_task), trace.rs (export timeline)", "A task submitted from the dashboard is routed, edits files, and is completed only after fermi's own checks pass; the trace links the request, route, model call, edit, each check's command and the tested revision, and the outcome. Added 2026-10-25.")
a("E2E-02", "implemented", f"{core}/workspace.rs, hub.rs (submit, diff, delete), crates/fermi/src/server.rs (isolate, /api/tasks/{{id}}/diff)", "A task can run isolated: its own git worktree and branch from the repository's commit, so tasks started together on one repository can't touch each other's files; its changes stay there for review and go away with the task. Added 2026-10-25.")
a("E2E-03", "implemented", f"{core}/browser.rs, mcp.rs (browser tools), hub.rs", "An agent writes a page, opens it in fermi's headless browser through the bridge, tries invalid and valid input and saves a screenshot; independent interaction tests then pass on the same file. Added 2026-10-25.")
a("E2E-04", "implemented", f"{core}/engine.rs (hand_over), trace.rs (Ops), providers/*.rs (RunResult.tools)", "When a call fails over after running tools, the work stays and the next account is told which tool calls already ran and what the folder holds, so it continues instead of repeating side effects; attempts stay within the profile's accounts and [retry]; if every account fails the task ends failed, never complete. Added 2026-10-25.")
a("E2E-05", "implemented", f"{core}/hub.rs (cancel), providers/proc.rs (kill_tree), state.rs (busy), billing.rs (Reservation)", "Cancelling from the dashboard stops the CLI and everything it started, gives back the account's slot and settles the budget reservation; the task stays cancelled. Fixed 2026-10-25: a cancelled call kept its account counted as busy.")
a("E2E-06", "implemented", f"{core}/remote.rs, hub.rs (shutdown, reattach), crates/fermi/src/server.rs (create_task)", "A task sent to a worker from the dashboard keeps running when the window closes; reopening reattaches to the job, restores its events and trace and applies the change once. Fixed 2026-10-25: the dashboard refused tasks for a profile whose accounts are on workers.")
a("E2E-07", "implemented", f"{core}/plugins.rs (scope, log), mcp.rs, sandbox.rs", "A plugin tool called from an agent task with a forbidden path is refused before it runs; the refusal and its reason are in the task's trace and in the plugin log under the task's id; the plugin's own read of the sentinel is stopped by the Linux sandbox. Added 2026-10-25.")
a("E2E-08", "implemented", f"{core}/billing.rs, engine.rs, crates/fermi/src/server.rs", "After an in-flight call settles over its estimate, new calls are refused; the overrun and remaining allowance show in the task and the dashboard. Added 2026-10-24.")
a("E2E-09", "implemented", f"{core}/eval.rs (run, compare), crates/fermi/src/server.rs (/api/evals/compare)", "Two models run the same frozen task at once, each in a fresh folder of its own, scored by the same evaluator; neither sees the other's files, prompts or memory, and the comparison shows model, score, latency and cost. Added 2026-10-25.")
a("E2E-10", "implemented", f"{core}/code.rs (rename_symbol, impact), workspace.rs, verify.rs", "A refactor across the three packages runs as an isolated, verified task: the agent checks callers and impact through the bridge, renames the symbol and its references (leaving unrelated names alone), the checks pass on the exact revision under review, and the full diff is available. Added 2026-10-25.")

missing_audit = [c for c in ids if c not in A]
assert not missing_audit, missing_audit

def level(c):
    p = c["profiles"]
    if c["id"].startswith("E2E"):
        return "e2e"
    if "T3" in p:
        return "security" if c["id"].startswith(("SE", "PL-07", "EV-06")) else "failure-resilience"
    if p == ["T0"]:
        return "unit"
    if "T2" in p and "T1" not in p:
        return "e2e"
    return "integration"

def prof(c):
    m = {"T0": "deterministic", "T1": "deterministic", "T2": "deterministic", "T3": "security", "T4": "live"}
    out = []
    for t in c["profiles"]:
        if m[t] not in out:
            out.append(m[t])
    return out

# ── test mappings (kept from the current registry) and the published run ────
registry = os.path.join(ROOT, "tests", "acceptance", "systems.yaml")
tests, cur = {}, None
if os.path.exists(registry):
    for line in open(registry, encoding="utf8"):
        m = re.match(r"  - id: (\S+)", line)
        if m:
            cur = m.group(1)
        m = re.match(r"    (testFile|testName|repetitions): (\S+)", line)
        if m and m.group(2) != "null":
            tests.setdefault(cur, {})[m.group(1)] = m.group(2)
run_meta = json.load(open(RUN, encoding="utf8")) if RUN else {}
results = {c["id"]: c for c in run_meta.get("cases", [])}

def result(cid):
    r = results.get(cid)
    if r:
        n = len(r["attempts"])
        return r["status"] + (f" ({n}/{n})" if n > 1 and r["status"] == "passed" else "")
    return "not_run" if cid in tests else "not_implemented"

# ── registry ────────────────────────────────────────────────────────────────
os.makedirs(os.path.join(ROOT, "tests", "acceptance"), exist_ok=True)
def q(s):
    return json.dumps(s, ensure_ascii=False)
lines = ["# Acceptance registry: the authoritative inventory of all 106 case IDs.",
         "# Source: docs/FERMI_ADE_Verification_and_Acceptance_Testing (APEX = fermi).",
         "# Generated by tests/acceptance/phase1_audit.py --registry; testFile/testName are null until a test exists.",
         "# status is the latest published result; executionState not_run means the case has no executed run.",
         "schemaVersion: 1", "cases:"]
for c in cases:
    sys_key = next(s for s in SYSTEMS if c["id"].startswith(s[0] + "-"))
    au = A[c["id"]]
    test = tests.get(c["id"], {})
    lines += [
        f"  - id: {c['id']}",
        f"    title: {q(c['title'])}",
        f"    system: {sys_key[1]}",
        "    required: true",
        f"    level: {level(c)}",
        f"    tiers: [{', '.join(c['profiles'])}]",
        f"    profiles: [{', '.join(prof(c))}]",
        f"    fixtures: [{', '.join(c['fixtures'])}]",
        f"    action: {q(c['action'])}",
        f"    expected: {q(c['expected'])}",
        f"    evidence: {q(c['evidence'])}",
        f"    capability: {au['capability']}",
        f"    implementation: {q(au['where'])}",
        f"    auditNote: {q(au['note'])}",
        f"    testFile: {test.get('testFile', 'null')}",
        f"    testName: {test.get('testName', 'null')}",
    ]
    if "repetitions" in test:
        lines.append(f"    repetitions: {test['repetitions']}")
    r = results.get(c["id"])
    lines += [
        f"    executionState: {'completed' if r and r['status'] in ('passed', 'failed') else 'not_run'}",
        f"    status: {r['status'] if r else 'not_implemented'}",
    ]
if WRITE_REGISTRY:
    open(registry, "w", encoding="utf8", newline="\n").write("\n".join(lines) + "\n")

# ── matrix ──────────────────────────────────────────────────────────────────
counts = {}
out = ["# Acceptance matrix", "",
       "Phase 1 audit of fermi (called APEX in the spec) against `docs/FERMI_ADE_Verification_and_Acceptance_Testing`.",
       "",
       "RUNLINE", "",
       "*Result* is the published acceptance outcome: `passed` or `failed` from an executed test with validated evidence; `not_implemented` when no test exists (usually because the capability is missing); `not_run` when a test exists but wasn't in the published run.",
       "*Capability* is an audit finding from reading the code; it is not verification.", "",
       "Capability: **implemented** (the behavior exists end to end), **partial** (some of the expected behavior exists), **missing** (absent).", ""]
summary_rows = []
for key, slug, name in SYSTEMS:
    cs = [c for c in cases if c["id"].startswith(key + "-")]
    n = {s: sum(1 for c in cs if A[c["id"]]["capability"] == s) for s in ("implemented", "partial", "missing")}
    summary_rows.append((name, len(cs), n, sum(1 for c in cs if result(c["id"]).startswith("passed"))))
out += ["## Summary", "", "| System | Cases | Capability implemented | Partial | Missing | Acceptance passed |", "| --- | --- | --- | --- | --- | --- |"]
tot = {"implemented": 0, "partial": 0, "missing": 0}
passed_total = 0
for name, total, n, passed in summary_rows:
    out.append(f"| {name} | {total} | {n['implemented']} | {n['partial']} | {n['missing']} | {passed} / {total} |")
    passed_total += passed
    for k in tot:
        tot[k] += n[k]
out += [f"| **Total** | **106** | **{tot['implemented']}** | **{tot['partial']}** | **{tot['missing']}** | **{passed_total} / 106** |", ""]
for key, slug, name in SYSTEMS:
    cs = [c for c in cases if c["id"].startswith(key + "-")]
    out += [f"## {name}", "", "| ID | Case | Tiers | Result | Test | Capability | Where | Audit note |", "| --- | --- | --- | --- | --- | --- | --- | --- |"]
    for c in cs:
        au = A[c["id"]]
        cell = lambda s: (s or "—").replace("|", "\\|")
        out.append(f"| {c['id']} | {cell(c['title'])} | {' '.join(c['profiles'])} | **{result(c['id'])}** | {cell(tests.get(c['id'], {}).get('testName'))} | {au['capability']} | {cell(au['where'])} | {cell(au['note'])} |")
    out.append("")
if run_meta:
    runline = (f"Published run `{run_meta['runId']}` on revision `{(run_meta.get('commitSha') or '?')[:12]}`"
               f"{' (dirty tree)' if run_meta.get('dirtyTree') else ''}, profile {run_meta.get('profile')}, {run_meta.get('os')}. "
               f"Full report: `docs/acceptance-runs/{run_meta['runId']}/REPORT.md`.")
else:
    runline = "No published acceptance run yet."
text = "\n".join(out).replace("RUNLINE", runline)
open(os.path.join(ROOT, "docs", "ACCEPTANCE_MATRIX.md"), "w", encoding="utf8", newline="\n").write(text + "\n")
print("cases", len(cases), tot)
