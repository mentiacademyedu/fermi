//! End-to-end workflows (E2E-01 … E2E-10, but E2E-08, which is in `cost.rs`):
//! each runs through the dashboard's HTTP API, as the user's window drives
//! fermi, with the real hub, engine, verification, browser, plugins and
//! workers; only the model's output comes from the fake CLI.

use acceptance_tests::provider::egress_recorder;
use acceptance_tests::workers::{jobs, start_worker, until, workers_toml};
use acceptance_tests::{account, alive, fixture, git, repo_root, sha256, wait_file, Case, Fermi};
use fermi_core::engine::Engine;
use fermi_core::events::Event;
use fermi_core::hub::Hub;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(600);

// ── the dashboard, as the user's window ─────────────────────────────────────

struct Dash {
    base: String,
    token: String,
    hub: Arc<Hub>,
    http: reqwest::Client,
    stop: tokio::sync::oneshot::Sender<()>,
    serving: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Dash {
    /// Opens the dashboard on `engine` (a window, or the desktop app).
    async fn open(engine: Arc<Engine>, cwd: &Path) -> Dash {
        let server = fermi::server::start(engine, 0, cwd.to_path_buf(), "acceptance").await.unwrap();
        let (base, token) = server.url.split_once("/#t=").map(|(b, t)| (b.to_string(), t.to_string())).unwrap();
        let hub = server.hub.clone();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(server.serve(async {
            let _ = stopped.await;
        }));
        Dash { base, token, hub, http: reqwest::Client::new(), stop, serving }
    }

    async fn get(&self, path: &str) -> Value {
        let r = self.http.get(format!("{}{path}", self.base)).header("x-fermi-token", &self.token).send().await.unwrap();
        r.json().await.unwrap_or(Value::Null)
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self.http.post(format!("{}{path}", self.base)).header("x-fermi-token", &self.token).json(&body).send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    /// Submits a task as the new-task form does.
    async fn create(&self, body: Value) -> u64 {
        let (code, v) = self.post("/api/tasks", body).await;
        v["id"].as_u64().unwrap_or_else(|| panic!("task not created (HTTP {code}): {v}"))
    }

    /// The task as the window shows it, once it has finished.
    async fn wait(&self, id: u64) -> Value {
        let start = Instant::now();
        loop {
            let t = self.get(&format!("/api/tasks/{id}")).await;
            if t["status"] != "running" || start.elapsed() > T {
                return t;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Closes the window (the server stops and its hub shuts down).
    async fn close(self) {
        let _ = self.stop.send(());
        let _ = self.serving.await;
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap_or_default()
}

fn write(dir: &Path, rel: &str, text: &str) {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// Replaces `old` (which must occur once) in a file.
fn edit(dir: &Path, rel: &str, old: &str, new: &str) {
    let s = read(dir, rel);
    assert_eq!(s.matches(old).count(), 1, "{old:?} in {rel}");
    write(dir, rel, &s.replace(old, new));
}

fn commit(dir: &Path, msg: &str) {
    git(dir, &["add", "-A"]).unwrap();
    git(dir, &["commit", "-q", "-m", msg]).unwrap();
}

fn greet() -> String {
    std::fs::read_to_string(repo_root().join("tests/fixtures/F-CODE/expected/engine-lib-greet.rs")).unwrap()
}

/// `cargo test` run by the test itself, outside fermi (its own target folder).
fn cargo_test(dir: &Path, target: &Path) -> (bool, String) {
    let o = std::process::Command::new("cargo")
        .args(["test", "--offline", "--workspace"])
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target)
        .output()
        .unwrap();
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

/// The model calls an account received (the dashboard's login check sends no prompt).
fn prompts(f: &Fermi, account: &str) -> Vec<Value> {
    f.calls(account).into_iter().filter(|c| c["prompt"].as_str().is_some_and(|p| !p.is_empty())).collect()
}

fn events_of<'a>(task: &'a Value, kind: &str) -> Vec<&'a Value> {
    task["events"].as_array().into_iter().flatten().filter(|e| e["type"] == kind).collect()
}

/// Trace operations of one tool name.
fn ops<'a>(trace: &'a Value, name: &str) -> Vec<&'a Value> {
    trace["operations"].as_array().into_iter().flatten().filter(|o| o["kind"] == "tool" && o["name"] == name).collect()
}

fn timeline_kinds(trace: &Value) -> Vec<String> {
    trace["timeline"].as_array().into_iter().flatten().filter_map(|e| e["kind"].as_str().map(str::to_string)).collect()
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn lines(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Every file under `dir`, as text (for canary scans).
fn all_text(dir: &Path) -> Vec<(String, String)> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok()) {
        let p = e.path();
        if p.is_dir() {
            out.extend(all_text(&p));
        } else {
            out.push((p.display().to_string(), String::from_utf8_lossy(&std::fs::read(&p).unwrap_or_default()).into_owned()));
        }
    }
    out
}

// ── E2E-01 ──────────────────────────────────────────────────────────────────

/// The protected test for E2E-01's requested behavior.
const PROTECTED_GREET: &str =
    "#[test]\nfn greets_by_the_normalized_name() {\n    assert_eq!(engine::greet(\" Fermi \"), \"hello fermi\");\n}\n";

#[tokio::test(flavor = "multi_thread")]
async fn e2e_01_coding_task_to_tested_change() {
    let mut c = Case::start("E2E-01", "T2");
    c.real(&["dashboard HTTP API (axum)", "hub task service", "router", "verification (cargo check, cargo test)", "trace export", "git"]);
    c.mocked(&["model output (fake-cli)"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    write(&fx.path, "engine/tests/greet.rs", PROTECTED_GREET);
    edit(&fx.path, ".fermi/checks.toml", "max_repairs = 3", "max_repairs = 1\nprotect = [\"engine/tests/**\"]");
    commit(&fx.path, "a protected test for greet");
    let base = git(&fx.path, &["rev-parse", "HEAD"]).unwrap();
    let step = json!({ "match": "[[E2E-01]]",
        "calls": [{ "id": "msg_e1", "usage": { "input_tokens": 1200, "output_tokens": 300 }, "text": "Adding greet to the engine.", "tools": [
            { "id": "t_read", "name": "Read", "input": { "file_path": "engine/src/lib.rs" }, "output": "(the file)" },
            { "id": "t_edit", "name": "Edit", "input": { "file_path": "engine/src/lib.rs" }, "output": "edited" } ] }],
        "edits": [{ "path": "engine/src/lib.rs", "content": greet() }], "text": "Added greet() to the engine." });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step])], "").unwrap();
    let ui = Dash::open(f.engine.clone(), &fx.path).await;
    let prompt = "Add a greet(name) function to the engine crate that says hello to the normalized name. [[E2E-01]]";
    let id = ui.create(json!({ "prompt": prompt, "cwd": fx.path, "profile": "p", "verify": { "max_repairs": 1 } })).await;
    let task = ui.wait(id).await;
    let trace = ui.get(&format!("/api/tasks/{id}/trace")).await;
    ui.close().await;
    c.json("ui-task.json", &task);
    c.json("trace.json", &trace);
    let patch = git(&fx.path, &["diff", "HEAD"]).unwrap();
    c.text("patch.diff", &patch);

    let calls = prompts(&f, "cl");
    c.json("model-requests.json", &json!(calls));
    let route = trace["timeline"].as_array().into_iter().flatten().find(|e| e["kind"] == "route").cloned().unwrap_or_default();
    c.check(
        "modelSelected",
        route["account"] == "cl"
            && route["model"].as_str().is_some_and(|m| !m.is_empty())
            && route["why"].as_str().is_some_and(|w| !w.is_empty())
            && calls.len() == 1
            && calls[0]["prompt"].as_str().is_some_and(|p| p.contains(prompt)),
        format!(
            "routed to {} on {} ({}); {} model request(s) carrying the task",
            route["model"],
            route["account"],
            route["why"],
            calls.len()
        ),
    );
    let status = git(&fx.path, &["status", "--porcelain"]).unwrap();
    c.check(
        "intendedChange",
        read(&fx.path, "engine/src/lib.rs") == greet()
            && status.trim() == "M engine/src/lib.rs"
            && read(&fx.path, "engine/tests/greet.rs") == PROTECTED_GREET,
        format!("changed: {}; the protected test is unchanged", status.trim()),
    );
    let (ok, out) = cargo_test(&fx.path, &c.work.join("oracle-target"));
    c.text("oracle-cargo-test.txt", &out);
    c.check(
        "independentTestsPass",
        ok && out.contains("greets_by_the_normalized_name ... ok") && out.contains("greet_uses_the_normalized_name ... ok"),
        "cargo test, run by the test outside fermi, passes, the protected test included",
    );
    // request → route → edit (inside model call msg_e1, recorded when the call closes) → checks → outcome.
    let seq = |kind: &str, pick: &dyn Fn(&Value) -> bool| {
        trace["timeline"].as_array().into_iter().flatten().find(|e| e["kind"] == kind && pick(e)).and_then(|e| e["seq"].as_u64())
    };
    let chain = [
        seq("request", &|e| e["text"].as_str().is_some_and(|t| t.contains("[[E2E-01]]"))),
        seq("route", &|_| true),
        seq("tool", &|e| e["id"] == "t_edit"),
        seq("check", &|e| {
            e["name"] == "tests" && e["status"] == "passed" && e["command"].as_str().is_some_and(|c| c.contains("cargo test"))
        }),
        seq("outcome", &|e| e["ok"] == true),
    ];
    let ordered =
        chain.iter().all(Option::is_some) && chain.windows(2).all(|w| w[0] < w[1]) && seq("model_call", &|e| e["id"] == "msg_e1").is_some();
    let edit_op = ops(&trace, "Edit").first().cloned().cloned().unwrap_or_default();
    let (_, _, now) = fermi_core::verify::fingerprint(&fx.path);
    let tested = trace["tested_revision"]["fingerprint"].as_str().unwrap_or_default().to_string();
    c.check(
        "traceLinksTaskToTestedRevision",
        ordered
            && edit_op["input"]["file_path"] == "engine/src/lib.rs"
            && edit_op["call"] == "msg_e1"
            && trace["tested_revision"]["passed"] == true
            && tested == now
            && tested.starts_with(&base[..12])
            && tested.contains('+'),
        format!("sequence {chain:?}; tested revision {tested} = the final tree {now} (base {} plus the change)", &base[..12]),
    );
    c.check(
        "completedVerified",
        task["status"] == "done" && task["checks"] == true,
        format!("status {}, checks {}", task["status"], task["checks"]),
    );
    c.finish();
}

// ── E2E-02 ──────────────────────────────────────────────────────────────────

const JOBS: [&str; 3] = ["A", "B", "C"];
const ROUNDS: usize = 20;

#[tokio::test(flavor = "multi_thread")]
async fn e2e_02_three_parallel_coding_tasks() {
    let mut c = Case::start("E2E-02", "T2 T3");
    c.real(&["dashboard HTTP API (axum)", "hub task service", "isolated worktrees (git worktree)", "three CLI processes at once"]);
    c.mocked(&["model output (fake-cli)"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let steps: Vec<Value> = JOBS
        .iter()
        .map(|j| {
            json!({ "match": format!("[[{j}]]"), "touch": format!("started-{j}"), "wait_for": "go", "text": format!("Job {j} done."),
                "edits": [
                    { "path": format!("jobs/{j}.txt"), "content": format!("output of job {j}\n") },
                    { "path": format!("SENTINEL-{j}.txt"), "content": format!("sentinel {j}\n") },
                    { "path": "shared.txt", "content": format!("written by job {j}\n") },
                ] })
        })
        .collect();
    let f = Fermi::new(&c, &[account("cl", "claude", steps)], "").unwrap();
    let home = f.account_dir("cl");
    let ui = Dash::open(f.engine.clone(), &fx.path).await;
    let mut manifests = vec![];
    let (mut overlap_ok, mut owned_ok, mut sentinel_ok, mut intended_ok, mut base_ok, mut cleaned_ok) =
        (true, true, true, true, true, true);
    let mut problems: Vec<String> = vec![];
    for round in 1..=ROUNDS {
        let _ = std::fs::remove_file(home.join("go"));
        for j in JOBS {
            let _ = std::fs::remove_file(home.join(format!("started-{j}")));
        }
        let mut ids = vec![];
        for j in JOBS {
            let prompt = format!("Job [[{j}]], round {round}: write your output.");
            ids.push(ui.create(json!({ "prompt": prompt, "cwd": fx.path, "profile": "p", "isolate": true })).await);
        }
        // All three CLIs are running before any may finish: the jobs overlap.
        let all_started = until(Duration::from_secs(60), || JOBS.iter().all(|j| home.join(format!("started-{j}")).exists())).await;
        std::fs::write(home.join("go"), "go").unwrap();
        let mut round_jobs = vec![];
        for (j, id) in JOBS.iter().zip(&ids) {
            let task = ui.wait(*id).await;
            let diff = ui.get(&format!("/api/tasks/{id}/diff")).await;
            let wt = PathBuf::from(diff["path"].as_str().unwrap_or_default());
            let files: Vec<String> = diff["files"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| format!("{} {}", f["status"], f["path"].as_str().unwrap_or("")))
                .collect();
            let mut want = vec![format!("\"A\" SENTINEL-{j}.txt"), format!("\"A\" jobs/{j}.txt"), "\"A\" shared.txt".to_string()];
            want.sort();
            let mut got = files.clone();
            got.sort();
            let foreign: Vec<&str> =
                JOBS.iter().filter(|o| *o != j).copied().filter(|o| wt.join(format!("SENTINEL-{o}.txt")).exists()).collect();
            let call = prompts(&f, "cl")
                .into_iter()
                .find(|x| x["prompt"].as_str().is_some_and(|p| p.contains(&format!("[[{j}]], round {round}:"))));
            let ran_in = call.as_ref().and_then(|x| x["cwd"].as_str()).map(PathBuf::from).unwrap_or_default();
            let started_ms = call.as_ref().and_then(|x| x["started_ms"].as_u64()).unwrap_or(0);
            let ok_owned = task["status"] == "done"
                && read(&wt, "shared.txt") == format!("written by job {j}\n")
                && read(&wt, &format!("jobs/{j}.txt")) == format!("output of job {j}\n")
                && canon(&ran_in) == canon(&wt);
            if !ok_owned {
                problems.push(format!(
                    "round {round} job {j}: status {}, shared.txt {:?}, ran in {}",
                    task["status"],
                    read(&wt, "shared.txt"),
                    ran_in.display()
                ));
            }
            owned_ok &= ok_owned;
            sentinel_ok &= foreign.is_empty();
            intended_ok &= got == want && diff["base"] == fx.commit.as_str();
            round_jobs.push(json!({ "job": j, "task": id, "status": task["status"], "worktree": wt, "branch": diff["branch"], "base": diff["base"],
                "files": files, "fingerprint": diff["fingerprint"], "cli_started_ms": started_ms, "ended": task["ended"], "foreign_sentinels": foreign }));
        }
        overlap_ok &= all_started;
        // The repository the user started them on is untouched.
        let clean =
            git(&fx.path, &["status", "--porcelain"]).unwrap().is_empty() && git(&fx.path, &["rev-parse", "HEAD"]).unwrap() == fx.commit;
        base_ok &= clean;
        for id in &ids {
            ui.post(&format!("/api/tasks/{id}/delete"), json!({})).await;
        }
        let worktrees = git(&fx.path, &["worktree", "list"]).unwrap().lines().count();
        let branches = git(&fx.path, &["branch", "--list", "fermi/task-*"]).unwrap();
        cleaned_ok &= worktrees == 1 && branches.trim().is_empty();
        manifests.push(json!({ "round": round, "all_started_before_release": all_started, "base_clean": clean, "jobs": round_jobs, "worktrees_after_delete": worktrees }));
    }
    ui.close().await;
    c.json("rounds.json", &json!(manifests));
    c.check(
        "activityOverlaps",
        overlap_ok,
        format!("in all {ROUNDS} rounds the three CLIs were running at the same time before any was let finish"),
    );
    c.check(
        "outputsBelongToJobs",
        owned_ok,
        if problems.is_empty() { format!("{} jobs, each ran and wrote in its own worktree", ROUNDS * 3) } else { problems.join("; ") },
    );
    c.check("noSentinelCrossing", sentinel_ok, "no worktree holds another job's sentinel");
    c.check("onlyIntendedEdits", intended_ok, "each worktree's diff against the shared base is exactly its job's three files");
    c.check("baseRepositoryUntouched", base_ok, "the repository the tasks were started on stayed clean, at the same commit");
    c.check("worktreesRemovedWithTasks", cleaned_ok, "deleting the tasks removed their worktrees and branches");
    c.finish();
}

// ── E2E-03 ──────────────────────────────────────────────────────────────────

/// What the agent builds: a sign-up form that checks its input.
const FORM_HTML: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Sign up</title>
<style>body { font: 16px sans-serif; margin: 2rem } label { display: block; margin: .5rem 0 } .hidden { display: none } #error { color: #b00020 }</style>
</head><body>
<h1>Create your account</h1>
<form id="f" novalidate>
  <label>Name <input id="name"></label>
  <label>Email <input id="email"></label>
  <button type="submit">Sign up</button>
</form>
<p id="error" class="hidden" role="alert"></p>
<p id="result" class="hidden"></p>
<script>
document.getElementById('f').addEventListener('submit', (e) => {
  e.preventDefault();
  const name = document.getElementById('name').value.trim(), email = document.getElementById('email').value.trim();
  const err = document.getElementById('error'), res = document.getElementById('result');
  if (!name || !/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(email)) {
    err.textContent = 'Enter your name and a valid email address.';
    err.classList.remove('hidden');
    res.classList.add('hidden');
    return;
  }
  err.classList.add('hidden');
  res.textContent = 'Thanks, ' + name + '!';
  res.classList.remove('hidden');
});
</script>
</body></html>
"#;

fn file_url(p: &Path) -> String {
    let s = p.display().to_string().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_03_browser_agent_builds_and_tests_ui() {
    let mut c = Case::start("E2E-03", "T2");
    c.real(&[
        "dashboard HTTP API (axum)",
        "hub task service",
        "fermi's MCP bridge (its own process)",
        "browser tools and a headless browser",
        "git",
    ]);
    c.mocked(&["the agent's decisions (fake-cli, calling the bridge's browser tools)"]);
    let project = c.work.join("ui");
    write(&project, "README.md", "A sign-up page.\n");
    git(&project, &["init", "-q", "-b", "main"]).unwrap();
    commit(&project, "start");
    let page = project.join("index.html");
    let url = file_url(&page);
    let mcp = |name: &str, args: Value| json!({ "mcp": { "name": name, "arguments": args } });
    let step = json!({ "match": "[[E2E-03]]", "text": "Built the form and tried it in the browser.",
        "edits": [{ "path": "index.html", "content": FORM_HTML }],
        "ops": [
            mcp("browser_open", json!({ "url": url, "width": 800, "height": 600 })),
            mcp("browser_fill", json!({ "selector": "#name", "value": "Ada" })),
            mcp("browser_fill", json!({ "selector": "#email", "value": "not-an-email" })),
            mcp("browser_click", json!({ "text": "Sign up" })),
            mcp("browser_read", json!({ "selector": "#error" })),
            mcp("browser_fill", json!({ "selector": "#email", "value": "ada@example.invalid" })),
            mcp("browser_click", json!({ "text": "Sign up" })),
            mcp("browser_wait", json!({ "selector": "#result", "state": "visible" })),
            mcp("browser_read", json!({ "selector": "#result" })),
            mcp("browser_screenshot", json!({ "name": "final" })),
            mcp("browser_close", json!({})),
        ] });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step])], "").unwrap();
    if fermi_core::browser::find(&f.engine.cfg.browser).is_none() {
        return c.block("no Chrome, Edge or Chromium on this machine");
    }
    let ui = Dash::open(f.engine.clone(), &project).await;
    let id = ui
        .create(json!({ "prompt": "Build a sign-up page (index.html) with name and email; reject an invalid email with an error, thank the user otherwise. Open it in the browser, try bad and good input, and take a screenshot. [[E2E-03]]", "cwd": project, "profile": "p" }))
        .await;
    let task = ui.wait(id).await;
    let trace = ui.get(&format!("/api/tasks/{id}/trace")).await;
    ui.close().await;
    let delivered = std::fs::read(&page).unwrap_or_default();
    let delivered_at = std::fs::metadata(&page).and_then(|m| m.modified()).ok();
    let (_, _, rev_after_task) = fermi_core::verify::fingerprint(&project);
    c.json("ui-task.json", &task);
    c.json("trace.json", &trace);
    c.text("delivered/index.html", &String::from_utf8_lossy(&delivered));

    // The agent's browser session: its trace and screenshot.
    let session = fermi_core::browser::session_dir(&f.home, &project).parent().unwrap().join(id.to_string());
    let btrace: Vec<Value> = lines(&session.join("trace.jsonl"));
    c.text("browser/trace.jsonl", &std::fs::read_to_string(session.join("trace.jsonl")).unwrap_or_default());
    let shot = std::fs::read_dir(&session)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "png") && p.file_name().is_some_and(|n| n.to_string_lossy().contains("final")));
    if let Some(s) = &shot {
        let bytes = std::fs::read(s).unwrap_or_default();
        let _ = std::fs::create_dir_all(c.dir.join("browser"));
        let _ = std::fs::copy(s, c.dir.join("browser").join("final.png"));
        c.text("browser/final.png.sha256", &sha256(&bytes));
    }
    let actions = |a: &str| btrace.iter().filter(|e| e["kind"] == "action" && e["action"] == a).count();
    c.check(
        "codeDelivered",
        task["status"] == "done"
            && delivered == FORM_HTML.as_bytes()
            && git(&project, &["status", "--porcelain"]).unwrap().trim() == "?? index.html",
        format!("task {}; index.html is new ({} bytes)", task["status"], delivered.len()),
    );
    c.check(
        "agentDroveTheBrowser",
        actions("open") == 1 && actions("fill") == 3 && actions("click") == 2 && shot.is_some(),
        format!(
            "{} open, {} fills, {} clicks, screenshot {:?}",
            actions("open"),
            actions("fill"),
            actions("click"),
            shot.as_ref().map(|s| s.display().to_string())
        ),
    );
    let reads: Vec<String> =
        ops(&trace, "mcp__fermi__browser_read").iter().map(|o| o["output"].as_str().unwrap_or("").to_string()).collect();
    c.check(
        "agentSawBothOutcomes",
        reads.len() == 2 && reads[0].contains("valid email address") && reads[1].contains("Thanks, Ada!"),
        format!("what the agent read back: {reads:?}"),
    );

    // The protected interaction tests: the test's own browser, the delivered page, fixed viewport.
    let oracle = async {
        let b = fermi_core::browser::Browser::launch(&f.engine.cfg.browser, &c.work.join("oracle-browser"), f.engine.redactor.clone()).await?;
        b.viewport(800, 600, false).await?;
        b.open(&url).await?;
        let mut results = vec![];
        for (name, email, want_error) in [("Grace", "grace@", true), ("", "grace@example.invalid", true), ("Grace", "grace@example.invalid", false)] {
            b.fill("#name", name, None).await?;
            b.fill("#email", email, None).await?;
            b.click(None, Some("Sign up"), None).await?;
            let error = b.check(Some("#error"), None).await?;
            let result = b.check(Some("#result"), None).await?;
            let text = b.read(Some(if want_error { "#error" } else { "#result" })).await?;
            let passed = if want_error {
                error["visible"] == true && result["visible"] == false && text["text"].as_str().is_some_and(|t| t.contains("valid email"))
            } else {
                result["visible"] == true && error["visible"] == false && text["text"] == "Thanks, Grace!"
            };
            results.push(json!({ "name": name, "email": email, "expect": if want_error { "error" } else { "thanks" }, "passed": passed, "shown": text["text"] }));
        }
        anyhow::Ok(results)
    }
    .await;
    let results = oracle.unwrap_or_else(|e| vec![json!({ "error": format!("{e:#}") })]);
    c.json("interaction-tests.json", &json!(results));
    c.check(
        "protectedInteractionTestsPass",
        results.len() == 3 && results.iter().all(|r| r["passed"] == true),
        format!("{} of 3 interaction tests passed", results.iter().filter(|r| r["passed"] == true).count()),
    );
    let shot_at = shot.as_ref().and_then(|s| std::fs::metadata(s).and_then(|m| m.modified()).ok());
    let (_, _, rev_now) = fermi_core::verify::fingerprint(&project);
    c.check(
        "evidenceFromOneRevision",
        std::fs::read(&page).unwrap_or_default() == delivered
            && rev_now == rev_after_task
            && delivered_at.zip(shot_at).is_some_and(|(d, s)| d <= s),
        format!("revision {rev_now}: the screenshot was taken after the page was written, and the tests ran on the same file"),
    );
    c.finish();
}

// ── E2E-04 ──────────────────────────────────────────────────────────────────

/// The interrupted attempt edits the code and calls a deploy hook (a side
/// effect), then the provider fails; a continuation finishes with `rest`.
fn outage_steps(hook: &str, rest: Value) -> Vec<Value> {
    let mut rest = rest;
    rest["match"] = json!("stopped partway");
    vec![
        rest,
        json!({ "calls": [{ "id": "msg_w", "tools": [
                { "id": "t_edit", "name": "Edit", "input": { "file_path": "engine/src/lib.rs" }, "output": "edited" },
                { "id": "t_hook", "name": "Bash", "input": { "command": format!("curl {hook}") }, "output": "notified" } ] }],
            "edits": [{ "path": "engine/src/lib.rs", "content": greet() }],
            "ops": [{ "do": { "http": hook } }],
            "error": "API Error: 529 Overloaded" }),
    ]
}

const RETRY: &str = "\n[retry]\nattempts = 2\nbackoff_secs = 0\nmax_wait_secs = 5\n";

#[tokio::test(flavor = "multi_thread")]
async fn e2e_04_provider_fails_halfway() {
    let mut c = Case::start("E2E-04", "T2 T3");
    c.real(&[
        "dashboard HTTP API (axum)",
        "engine failover",
        "verification (cargo check, cargo test)",
        "trace export",
        "an HTTP endpoint the agent calls (the side effect)",
    ]);
    c.mocked(&["model output and the provider outage (fake-cli)"]);
    let hook_server = egress_recorder().await;
    let hook = format!("{}/deploy-hook", hook_server.url);
    let hits = || hook_server.log.lock().unwrap().iter().filter(|l| l.contains("/deploy-hook")).count();
    let mut results = json!({});

    // 1. Recovery: the second account continues the work.
    let finish = json!({ "text": "Continued: added the changelog entry.", "tools": [{ "name": "Write", "input": { "file_path": "CHANGES.md" } }],
        "edits": [{ "path": "CHANGES.md", "content": "- engine: greet()\n" }] });
    let fx = fixture("F-CODE", &c.work.join("repo-recover")).unwrap();
    let f = Fermi::at(
        &c,
        "home-recover",
        &[account("c1", "claude", outage_steps(&hook, finish.clone())), account("c2", "claude", outage_steps(&hook, finish))],
        RETRY,
    )
    .unwrap();
    let ui = Dash::open(f.engine.clone(), &fx.path).await;
    let prompt = "Add greet() to the engine, notify the deploy hook, and add a changelog entry.";
    let id = ui.create(json!({ "prompt": prompt, "cwd": fx.path, "profile": "p", "verify": { "max_repairs": 0 } })).await;
    let task = ui.wait(id).await;
    let trace = ui.get(&format!("/api/tasks/{id}/trace")).await;
    ui.close().await;
    let after_recovery = hits();
    let second: Vec<Value> = [prompts(&f, "c1"), prompts(&f, "c2")]
        .concat()
        .into_iter()
        .filter(|p| p["prompt"].as_str().is_some_and(|t| t.contains("stopped partway")))
        .collect();
    let kinds = timeline_kinds(&trace);
    results["recovery"] =
        json!({ "task": task["status"], "timeline": kinds, "hook_calls": after_recovery, "continuation_prompts": second });
    c.json("recovery-trace.json", &trace);
    c.text("recovery-patch.diff", &git(&fx.path, &["diff", "HEAD"]).unwrap());
    let handed = second.first().and_then(|p| p["prompt"].as_str()).unwrap_or_default().to_string();
    c.check("sideEffectNotDuplicated", after_recovery == 1, format!("the deploy hook was called {after_recovery} time(s)"));
    c.check(
        "continuationToldWhatWasDone",
        second.len() == 1
            && handed.contains("Bash curl")
            && handed.contains("Edit engine/src/lib.rs (ok)")
            && handed.contains("M engine/src/lib.rs"),
        format!("{} continuation request(s); it lists the tool calls already made and the uncommitted change", second.len()),
    );
    let routes = kinds.iter().filter(|k| *k == "route").count();
    let failovers = kinds.iter().filter(|k| *k == "failover").count();
    c.check(
        "recoveredWithinLimits",
        task["status"] == "done"
            && task["checks"] == true
            && routes == 2
            && failovers == 1
            && read(&fx.path, "engine/src/lib.rs") == greet()
            && read(&fx.path, "CHANGES.md") == "- engine: greet()\n",
        format!(
            "{} after {routes} attempts and {failovers} failover; the interrupted attempt's edit was kept, the checks passed",
            task["status"]
        ),
    );

    // 2. Failure: the fallback fails too; nothing is reported complete.
    let fails = json!({ "error": "API Error: 529 Overloaded" });
    let fx2 = fixture("F-CODE", &c.work.join("repo-fail")).unwrap();
    let f2 = Fermi::at(
        &c,
        "home-fail",
        &[account("c1", "claude", outage_steps(&hook, fails.clone())), account("c2", "claude", outage_steps(&hook, fails))],
        RETRY,
    )
    .unwrap();
    let ui = Dash::open(f2.engine.clone(), &fx2.path).await;
    let id2 = ui.create(json!({ "prompt": prompt, "cwd": fx2.path, "profile": "p", "verify": { "max_repairs": 0 } })).await;
    let task2 = ui.wait(id2).await;
    let trace2 = ui.get(&format!("/api/tasks/{id2}/trace")).await;
    ui.close().await;
    let saved = fermi_core::hub::read_task(&fermi_core::hub::history_dir(&f2.home), id2).map(|t| t.status);
    let kinds2 = timeline_kinds(&trace2);
    let outcome = trace2["timeline"].as_array().into_iter().flatten().filter(|e| e["kind"] == "outcome").cloned().collect::<Vec<_>>();
    results["failure"] =
        json!({ "task": task2["status"], "saved": saved, "timeline": kinds2, "outcomes": outcome, "hook_calls_total": hits() });
    c.json("failure-trace.json", &trace2);
    c.json("fault-timelines.json", &results);
    c.check(
        "terminalFailureIsReliable",
        task2["status"] == "failed"
            && saved == Some(fermi_core::hub::TaskStatus::Failed)
            && outcome.len() == 1
            && outcome[0]["ok"] == false
            && !kinds2.contains(&"checks".to_string())
            && kinds2.iter().filter(|k| *k == "route").count() == 2
            && hits() == 2,
        format!(
            "{} (saved {saved:?}) after {} attempts; no checks, no completion; one hook call in this run",
            task2["status"],
            kinds2.iter().filter(|k| *k == "route").count()
        ),
    );
    c.check(
        "priorWorkRetainedOnFailure",
        read(&fx2.path, "engine/src/lib.rs") == greet(),
        "the interrupted attempt's edit is still in the folder for the user to keep or discard",
    );
    c.finish();
}

// ── E2E-05 ──────────────────────────────────────────────────────────────────

/// A metered model whose estimate is exactly `max_output` × `output`.
fn meter(id: &str) -> String {
    format!(
        "\n[[models]]\nid = \"{id}\"\ncaps = [\"code\", \"general\", \"fast\", \"reasoning\"]\ntier = 2\ncli = \"{}\"\n\n[[prices]]\nversion = \"P1\"\neffective = \"2026-01-01\"\n[prices.models.\"{id}\"]\ninput = 0\noutput = 200\nmax_output = 1000\n\n[[budgets]]\nname = \"b\"\nlimit = 1.00\n",
        id.split(':').nth(1).unwrap()
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_05_cancel_during_tool_operation() {
    let mut c = Case::start("E2E-05", "T2 T3");
    c.real(&["dashboard HTTP API (axum)", "hub cancellation", "process-tree termination", "account slots and budget reservations"]);
    c.mocked(&["the CLI and its long-running tool (fake-cli and a child process)"]);
    let project = c.work.join("project");
    std::fs::create_dir_all(&project).unwrap();
    let step = json!({ "match": "[[E2E-05]]", "touch": "inflight", "child": "child.pid",
        "calls": [{ "id": "msg_long", "tools": [{ "id": "t_build", "name": "Bash", "input": { "command": "cargo build --release" }, "no_result": true }] }],
        "hold": "never-released", "text": "never" });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step])], &format!("budget = \"b\"\n{}", meter("claude:meter"))).unwrap();
    let ui = Dash::open(f.engine.clone(), &project).await;
    let id = ui.create(json!({ "prompt": "Build the release. [[E2E-05]]", "cwd": project, "profile": "p", "model": "claude:meter" })).await;
    let home = f.account_dir("cl");
    wait_file(&home.join("child.pid"), T).await.unwrap();
    wait_file(&home.join("inflight"), T).await.unwrap();
    let tool_running = until(Duration::from_secs(60), || {
        ui.hub.get(id).is_some_and(|t| t.events.iter().any(|e| matches!(e, Event::Tool { name, .. } if name == "Bash")))
    })
    .await;
    let pid = |f: &str| std::fs::read_to_string(home.join(f)).ok().and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(0);
    let (cli, child) = (pid("inflight"), pid("child.pid"));
    let slot = |state: &Value| {
        state["accounts"].as_array().and_then(|a| a.iter().find(|x| x["id"] == "cl")).map(|a| a["running"].clone()).unwrap_or_default()
    };
    let before = json!({ "cli": { "pid": cli, "alive": alive(cli) }, "child": { "pid": child, "alive": alive(child) }, "slot": slot(&ui.get("/api/state").await) });
    let (code, cancelled) = ui.post(&format!("/api/tasks/{id}/cancel"), json!({})).await;
    let asked = Instant::now();
    let deadline = Duration::from_secs(10);
    let gone = until(deadline, || !alive(cli) && !alive(child)).await;
    let took = asked.elapsed().as_millis() as u64;
    // Well past the deadline, nothing has turned it into a completed task.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let task = ui.get(&format!("/api/tasks/{id}")).await;
    let state = ui.get("/api/state").await;
    let after = json!({ "cli": { "pid": cli, "alive": alive(cli) }, "child": { "pid": child, "alive": alive(child) }, "slot": slot(&state), "ms": took });
    ui.close().await;
    let saved = fermi_core::hub::read_task(&fermi_core::hub::history_dir(&f.home), id);
    let ledger = lines(&f.home.join("ledger.jsonl"));
    c.json(
        "process-inventory.json",
        &json!({ "before_cancel": before, "after_cancel": after, "deadline_ms": deadline.as_millis() as u64 }),
    );
    c.json("cancel-response.json", &json!({ "status": code, "body": cancelled }));
    c.json("ui-task.json", &task);
    c.json("ledger.json", &json!(ledger));
    c.check(
        "cancelledWhileToolActive",
        tool_running && before["cli"]["alive"] == true && before["child"]["alive"] == true && code == 200 && cancelled["ok"] == true,
        format!("cancelled through the dashboard while `Bash` ran (CLI {cli}, its child {child})"),
    );
    c.check(
        "noOrphans",
        gone,
        format!("the CLI and the process it started were gone {took} ms after the cancel (deadline {} ms)", deadline.as_millis()),
    );
    let completed = events_of(&task, "done").iter().any(|e| e["ok"] == true);
    c.check(
        "neverReportedComplete",
        task["status"] == "cancelled" && !completed && saved.as_ref().is_some_and(|t| t.status == fermi_core::hub::TaskStatus::Cancelled),
        format!("dashboard {}, history {:?}", task["status"], saved.as_ref().map(|t| t.status)),
    );
    let released = ledger.iter().any(|e| e["source"].as_str().is_some_and(|s| s.contains("cancelled")));
    c.check(
        "resourcesReleased",
        before["slot"] == 1 && after["slot"] == 0 && released,
        format!("account slot {} → {}; budget reservation settled as cancelled: {released}", before["slot"], after["slot"]),
    );
    c.finish();
}

// ── E2E-06 ──────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn e2e_06_close_desktop_during_remote_execution() {
    let mut c = Case::start("E2E-06", "T2 T4");
    c.real(&[
        "fermi worker as its own process (the execution service)",
        "two dashboard instances (closed, then reopened)",
        "event log, artifacts and patch",
    ]);
    c.mocked(&["model output (fake-cli on the worker)"]);
    let held = json!({ "calls": [{ "id": "msg_1", "tools": [{ "id": "t_edit", "name": "Edit", "input": { "file_path": "engine/src/lib.rs" }, "output": "edited" }] }],
        "edits": [{ "path": "engine/src/lib.rs", "content": greet() }], "hold": "go", "text": "Finished on the worker." });
    let wh = Fermi::at(&c, "worker-home", &[account("c1", "claude", vec![held])], "").unwrap();
    let w = start_worker(&wh.home, "svc", None);
    let f = Fermi::at(&c, "controller", &[], &format!("workers = [\"svc\"]\n{}", workers_toml(&[("svc", &w.url)], 30, 0))).unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let desk = Dash::open(f.engine.clone(), &fx.path).await;
    let id = desk.create(json!({ "prompt": "Add greet, then wait for the go-ahead.", "cwd": fx.path, "profile": "p" })).await;
    // A durable checkpoint: the edit's events and the job are in the saved history.
    let file = fermi_core::hub::history_dir(&f.home).join(format!("{id}.json"));
    let cp = until(T, || std::fs::read_to_string(&file).is_ok_and(|t| t.contains("t_edit") && t.contains("\"remote\""))).await;
    let checkpoint: Value = serde_json::from_slice(&std::fs::read(&file).unwrap_or_default()).unwrap_or_default();
    c.json("checkpoint.json", &checkpoint);
    // Close the desktop.
    desk.close().await;
    let job = checkpoint["remote"]["job"].as_str().unwrap_or_default().to_string();
    tokio::time::sleep(Duration::from_millis(300)).await;
    let running = jobs(&w.dir).iter().any(|j| j["id"] == job.as_str() && j["status"] == "running");
    wh.release("c1", "go");
    let finished = until(T, || jobs(&w.dir).iter().any(|j| j["id"] == job.as_str() && j["status"] == "done")).await;
    c.json("service-jobs.json", &json!(jobs(&w.dir)));
    c.check("checkpointed", cp && !job.is_empty(), format!("task {id} saved as job {job} on worker svc"));
    c.check("serviceContinued", running && finished, "the worker kept running the job with the desktop closed, and finished it");
    // Reopen and reconnect.
    let desk = Dash::open(f.engine.clone(), &fx.path).await;
    let task = desk.wait(id).await;
    let trace = desk.get(&format!("/api/tasks/{id}/trace")).await;
    desk.close().await;
    c.json("reconnected-task.json", &task);
    c.json("reconnected-trace.json", &trace);
    let texts: Vec<&str> = events_of(&task, "text").iter().filter_map(|e| e["text"].as_str()).collect();
    c.check(
        "restoredAfterReconnect",
        task["status"] == "done" && texts.contains(&"Finished on the worker.") && ops(&trace, "Edit").len() == 1,
        format!("{}; texts {texts:?}; {} edit in the trace", task["status"], ops(&trace, "Edit").len()),
    );
    let ledger = lines(&fermi_core::remote::dir(&f.home, id).join("ledger.jsonl"));
    c.json("ledger.json", &json!(ledger));
    let count = |e: &str| ledger.iter().filter(|x| x["event"] == e).count();
    let applied = std::fs::read(fx.path.join("engine/src/lib.rs")).unwrap_or_default();
    c.text(
        "artifact-checksums.json",
        &serde_json::to_string_pretty(&json!({ "engine/src/lib.rs": sha256(&applied), "expected": sha256(greet().as_bytes()) })).unwrap(),
    );
    c.check(
        "noDuplicateSubmission",
        count("dispatched") == 1 && count("applied") == 1 && jobs(&w.dir).len() == 1 && sha256(&applied) == sha256(greet().as_bytes()),
        format!(
            "{} dispatch, {} job on the worker, the change applied {} time with the expected checksum",
            count("dispatched"),
            jobs(&w.dir).len(),
            count("applied")
        ),
    );
    c.finish();
}

// ── E2E-07 ──────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn e2e_07_plugin_forbidden_operation() {
    let mut c = Case::start("E2E-07", "T2 T3");
    c.real(&[
        "dashboard HTTP API (axum)",
        "fermi's MCP bridge (its own process)",
        "plugin host, argument scope and deny_read checks",
        "plugin sandbox (Landlock on Linux)",
    ]);
    c.mocked(&["the adversarial plugin (fake-plugin)", "the agent's decisions (fake-cli)"]);
    let canary = "E2E07-SENTINEL-7f3a-c4n4ry";
    let zone = c.work.join("secret-zone");
    std::fs::create_dir_all(&zone).unwrap();
    let sentinel = zone.join("sentinel.txt");
    std::fs::write(&sentinel, canary).unwrap();
    let project = c.work.join("project");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(project.join(".env"), canary).unwrap();
    let probe = c.work.join("probe");
    std::fs::create_dir_all(&probe).unwrap();
    let log = probe.join("adversarial.log");
    let mcp = |name: &str, args: Value| json!({ "mcp": { "name": name, "arguments": args } });
    let step = json!({ "match": "[[E2E-07]]", "text": "I tried the plugin's tools.", "ops": [
        mcp("plugin__adversarial__read_file", json!({ "path": sentinel.display().to_string() })),
        mcp("plugin__adversarial__read_file", json!({ "path": ".env" })),
        mcp("plugin__adversarial__read_sentinel", json!({})),
    ] });
    let deny = format!("[security]\ndeny_read = [\"{}/.env\"]\n", project.display().to_string().replace('\\', "/"));
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step])], &deny).unwrap();
    let src = acceptance_tests::plugin_fixture(
        "adversarial",
        &c.work.join("src/adversarial"),
        &[("SENTINEL", &sentinel), ("LOG", &log), ("PROBE", &probe)],
    )
    .unwrap();
    fermi_core::plugins::install(&f.engine, &src).await.unwrap();
    let ui = Dash::open(f.engine.clone(), &project).await;
    let id = ui
        .create(json!({ "prompt": "Use the adversarial plugin's tools to look around. [[E2E-07]]", "cwd": project, "profile": "p" }))
        .await;
    let task = ui.wait(id).await;
    let trace = ui.get(&format!("/api/tasks/{id}/trace")).await;
    ui.close().await;
    c.json("trace.json", &trace);
    let plugin_log: Vec<Value> = lines(&fermi_core::plugins::dir(&f.home).join("events.jsonl"));
    c.json("plugin-events.json", &json!(plugin_log));
    let reads = ops(&trace, "mcp__fermi__plugin__adversarial__read_file");
    let refused = |o: &Value| o["status"] == "error" && o["output"].as_str().is_some_and(|t| t.contains("refused"));
    c.check(
        "deniedAndTraced",
        task["status"] == "done"
            && reads.len() == 2
            && reads.iter().all(|o| refused(o))
            && reads[0]["output"].as_str().is_some_and(|t| t.contains("outside the plugin's scope"))
            && reads[1]["output"].as_str().is_some_and(|t| t.contains("deny_read") || t.contains("outside the plugin's scope")),
        format!("{} read_file attempts in the task's trace, each an error with the refusal and its reason", reads.len()),
    );
    let ours: Vec<&Value> = plugin_log.iter().filter(|e| e["event"] == "refused" && e["task"] == id).collect();
    let reached = lines(&log).iter().filter(|r| r["method"] == "tools/call" && r["params"]["name"] == "read_file").count();
    c.check(
        "attemptInPluginLog",
        ours.len() == 2 && ours.iter().all(|e| e["reason"].as_str().is_some_and(|r| !r.is_empty())) && reached == 0,
        format!("{} refusals logged for task {id} with their reasons; {reached} read_file calls reached the plugin", ours.len()),
    );
    let (files, net) = fermi_core::sandbox::os_enforced();
    let direct = ops(&trace, "mcp__fermi__plugin__adversarial__read_sentinel").first().cloned().cloned().unwrap_or_default();
    if files && net {
        c.check(
            "sandboxDeniedDirectRead",
            direct["output"].as_str().is_some_and(|t| !t.contains(canary)),
            format!("the plugin's own read of the sentinel: {}", direct["output"]),
        );
        // Every artifact fermi keeps for this task, plus the plugin's files.
        let mut scanned = all_text(&fermi_core::hub::history_dir(&f.home));
        scanned.extend(all_text(&fermi_core::trace::tapes_dir(&f.home, id)));
        scanned.extend(all_text(&fermi_core::plugins::dir(&f.home)));
        scanned.push(("trace".into(), trace.to_string()));
        scanned.push(("usage".into(), std::fs::read_to_string(f.home.join("usage.jsonl")).unwrap_or_default()));
        let leaks: Vec<&String> = scanned.iter().filter(|(_, t)| t.contains(canary)).map(|(p, _)| p).collect();
        c.check("canaryAbsent", leaks.is_empty(), format!("{} files scanned; sentinel found in {leaks:?}", scanned.len()));
        c.finish();
    } else {
        c.check(
            "hookRefusalsCarryNoCanary",
            reads.iter().all(|o| !o["output"].to_string().contains(canary)),
            "the refused calls returned no sentinel bytes",
        );
        c.block("the OS file sandbox for plugins needs Linux (Landlock); the refusal in the agent's trace and the plugin log were verified on this platform");
    }
}

// ── E2E-09 ──────────────────────────────────────────────────────────────────

/// The baseline tree an evaluation run's work folder started from.
fn baseline_tree(work: &Path) -> String {
    git(work, &["rev-parse", "HEAD^{tree}"]).unwrap_or_default()
}

/// E2E-09 on real models (the live profile): two models of a real account
/// compared on one frozen task. What is checked is fermi's part (fresh
/// environments, one oracle, identity, latency, usage and cost recorded),
/// not whether either model solves the task.
async fn e2e_09_live(mut c: Case, accounts: &[String]) {
    c.real(&[
        "evaluation runner (fresh folder per run)",
        "independent scoring",
        "the dashboard's comparison API",
        "the provider's own CLI on a real account",
        "two real models",
    ]);
    let ds = acceptance_tests::smoke_dataset(&c.work.join("dataset"), 1).unwrap();
    let f = match Fermi::live(&c, "fermi-home", accounts, "") {
        Ok(f) => f,
        Err(e) => return c.block(&format!("{e:#}")),
    };
    let Some((ma, mb)) = acceptance_tests::live_models(&f) else {
        return c.block("the live comparison is set up for Claude accounts");
    };
    let cfg = |name: &str, model: &str| fermi_core::eval::EvalConfig {
        name: name.into(),
        profile: Some("p".into()),
        model: Some(model.into()),
        ..Default::default()
    };
    let (ca, cb) = (cfg("A", &ma), cfg("B", &mb));
    let (a, b) = tokio::join!(fermi_core::eval::run(&f.engine, &ds, &ca), fermi_core::eval::run(&f.engine, &ds, &cb));
    let (a, b) = (a.unwrap(), b.unwrap());
    let ui = Dash::open(f.engine.clone(), &c.work).await;
    let cmp = ui.get(&format!("/api/evals/compare?a={}&b={}", a.id, b.id)).await;
    ui.close().await;
    let runs = fermi_core::eval::runs_dir(&f.home);
    let (wa, wb) = (runs.join(&a.id).join("tasks/add/work"), runs.join(&b.id).join("tasks/add/work"));
    for (label, r) in [("a", &a), ("b", &b)] {
        c.json(&format!("report-{label}.json"), &serde_json::to_value(r).unwrap());
        c.text(&format!("{label}/patch.diff"), &std::fs::read_to_string(runs.join(&r.id).join(&r.tasks[0].patch)).unwrap_or_default());
    }
    c.json("comparison.json", &cmp);
    let (ta, tb) = (&a.tasks[0], &b.tasks[0]);
    c.json(
        "environments.json",
        &json!({ "a": { "run": a.id, "work": wa, "baseline_tree": baseline_tree(&wa), "model": ta.model, "account": ta.account },
                 "b": { "run": b.id, "work": wb, "baseline_tree": baseline_tree(&wb), "model": tb.model, "account": tb.account } }),
    );
    c.check(
        "freshEnvironments",
        canon(&wa) != canon(&wb)
            && !baseline_tree(&wa).is_empty()
            && baseline_tree(&wa) == baseline_tree(&wb)
            && ta.fixture_hash == tb.fixture_hash
            && a.dataset.hash == b.dataset.hash
            && ta.account.as_ref().is_some_and(|x| accounts.contains(x))
            && tb.account.as_ref().is_some_and(|x| accounts.contains(x)),
        format!(
            "two folders from the same pristine fixture (tree {}), on {:?} and {:?}",
            &baseline_tree(&wa).chars().take(12).collect::<String>(),
            ta.account,
            tb.account
        ),
    );
    let mem = |w: &Path| -> Vec<String> {
        fermi_core::memory::Memory::existing(&f.home, w)
            .map(|m| m.list(fermi_core::memory::Caller::User).iter().map(|v| format!("{} {}", v.record.id, v.record.title)).collect())
            .unwrap_or_default()
    };
    let (mem_a, mem_b) = (mem(&wa), mem(&wb));
    c.json("isolation.json", &json!({ "memory_a": mem_a, "memory_b": mem_b }));
    c.check(
        "noSharedMemory",
        mem_a.iter().all(|r| !mem_b.contains(r)),
        format!("project memory: A {} record(s), B {} record(s), none in common", mem_a.len(), mem_b.len()),
    );
    let same_oracle = a.evaluator == b.evaluator
        && ta.checks.iter().map(|x| (x.name.clone(), x.weight)).collect::<Vec<_>>()
            == tb.checks.iter().map(|x| (x.name.clone(), x.weight)).collect::<Vec<_>>()
        && ta.max_score == tb.max_score;
    c.check(
        "sameOracle",
        same_oracle,
        format!(
            "one evaluator and the same weighted checks: A ({ma}) {} {}/{}, B ({mb}) {} {}/{}",
            ta.status, ta.score, ta.max_score, tb.status, tb.score, tb.max_score
        ),
    );
    let side = |s: &str| &cmp[s];
    let measured = |s: &str| {
        side(s)["summary"]["latency_ms"].as_u64().is_some_and(|l| l > 0)
            && side(s)["summary"]["output_tokens"].as_u64().is_some_and(|t| t > 0)
    };
    let cost_stated = |s: &str, k: &str| {
        side(s)["summary"]["cost_micros"].is_i64()
            || cmp["missing"].as_array().is_some_and(|m| m.iter().any(|x| x.as_str().is_some_and(|x| x.starts_with(&format!("{k} cost")))))
    };
    c.check(
        "comparisonShowsIdentityQualityLatencyCost",
        side("a")["config"]["model"] == ma.as_str()
            && side("b")["config"]["model"] == mb.as_str()
            && ta.model.as_deref() == Some(ma.as_str())
            && tb.model.as_deref() == Some(mb.as_str())
            && side("a")["summary"]["score"].is_u64()
            && side("b")["summary"]["score"].is_u64()
            && measured("a")
            && measured("b")
            && cost_stated("a", "A")
            && cost_stated("b", "B"),
        format!(
            "A {} score {} in {} ms, {} output tokens, cost {}; B {} score {} in {} ms, {} output tokens, cost {}; missing {}",
            side("a")["config"]["model"],
            side("a")["summary"]["score"],
            side("a")["summary"]["latency_ms"],
            side("a")["summary"]["output_tokens"],
            side("a")["summary"]["cost_micros"],
            side("b")["config"]["model"],
            side("b")["summary"]["score"],
            side("b")["summary"]["latency_ms"],
            side("b")["summary"]["output_tokens"],
            side("b")["summary"]["cost_micros"],
            cmp["missing"]
        ),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_09_compare_two_models() {
    let c = Case::start("E2E-09", "T2 T4");
    let (Some(mut c), live) = acceptance_tests::live_or_block(c) else { return };
    if let Some(accounts) = live {
        return e2e_09_live(c, &accounts).await;
    }
    c.real(&[
        "evaluation runner (fresh folder per run)",
        "independent scoring",
        "fermi's MCP bridge (memory)",
        "the dashboard's comparison API",
    ]);
    c.mocked(&["both models (fake-cli)"]);
    let ds = acceptance_tests::smoke_dataset(&c.work.join("dataset"), 1).unwrap();
    let solution =
        |kind: &str| std::fs::read_to_string(repo_root().join(format!("tests/fixtures/F-EVAL/solutions/add/{kind}.rs"))).unwrap();
    let marker = |m: &str| format!("CANDIDATE-{m}-ONLY-5e1d");
    let a_step = json!({ "match": "Task add:", "text": "Implemented add.", "usage": { "input": 1000, "output": 200 },
        "edits": [{ "path": "src/lib.rs", "content": solution("correct") }, { "path": "NOTES.md", "content": marker("A") }],
        "ops": [{ "mcp": { "name": "memory_record", "arguments": { "kind": "note", "title": "How A solved add", "body": format!("{} wrapping is wrong", marker("A")) } } }] });
    let b_step = json!({ "match": "Task add:", "text": "Implemented add.", "usage": { "input": 800, "output": 150 },
        "edits": [{ "path": "src/lib.rs", "content": solution("wrong") }, { "path": "NOTES.md", "content": marker("B") }] });
    let prices = "\n[[prices]]\nversion = \"E1\"\neffective = \"2026-01-01\"\n[prices.models.\"claude:sonnet\"]\ninput = 3\noutput = 15\n[prices.models.\"openai:gpt\"]\ninput = 1\noutput = 4\n";
    let f = Fermi::new(&c, &[account("cl", "claude", vec![a_step]), account("cx", "openai", vec![b_step])], prices).unwrap();
    let cfg = |name: &str, model: &str| fermi_core::eval::EvalConfig {
        name: name.into(),
        profile: Some("p".into()),
        model: Some(model.into()),
        ..Default::default()
    };
    // Both at once, each in an environment of its own.
    let (ca, cb) = (cfg("A", "claude:sonnet"), cfg("B", "openai:gpt"));
    let (a, b) = tokio::join!(fermi_core::eval::run(&f.engine, &ds, &ca), fermi_core::eval::run(&f.engine, &ds, &cb));
    let (a, b) = (a.unwrap(), b.unwrap());
    let ui = Dash::open(f.engine.clone(), &c.work).await;
    let cmp = ui.get(&format!("/api/evals/compare?a={}&b={}", a.id, b.id)).await;
    ui.close().await;
    c.json("report-a.json", &serde_json::to_value(&a).unwrap());
    c.json("report-b.json", &serde_json::to_value(&b).unwrap());
    c.json("comparison.json", &cmp);
    let runs = fermi_core::eval::runs_dir(&f.home);
    let (wa, wb) = (runs.join(&a.id).join("tasks/add/work"), runs.join(&b.id).join("tasks/add/work"));
    let envs = json!({ "a": { "run": a.id, "work": wa, "fixture": a.tasks[0].fixture_hash, "model": a.tasks[0].model, "account": a.tasks[0].account },
        "b": { "run": b.id, "work": wb, "fixture": b.tasks[0].fixture_hash, "model": b.tasks[0].model, "account": b.tasks[0].account } });
    c.json("environments.json", &envs);
    c.check(
        "freshEnvironments",
        canon(&wa) != canon(&wb)
            && a.tasks[0].fixture_hash == b.tasks[0].fixture_hash
            && a.dataset.hash == b.dataset.hash
            && a.tasks[0].account.as_deref() == Some("cl")
            && b.tasks[0].account.as_deref() == Some("cx"),
        "two folders from the same fixture and dataset; each candidate ran on its own account",
    );
    // Neither saw the other's work: files, patches, prompts, memory.
    let patch = |r: &fermi_core::eval::Report| std::fs::read_to_string(runs.join(&r.id).join(&r.tasks[0].patch)).unwrap_or_default();
    let prompt_text =
        |acct: &str| prompts(&f, acct).iter().map(|p| p["prompt"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>().join("\n");
    let mem = |w: &Path| {
        fermi_core::memory::Memory::existing(&f.home, w)
            .map(|m| {
                m.list(fermi_core::memory::Caller::User).iter().map(|v| format!("{} {}", v.record.title, v.record.body)).collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let (mem_a, mem_b) = (mem(&wa), mem(&wb));
    c.json("isolation.json", &json!({ "memory_a": mem_a, "memory_b": mem_b }));
    c.check(
        "noCrossVisibility",
        read(&wa, "NOTES.md") == marker("A")
            && read(&wb, "NOTES.md") == marker("B")
            && !patch(&b).contains(&marker("A"))
            && !patch(&a).contains(&marker("B"))
            && !prompt_text("cx").contains(&marker("A"))
            && !prompt_text("cl").contains(&marker("B"))
            && mem_a.iter().any(|m| m.contains(&marker("A")))
            && !mem_b.iter().any(|m| m.contains(&marker("A"))),
        "each folder, patch and prompt holds only its own candidate's work; A's memory note stayed in A's project",
    );
    let same_oracle = a.evaluator == b.evaluator
        && a.tasks[0].checks.iter().map(|x| (x.name.clone(), x.weight)).collect::<Vec<_>>()
            == b.tasks[0].checks.iter().map(|x| (x.name.clone(), x.weight)).collect::<Vec<_>>();
    c.check(
        "sameOracle",
        same_oracle && a.tasks[0].status == "passed" && b.tasks[0].status == "failed",
        format!(
            "one evaluator and the same weighted checks: A {} {}/{}, B {} {}/{}",
            a.tasks[0].status, a.tasks[0].score, a.tasks[0].max_score, b.tasks[0].status, b.tasks[0].score, b.tasks[0].max_score
        ),
    );
    let side = |s: &str| &cmp[s];
    c.check(
        "comparisonShowsIdentityQualityLatencyCost",
        side("a")["config"]["model"] == "claude:sonnet"
            && side("b")["config"]["model"] == "openai:gpt"
            && side("a")["summary"]["score"] == a.summary.score
            && side("b")["summary"]["score"] == b.summary.score
            && side("a")["summary"]["latency_ms"].is_u64()
            && side("a")["summary"]["cost_micros"].is_i64()
            && side("b")["summary"]["cost_micros"].is_i64()
            && cmp["differences"]["score"].is_i64(),
        format!(
            "A {} score {} cost {}; B {} score {} cost {}",
            side("a")["config"]["model"],
            side("a")["summary"]["score"],
            side("a")["summary"]["cost_micros"],
            side("b")["config"]["model"],
            side("b")["summary"]["score"],
            side("b")["summary"]["cost_micros"]
        ),
    );
    c.finish();
}

// ── E2E-10 ──────────────────────────────────────────────────────────────────

/// A caller of `normalize` (through `describe`) the agent must not change.
const PROTECTED_DESCRIBE: &str =
    "#[test]\nfn describe_normalizes_and_scores() {\n    assert_eq!(engine::describe(\" Ada \"), \"ada:1\");\n}\n";

#[tokio::test(flavor = "multi_thread")]
async fn e2e_10_multi_file_refactor() {
    let mut c = Case::start("E2E-10", "T2");
    c.real(&[
        "dashboard HTTP API (axum)",
        "isolated worktree",
        "code intelligence through fermi's MCP bridge (symbol, rename, impact)",
        "verification (cargo check, cargo test)",
        "git",
    ]);
    c.mocked(&["the agent's decisions (fake-cli, calling the bridge's code tools)"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    write(&fx.path, "engine/tests/describe.rs", PROTECTED_DESCRIBE);
    edit(&fx.path, ".fermi/checks.toml", "max_repairs = 3", "max_repairs = 1\nprotect = [\"engine/tests/**\"]");
    commit(&fx.path, "a protected caller");
    let base = git(&fx.path, &["rev-parse", "HEAD"]).unwrap();
    let app_before = read(&fx.path, "app/src/main.rs");
    let mcp = |name: &str, args: Value| json!({ "mcp": { "name": name, "arguments": args } });
    let step = json!({ "match": "[[E2E-10]]", "text": "Renamed normalize to canonical_name across the workspace.", "ops": [
        mcp("code_symbol", json!({ "target": "normalize" })),
        mcp("code_rename", json!({ "target": "normalize", "to": "canonical_name" })),
        mcp("code_impact", json!({})),
    ] });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step])], "").unwrap();
    let ui = Dash::open(f.engine.clone(), &fx.path).await;
    let id = ui
        .create(json!({ "prompt": "Rename util's normalize to canonical_name everywhere, check what it affects, and keep the tests passing. [[E2E-10]]",
            "cwd": fx.path, "profile": "p", "isolate": true, "verify": { "max_repairs": 1 } }))
        .await;
    let task = ui.wait(id).await;
    let trace = ui.get(&format!("/api/tasks/{id}/trace")).await;
    let diff = ui.get(&format!("/api/tasks/{id}/diff")).await;
    ui.close().await;
    c.json("ui-task.json", &task);
    c.json("trace.json", &trace);
    c.json("review-diff.json", &diff);
    c.text("final.diff", diff["patch"].as_str().unwrap_or_default());
    let wt = PathBuf::from(diff["path"].as_str().unwrap_or_default());
    let out = |name: &str| ops(&trace, name).first().and_then(|o| o["output"].as_str()).unwrap_or_default().to_string();
    let (symbol, rename, impact) = (out("mcp__fermi__code_symbol"), out("mcp__fermi__code_rename"), out("mcp__fermi__code_impact"));
    c.text("impact/symbol.txt", &symbol);
    c.text("impact/rename.txt", &rename);
    c.text("impact/impact.txt", &impact);
    c.check(
        "impactChecked",
        symbol.contains("process at engine/src/lib.rs")
            && impact.contains("canonical_name at util/src/lib.rs")
            && impact.contains("describe at engine/src/lib.rs")
            && impact.contains("main at app/src/main.rs")
            && impact.contains("describe_normalizes_and_scores"),
        "before: the callers of normalize; after: the changed symbol and what reaches it (process, describe, app's main) with the tests to run",
    );
    let truth = acceptance_tests::ground_truth("F-CODE", "references.json");
    let files: Vec<String> = truth["callers"]["util::normalize"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x["file"].as_str().map(str::to_string))
        .collect();
    let mut changed: Vec<String> =
        diff["files"].as_array().into_iter().flatten().filter_map(|x| x["path"].as_str().map(str::to_string)).collect();
    changed.sort();
    // References, that is: the test named normalize_trims is another identity and keeps its name.
    let leftover: Vec<String> = ["util/src/lib.rs", "engine/src/lib.rs"]
        .iter()
        .filter(|p| ["normalize(", "util::normalize", "fn normalize("].iter().any(|r| read(&wt, p).contains(r)))
        .map(|p| p.to_string())
        .collect();
    c.check(
        "dependenciesCovered",
        files.iter().all(|f| changed.contains(f))
            && leftover.is_empty()
            && read(&wt, "engine/src/lib.rs").contains("use util::canonical_name;"),
        format!("every known caller's file changed ({files:?}); no reference to `normalize` left in them"),
    );
    c.check(
        "unrelatedIdentitiesIntact",
        changed == ["engine/src/lib.rs", "util/src/lib.rs"]
            && read(&wt, "app/src/main.rs") == app_before
            && app_before.contains("normalize()")
            && app_before.contains("\"normalize\"")
            && read(&wt, "util/src/lib.rs").contains("fn normalize_trims()"),
        format!("changed {changed:?}; app's comment and string that say normalize, and the test normalize_trims, are untouched"),
    );
    let tested = trace["tested_revision"]["fingerprint"].as_str().unwrap_or_default().to_string();
    let (ok, test_out) = cargo_test(&wt, &c.work.join("oracle-target"));
    c.text("oracle-cargo-test.txt", &test_out);
    c.check(
        "testsPassOnFinalRevision",
        task["status"] == "done"
            && trace["tested_revision"]["passed"] == true
            && tested == diff["fingerprint"].as_str().unwrap_or("-")
            && ok
            && test_out.contains("describe_normalizes_and_scores ... ok")
            && read(&wt, "engine/tests/describe.rs") == PROTECTED_DESCRIBE,
        format!(
            "fermi's checks passed at {tested}, the revision under review; cargo test outside fermi passes, the protected caller included"
        ),
    );
    let patch = diff["patch"].as_str().unwrap_or_default();
    c.check(
        "changesReviewable",
        diff["base"] == base.as_str()
            && diff["branch"] == format!("fermi/task-{id}")
            && patch.contains("-pub fn normalize")
            && patch.contains("+pub fn canonical_name")
            && git(&fx.path, &["status", "--porcelain"]).unwrap().is_empty()
            && git(&fx.path, &["rev-parse", "HEAD"]).unwrap() == base,
        format!("the full diff against {} on {}; the user's checkout is untouched", &base[..12], diff["branch"]),
    );
    c.finish();
}
