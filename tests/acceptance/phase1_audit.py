"""Phase 1 audit: parse the acceptance catalog and join it with the repository audit.

Writes tests/acceptance/systems.yaml (the registry) and docs/ACCEPTANCE_MATRIX.md.
"""
import re, sys, os, json

ROOT = sys.argv[1]
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
a("AI-01", "partial", f"{core}/providers/*.rs, events.rs", "Five adapters (Claude/Codex/Gemini CLIs, Anthropic and OpenAI APIs) normalize into one Event stream and RunResult. No deterministic provider simulator exists; CLI adapters need fake CLI binaries via [bins], API adapters a local server via base_url.")
a("AI-02", "implemented", f"{core}/engine.rs (Task, Engine::run)", "Every provider runs through Task and Engine::run with the same lifecycle and Event schema. Unverified against a fixture oracle.", "router::tests::*")
a("AI-03", "implemented", f"{core}/hub.rs (follow_up, continuation)", "Model switch per reply; same provider resumes the session, other providers get the transcript; Routed events attribute each turn.", "hub::tests::switching_models")
a("AI-04", "missing", f"{core}/providers/api.rs, engine.rs", "429 maps to Limited (Retry-After read) and fails over to the next account; there is no backoff/retry policy or attempt ceiling.")
a("AI-05", "partial", f"{core}/engine.rs (run_inner), providers/api.rs (send)", "HTTP 5xx is retryable: the task moves to the profile's next account, then the next candidate model, and fails with the last error when all are exhausted. This fallback is always on (not explicitly configured or disableable) and the switch is recorded as a Failover event. CLI adapters classify errors from text.")
a("AI-06", "missing", f"{core}/models.rs (Cap)", "Capabilities cover code/reasoning/general/fast/image; tool calling is not modeled or validated before dispatch.")
a("AI-07", "missing", "", "No budget or reservation ledger.")
a("AI-08", "partial", f"{core}/providers/api.rs (base_url)", "API accounts accept base_url (OpenAI-compatible endpoint), untested against a local model server; uses the Responses API, which local servers may not support.")
# System 2
a("MA-01", "implemented", f"{core}/hub.rs (submit/launch), team.rs (schedule)", "Tasks run concurrently; team runs start up to N agents in parallel, each in its own worktree.")
a("MA-02", "implemented", f"{core}/team.rs (schedule, topo)", "Dependencies gate starts; failed or interrupted tasks block dependents (run goes to attention); cancel stops the run.", "team::tests::plans_parse_and_order")
a("MA-03", "implemented", f"{core}/team.rs (start_task, integrate)", "Separate worktrees and branches merged with --no-ff into an integration branch; manually verified on a fixture (2 runs).")
a("MA-04", "partial", f"{core}/team.rs (integrate)", "Conflicts are detected (unmerged paths) and logged with both branches kept, but resolution is delegated automatically to a debugger agent rather than escalated to the user. Manually verified once.")
a("MA-05", "missing", "", "No delegate tool; agents cannot spawn scoped subtasks.")
a("MA-06", "partial", f"{core}/hub.rs, team.rs (on_done)", "A failed agent marks only its task failed and others continue; untested with a killed process.")
a("MA-07", "partial", f"{core}/team.rs (cancel), providers/proc.rs (kill_on_drop)", "Cancel aborts running agents (child processes killed on drop) and queued tasks never start; grandchildren of the CLI are not tracked.")
a("MA-08", "partial", f"{core}/team.rs (integrate, merge)", "Merge into the user's branch requires approval and the final checks run on the integration branch, but checks are agent judgments: there is no mechanical verification command gating the merge.")
# System 3
a("CI-01", "partial", f"{core}/code.rs (Index::open/update)", "Indexes Rust, Python, JS, TS/TSX, Go with .gitignore; unsupported files are skipped silently instead of being listed as unsupported.", "code::tests::rust_project_graph, python_and_typescript")
a("CI-02", "implemented", f"{core}/code.rs (search, symbol)", "Exact and prefix lookup with file and line span.", "code::tests::*")
a("CI-03", "implemented", f"{core}/code.rs (graph callers)", "Static callers resolved same-file, then imported files; the text output declares that dynamic calls may be missing.", "code::tests::rust_project_graph")
a("CI-04", "missing", "", "No semantic rename / edit API.")
a("CI-05", "implemented", f"{core}/code.rs (update)", "Incremental re-parse by mtime and size on every query; no completion event.", "code::tests::rust_project_graph (incremental assertion)")
a("CI-06", "missing", f"{core}/code.rs", "tree-sitter parses error-tolerantly; syntax errors are not surfaced and results are not marked stale.")
a("CI-07", "implemented", f"{core}/code.rs (impact)", "Transitive callers and importers with visited sets (cycles terminate) and tests to run; unsupported relationships are not explicitly disclosed.", "code::tests::rust_project_graph")
a("CI-08", "partial", f"{core}/code.rs (find, resolution)", "Duplicate names resolve per importing file; lookup by Type::name or path:line, but not by module-qualified path (crate::a::helper).")
# System 4
for i in range(1, 9):
    a(f"PM-0{i}", "missing", "", "fermi has no project memory store (only conversation history in ~/.fermi/history).")
# System 5
a("VE-01", "missing", "", "No verification service; agents run compilers themselves.")
a("VE-02", "missing", "", "No verification tool; test output is only what the agent reports.")
a("VE-03", "missing", "", "No repair-and-rerun loop owned by fermi.")
a("VE-04", "missing", "", "No lint integration.")
a("VE-05", "missing", "", "No dependency advisory scanner.")
a("VE-06", "missing", "", "No structured (syntax-tree) edit API.")
a("VE-07", "missing", "", "No bounded repair attempts.")
a("VE-08", "missing", f"{core}/team.rs (merge)", "No completion gate tied to mandatory checks (team merge needs user approval only).")
# System 6
for i in range(1, 9):
    a(f"BA-0{i}", "missing", "crates/fermi/src/preview.rs (user-facing only)", "The Preview tab is a browser for the user; agents have no browser automation tools in fermi.")
# System 7
a("PL-01", "missing", "", "No plugin manager or manifest format.")
a("PL-02", "missing", "", "No plugin manager.")
a("PL-03", "missing", "", "No command registry.")
a("PL-04", "missing", "", "No plugin host.")
a("PL-05", "partial", f"{core}/mcp.rs", "fermi is itself an MCP server (bridge tools); connecting fermi to an external MCP server is left to the CLIs' own configs.")
a("PL-06", "partial", f"{core}/providers/mod.rs, config.rs ([[models]], bins)", "Models and CLI binaries can be overridden in config; no pluggable adapter interface.")
a("PL-07", "missing", "", "No plugin sandbox.")
a("PL-08", "missing", "", "No plugin API versioning.")
# System 8
a("OB-01", "partial", f"{core}/hub.rs (TaskRec.events)", "Tool calls and turns are traced; individual model calls inside a CLI run are not visible.")
a("OB-02", "partial", f"{core}/events.rs (Tool)", "Tool name and a one-line input summary only; no outputs, duration or status.")
a("OB-03", "partial", f"{core}/state.rs (UsageRecord), providers/claude_cli.rs", "Input (incl. cache), output tokens and CLI-reported cost recorded per run; cached tokens are folded into input, unknown fields recorded as 0.")
a("OB-04", "partial", f"{core}/events.rs (Done, Failover, Blocked)", "Run failures and blocked approvals recorded; per-tool failures are not.")
a("OB-05", "implemented", f"{core}/hub.rs (history files)", "Every task's events persist in ~/.fermi/history/<id>.json and survive restarts.", "hub::tests::scanner_sees_other_writers_and_stale_tasks")
a("OB-06", "partial", f"{core}/hub.rs (follow_up)", "Follow-ups keep the task id; there is no attempt id or durable checkpoint resume.")
a("OB-07", "missing", "", "No secret redaction before persistence.")
a("OB-08", "missing", "", "No captured-response replay.")
# System 9
a("CO-01", "missing", "", "Cost comes from the CLI's own total_cost_usd; no price tables.")
a("CO-02", "missing", "", "No cached-input pricing.")
a("CO-03", "partial", f"{core}/router.rs", "Auto routing sends light work to tier 1 models; no explicit economy policy with costs.", "router::tests::auto_routes_by_need")
a("CO-04", "partial", f"{core}/router.rs", "Heavy work goes to tier 3; no declared quality ranking policy.", "router::tests::auto_routes_by_need")
a("CO-05", "missing", "", "No shared budget reservations.")
a("CO-06", "missing", "", "No versioned price tables.")
a("CO-07", "missing", "", "No context-size estimation or warnings.")
a("CO-08", "partial", f"{core}/state.rs (usage.jsonl)", "Usage logged per run by profile, account and model; not grouped by team run, parent or child task.")
# System 10
a("SE-01", "missing", "", "fermi enforces no file boundary; it relies on each CLI (Codex sandbox, Claude permission modes).")
a("SE-02", "missing", "", "Child processes inherit the full environment.")
a("SE-03", "partial", f"{core}/providers/claude_cli.rs (permission_denials), hub.rs (allow)", "Claude in acceptEdits/default modes blocks commands that need approval; fermi surfaces them and continues only after the user allows them.")
a("SE-04", "missing", "", "No network policy.")
a("SE-05", "partial", f"{core}/team.rs (worktrees)", "Team agents get separate worktrees, but no OS-level sandbox or secret separation.")
a("SE-06", "missing", "", "No provenance or untrusted-content policy.")
a("SE-07", "partial", f"{core}/hub.rs (FollowUp.allow), team.rs", "Approvals are scoped to tool rules (Bash(node:*)) per conversation and stored; no actor, timestamp or replay protection.")
a("SE-08", "missing", "", "No shared policy across entry points.")
# System 11
a("LR-01", "implemented", f"{core}/engine.rs, hub.rs", "Tasks run locally and record changed files and artifacts.")
a("LR-02", "missing", "", "No container workers.")
a("LR-03", "missing", "", "No remote workers.")
a("LR-04", "missing", f"{core}/hub.rs", "Tasks run inside the UI process and are marked interrupted when it closes.")
a("LR-05", "partial", "crates/fermi/web/index.html (WebSocket reconnect), hub.rs (Scanner)", "Dashboard clients reconnect and re-sync from the shared history; no worker-side event log.")
a("LR-06", "missing", "", "No remote workers or heartbeats beyond local stale detection.")
a("LR-07", "missing", "", "No remote cancellation.")
a("LR-08", "missing", "", "No worker protocol versioning.")
# System 12
for i in range(1, 9):
    a(f"EV-0{i}", "missing", "", "No evaluation harness, datasets or scoring.")
# E2E
a("E2E-01", "partial", "dashboard + engine", "Task, model routing and file changes work; no fermi-run test step or verified-change gate.")
a("E2E-02", "partial", f"{core}/team.rs", "Team runs isolate agents in worktrees; plain parallel tasks share the workspace folder.")
a("E2E-03", "missing", "", "Needs agent browser control (System 6).")
a("E2E-04", "partial", f"{core}/engine.rs", "Limits and auth failures restart on the next account; mid-run outage has no bounded retry or checkpoint.")
a("E2E-05", "partial", f"{core}/hub.rs (cancel)", "Cancel aborts the run and kills the CLI process; grandchildren are not tracked.")
a("E2E-06", "missing", "", "Needs remote execution (System 11).")
a("E2E-07", "missing", "", "Needs a plugin sandbox (System 7).")
a("E2E-08", "missing", "", "Needs budgets (System 9).")
a("E2E-09", "partial", f"{core}/team.rs, hub.rs", "Two models can be run separately; no paired comparison with isolation and a shared oracle.")
a("E2E-10", "partial", f"{core}/code.rs, team.rs", "Impact analysis and agent edits exist; no mandatory-check gate on the final revision.")

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

# ── registry ────────────────────────────────────────────────────────────────
os.makedirs(os.path.join(ROOT, "tests", "acceptance"), exist_ok=True)
def q(s):
    return json.dumps(s, ensure_ascii=False)
lines = ["# Acceptance registry: the authoritative inventory of all 106 case IDs.",
         "# Source: docs/FERMI_ADE_Verification_and_Acceptance_Testing (APEX = fermi).",
         "# Generated by the Phase 1 audit; testFile/testName are null until a test exists.",
         "# status is the latest published result; executionState not_run means no acceptance run yet.",
         "schemaVersion: 1", "cases:"]
for c in cases:
    sys_key = next(s for s in SYSTEMS if c["id"].startswith(s[0] + "-"))
    au = A[c["id"]]
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
        "    testFile: null",
        "    testName: null",
        "    executionState: not_run",
        "    status: not_implemented",
    ]
open(os.path.join(ROOT, "tests", "acceptance", "systems.yaml"), "w", encoding="utf8", newline="\n").write("\n".join(lines) + "\n")

# ── matrix ──────────────────────────────────────────────────────────────────
counts = {}
out = ["# Acceptance matrix", "",
       "Phase 1 audit of fermi (called APEX in the spec) against `docs/FERMI_ADE_Verification_and_Acceptance_Testing`.",
       "",
       "Audited revision: `REV` (working tree clean apart from `docs/`). Date: 2026-10-23.", "",
       "**No acceptance test exists yet, so every case's published status is `not_implemented` (execution state `not_run`).**",
       "The *capability* column is an audit finding from reading the code and from earlier manual runs; it is not verification.",
       "Related unit tests are listed for reference; they exercise pieces of a capability but are not acceptance evidence.", "",
       "Capability: **implemented** (the behavior exists end to end), **partial** (some of the expected behavior exists), **missing** (absent).", ""]
summary_rows = []
for key, slug, name in SYSTEMS:
    cs = [c for c in cases if c["id"].startswith(key + "-")]
    n = {s: sum(1 for c in cs if A[c["id"]]["capability"] == s) for s in ("implemented", "partial", "missing")}
    summary_rows.append((name, len(cs), n))
out += ["## Summary", "", "| System | Cases | Implemented | Partial | Missing | Acceptance passed |", "| --- | --- | --- | --- | --- | --- |"]
tot = {"implemented": 0, "partial": 0, "missing": 0}
for name, total, n in summary_rows:
    out.append(f"| {name} | {total} | {n['implemented']} | {n['partial']} | {n['missing']} | 0 / {total} |")
    for k in tot:
        tot[k] += n[k]
out += [f"| **Total** | **106** | **{tot['implemented']}** | **{tot['partial']}** | **{tot['missing']}** | **0 / 106** |", ""]
for key, slug, name in SYSTEMS:
    cs = [c for c in cases if c["id"].startswith(key + "-")]
    out += [f"## {name}", "", "| ID | Case | Tiers | Fixtures | Capability | Where | Audit note | Related unit tests |", "| --- | --- | --- | --- | --- | --- | --- | --- |"]
    for c in cs:
        au = A[c["id"]]
        cell = lambda s: (s or "—").replace("|", "\\|")
        out.append(f"| {c['id']} | {cell(c['title'])} | {' '.join(c['profiles'])} | {' '.join(c['fixtures'])} | {au['capability']} | {cell(au['where'])} | {cell(au['note'])} | {cell(au['related_tests'])} |")
    out.append("")
rev = os.popen(f'git -C "{ROOT}" rev-parse --short HEAD').read().strip()
text = "\n".join(out).replace("REV", rev)
open(os.path.join(ROOT, "docs", "ACCEPTANCE_MATRIX.md"), "w", encoding="utf8", newline="\n").write(text + "\n")
print("cases", len(cases), tot)
