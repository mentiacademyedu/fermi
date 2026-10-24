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
a("AI-01", "partial", f"{core}/providers/*.rs, events.rs", "Five adapters (Claude/Codex/Gemini CLIs, Anthropic and OpenAI APIs) normalize into one Event stream and RunResult. The acceptance test covers the three CLI adapters through the fake CLI; the two API adapters need an HTTP mock (not built yet), so the case stays partial.")
a("AI-02", "implemented", f"{core}/engine.rs (Task, Engine::run)", "Every provider runs through Task and Engine::run with the same lifecycle and Event schema. Unverified against a fixture oracle.", "router::tests::*")
a("AI-03", "implemented", f"{core}/hub.rs (follow_up, continuation)", "Model switch per reply; same provider resumes the session, other providers get the transcript; Routed events attribute each turn.", "hub::tests::switching_models")
a("AI-04", "missing", f"{core}/providers/api.rs, engine.rs", "429 maps to Limited (Retry-After read) and fails over to the next account; there is no backoff/retry policy or attempt ceiling.")
a("AI-05", "implemented", f"{core}/engine.rs (run_inner), providers/mod.rs (classify_error)", "Provider outages (HTTP 5xx from the APIs; 5xx, overloaded or unavailable from the CLIs) fail over to the profile's next account, then the next candidate model, recorded as Failover events; pinning the account and model disables it. Fixed 2026-10-23: CLI outages failed without fallback (D-004).")
a("AI-06", "missing", f"{core}/models.rs (Cap)", "Capabilities cover code/reasoning/general/fast/image; tool calling is not modeled or validated before dispatch.")
a("AI-07", "implemented", f"{core}/billing.rs (Ledger), engine.rs", "Profiles with a budget reserve each call's estimate atomically in a shared ledger before dispatch; a call that doesn't fit is refused with the numbers behind it; actual costs settle later and overruns stay visible. Added 2026-10-24.")
a("AI-08", "partial", f"{core}/providers/api.rs (base_url)", "API accounts accept base_url (OpenAI-compatible endpoint), untested against a local model server; uses the Responses API, which local servers may not support.")
# System 2
a("MA-01", "implemented", f"{core}/hub.rs (submit/launch), team.rs (schedule)", "Tasks run concurrently; team runs start up to N agents in parallel, each in its own worktree.")
a("MA-02", "implemented", f"{core}/team.rs (schedule, topo)", "Dependencies gate starts; failed or interrupted tasks block dependents (run goes to attention); cancel stops the run.", "team::tests::plans_parse_and_order")
a("MA-03", "implemented", f"{core}/team.rs (start_task, integrate)", "Separate worktrees and branches merged with --no-ff into an integration branch; manually verified on a fixture (2 runs).")
a("MA-04", "partial", f"{core}/team.rs (integrate)", "Conflicts are detected (unmerged paths) and logged with both branches kept, but resolution is delegated automatically to a debugger agent rather than escalated to the user. Manually verified once.")
a("MA-05", "missing", "", "No delegate tool; agents cannot spawn scoped subtasks.")
a("MA-06", "implemented", f"{core}/hub.rs, team.rs (on_done)", "A killed agent process marks only its task failed; concurrent tasks finish with their results. In team runs a failed task blocks its dependents (MA-02).")
a("MA-07", "implemented", f"{core}/team.rs (cancel), providers/proc.rs (kill_tree)", "Cancel stops running agents and every process they started (process tree), and queued tasks never start. Fixed 2026-10-23: processes started by the CLI were orphaned (D-005).")
a("MA-08", "implemented", f"{core}/team.rs (start_verification, merge)", "The integrated branch must pass the project's mandatory checks before merging; a failing run gets bounded debugger fixes and re-checks, and merge refuses anything but the exact commit that passed. Added 2026-10-23.")
# System 3
a("CI-01", "implemented", f"{core}/code.rs (Index::open/update)", "Indexes Rust, Python, JS, TS/TSX, Go with .gitignore; lists unsupported files and the indexed git revision (added 2026-10-23, D-006).", "code::tests::rust_project_graph, python_and_typescript")
a("CI-02", "implemented", f"{core}/code.rs (search, symbol)", "Exact and prefix lookup with file and line span.", "code::tests::*")
a("CI-03", "implemented", f"{core}/code.rs (graph callers)", "Static callers resolved same-file, then imported files; results declare the analysis scope (dynamic dispatch is not covered; added 2026-10-23, D-006).", "code::tests::rust_project_graph")
a("CI-04", "missing", "", "No semantic rename / edit API.")
a("CI-05", "implemented", f"{core}/code.rs (update)", "Incremental re-parse by mtime and size on every query; no completion event.", "code::tests::rust_project_graph (incremental assertion)")
a("CI-06", "missing", f"{core}/code.rs", "tree-sitter parses error-tolerantly; syntax errors are not surfaced and results are not marked stale.")
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
a("OB-03", "partial", f"{core}/state.rs (UsageRecord), billing.rs", "Input, cached and cache-write tokens, output tokens, the CLI-reported cost and the price-table cost (with version and breakdown) are recorded per call; per-model-call detail inside a CLI run isn't visible.")
a("OB-04", "partial", f"{core}/events.rs (Done, Failover, Blocked)", "Run failures and blocked approvals recorded; per-tool failures are not.")
a("OB-05", "implemented", f"{core}/hub.rs (history files)", "Every task's events persist in ~/.fermi/history/<id>.json and survive restarts.", "hub::tests::scanner_sees_other_writers_and_stale_tasks")
a("OB-06", "partial", f"{core}/hub.rs (follow_up)", "Follow-ups keep the task id; there is no attempt id or durable checkpoint resume.")
a("OB-07", "missing", "", "No secret redaction before persistence.")
a("OB-08", "missing", "", "No captured-response replay.")
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
a("E2E-08", "implemented", f"{core}/billing.rs, engine.rs, crates/fermi/src/server.rs", "After an in-flight call settles over its estimate, new calls are refused; the overrun and remaining allowance show in the task and the dashboard. Added 2026-10-24.")
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
