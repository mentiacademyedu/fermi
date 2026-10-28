//! Dashboard tests (UI-01 … UI-05): the web UI (`crates/fermi/web/index.html`)
//! in a real headless browser against the real dashboard server, clicked
//! through as a user does. They aren't catalog cases (the 106 IDs live in
//! `tests/acceptance/systems.yaml`): the E2E workflows drive the dashboard's
//! HTTP API, and these guard the page on top of it. Only the model's output
//! comes from the fake CLI. Every case also fails on any page error.
//!
//!   cargo test -p acceptance-tests --locked --test dashboard

use acceptance_tests::{account, fixture, git, sha256, Case, Fermi};
use fermi_core::browser::{self, Browser};
use fermi_core::engine::Engine;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(120);

/// The dashboard server and a browser window on it.
struct Window {
    base: String,
    token: String,
    http: reqwest::Client,
    page: Browser,
    /// Requests whose refusal the case causes on purpose.
    allowed: std::sync::Mutex<Vec<String>>,
    stop: tokio::sync::oneshot::Sender<()>,
    serving: tokio::task::JoinHandle<anyhow::Result<()>>,
}

impl Window {
    /// Starts the dashboard on `engine` (launched in `cwd`) and opens it with its access key.
    async fn open(c: &Case, engine: Arc<Engine>, cwd: &Path) -> Window {
        let server = fermi::server::start(engine.clone(), 0, cwd.to_path_buf(), "ui").await.unwrap();
        let (base, token) = server.url.split_once("/#t=").map(|(b, t)| (b.to_string(), t.to_string())).unwrap();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let serving = tokio::spawn(server.serve(async {
            let _ = stopped.await;
        }));
        let page = Browser::launch(&engine.cfg.browser, &c.work.join("browser"), engine.redactor.clone()).await.unwrap();
        let w = Window { base, token, http: reqwest::Client::new(), page, allowed: Default::default(), stop, serving };
        w.page.open(&format!("{}/#t={}", w.base, w.token)).await.unwrap();
        w
    }

    /// Evaluates `expr` in the page (promises are awaited).
    async fn js(&self, expr: &str) -> Value {
        let r = self.page.send("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true, "awaitPromise": true })).await;
        r.map(|v| v["result"]["value"].clone()).unwrap_or(Value::Null)
    }

    /// Waits until `expr` is true in the page; false if it never was.
    async fn until(&self, expr: &str) -> bool {
        let start = Instant::now();
        while start.elapsed() < WAIT {
            if self.js(expr).await == true {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    /// Waits until the page's text contains `text`.
    async fn shows(&self, text: &str) -> bool {
        self.until(&format!("document.body.innerText.includes({})", json!(text))).await
    }

    /// Whether the window is open: the splash gone, the app shown and its live connection up.
    async fn ready(&self) -> bool {
        self.until("!document.querySelector('#app').hidden && typeof S === 'object' && S.connected === true").await
    }

    async fn api(&self, path: &str) -> Value {
        let r = self.http.get(format!("{}{path}", self.base)).header("x-fermi-token", &self.token).send().await.unwrap();
        r.json().await.unwrap_or(Value::Null)
    }

    /// Lets the browser's report of a refused request to `path` through.
    fn allow(&self, path: &str) {
        self.allowed.lock().unwrap().push(path.to_string());
    }

    /// Page errors and uncaught exceptions so far.
    async fn errors(&self) -> Vec<Value> {
        let s = self.page.state().await.unwrap_or_default();
        let allowed = self.allowed.lock().unwrap().clone();
        let expected = |e: &Value| {
            let (src, text) = (e["source"].as_str().unwrap_or_default(), e["text"].as_str().unwrap_or_default());
            text.starts_with("Failed to load resource") && allowed.iter().any(|a| src.ends_with(a.as_str()))
        };
        s["console"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| (e["level"] == "exception" || e["level"] == "error") && !expected(e))
            .cloned()
            .collect()
    }

    /// Saves the evidence (browser trace, a final screenshot) and closes the window.
    async fn close(self, c: &mut Case) {
        let _ = self.page.screenshot(Some("final"), true).await;
        let errors = self.errors().await;
        c.check("noPageErrors", errors.is_empty(), format!("{} page error(s): {errors:?}", errors.len()));
        let dir = self.page.dir.clone();
        drop(self.page);
        let _ = self.stop.send(());
        let _ = self.serving.await;
        c.text("browser/trace.jsonl", &std::fs::read_to_string(dir.join("trace.jsonl")).unwrap_or_default());
        for e in std::fs::read_dir(&dir).into_iter().flatten().filter_map(|e| e.ok()) {
            if e.path().extension().is_some_and(|x| x == "png") {
                let name = e.file_name().to_string_lossy().to_string();
                let bytes = std::fs::read(e.path()).unwrap_or_default();
                let _ = std::fs::write(c.dir.join("browser").join(&name), &bytes);
                c.text(&format!("browser/{name}.sha256"), &sha256(&bytes));
            }
        }
    }
}

/// Without a Chromium-based browser the case can't run here.
fn need_browser(c: Case, f: &Fermi) -> Option<Case> {
    if browser::find(&f.engine.cfg.browser).is_some() {
        return Some(c);
    }
    c.block("no Chrome, Edge or Chromium on this machine");
    None
}

fn greet(src: &str) -> String {
    format!("{src}\npub fn greet(name: &str) -> String {{\n    format!(\"hello {{}}\", name.trim().to_lowercase())\n}}\n")
}

// ── UI-01 ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn ui_01_opens_and_connects() {
    let mut c = Case::start("UI-01", "T2");
    c.real(&["dashboard server (axum)", "web UI in a headless Chromium-based browser", "live connection (WebSocket)"]);
    c.mocked(&["model output (fake-cli)"]);
    let f = Fermi::new(&c, &[account("cl", "claude", vec![])], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let w = Window::open(&c, f.engine.clone(), &fx.path).await;

    let ready = w.ready().await && w.until("!document.querySelector('#splash')").await;
    let shown = w.js("({ ws: document.querySelector('#ws-list').innerText, splash: !!document.querySelector('#splash'), hero: document.querySelector('#hero-sub').innerText })").await;
    c.json("window.json", &shown);
    c.check(
        "opensWithItsKey",
        ready && shown["ws"].as_str().is_some_and(|t| t.contains("repo")) && shown["splash"] == false,
        format!("app shown and connected: {ready}; workspaces listed: {}", shown["ws"]),
    );
    // The key stays out of the address bar once read.
    let url = w.js("location.href").await;
    c.check("keyLeavesTheAddressBar", url.as_str().is_some_and(|u| !u.contains(&w.token)), format!("address after loading: {url}"));

    // Without the key: the page says how to open it, and the API refuses
    // (the browser logs that refusal; it's the only error allowed here).
    w.js("sessionStorage.clear()").await;
    w.allow("/api/state");
    w.page.open(&format!("{}/", w.base)).await.unwrap();
    let refused = w.js("fetch('/api/state').then(r => r.status)").await;
    let says = w.shows("Open fermi from the app").await;
    let hidden = w.js("document.querySelector('#app').hidden").await;
    c.check(
        "refusedWithoutTheKey",
        says && hidden == true && refused == 401,
        format!("splash explains how to open it: {says}; app hidden: {hidden}; /api/state answered {refused}"),
    );
    w.close(&mut c).await;
    c.finish();
}

// ── UI-02 ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn ui_02_isolated_task_from_the_composer() {
    let mut c = Case::start("UI-02", "T2");
    c.real(&["dashboard server and web UI in a headless browser", "hub task service", "git worktrees", "trace export"]);
    c.mocked(&["model output (fake-cli)"]);
    let fx_src = std::fs::read_to_string(acceptance_tests::repo_root().join("tests/fixtures/F-CODE/repo/engine/src/lib.rs")).unwrap();
    let step = json!({ "match": "[[UI-02]]",
        "calls": [{ "id": "msg_u2", "usage": { "input_tokens": 900, "output_tokens": 200 }, "tools": [
            { "id": "t_edit", "name": "Edit", "input": { "file_path": "engine/src/lib.rs" }, "output": "edited" } ] }],
        "edits": [{ "path": "engine/src/lib.rs", "content": greet(&fx_src) }], "text": "Added greet() to the engine." });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step])], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let w = Window::open(&c, f.engine.clone(), &fx.path).await;
    assert!(w.ready().await, "the dashboard didn't open");

    // Type the task, tick Isolated, send.
    let prompt = "Add a greet(name) function to the engine crate. [[UI-02]]";
    w.page.fill("#prompt", prompt, None).await.unwrap();
    w.page.click(Some("#f-isolate"), None, None).await.unwrap();
    w.page.click(Some("#send"), None, None).await.unwrap();
    let done = w.shows("Added greet() to the engine.").await && w.until("!!document.querySelector('#log .ev.ok')").await;
    let id = w.js("S.sel").await.as_u64().unwrap_or_default();
    let task = w.api(&format!("/api/tasks/{id}")).await;
    c.json("task.json", &task);
    let calls = f.calls("cl");
    let sent = calls.iter().filter_map(|x| x["prompt"].as_str()).any(|p| p.contains(prompt));
    c.check(
        "sentFromTheComposer",
        done && task["status"] == "done"
            && sent
            && w.js("document.querySelector('#convo-title').innerText").await.as_str().is_some_and(|t| !t.is_empty()),
        format!("task {id}: {}; the agent got the prompt: {sent}; the conversation shows the outcome: {done}", task["status"]),
    );

    // Isolated: the change is on the task's own branch, the user's folder is untouched.
    let trace = w.api(&format!("/api/tasks/{id}/trace")).await;
    c.json("trace.json", &trace);
    let wt = trace["workspace"]["path"].as_str().map(std::path::PathBuf::from).unwrap_or_default();
    let in_wt = std::fs::read_to_string(wt.join("engine/src/lib.rs")).unwrap_or_default();
    let status = git(&fx.path, &["status", "--porcelain"]).unwrap_or_default();
    c.check(
        "isolatedFromTheUsersFolder",
        in_wt.contains("pub fn greet") && status.trim().is_empty() && !wt.starts_with(&fx.path),
        format!(
            "worktree {}; its lib.rs has greet: {}; the user's folder: {:?}",
            wt.display(),
            in_wt.contains("pub fn greet"),
            status.trim()
        ),
    );

    // Trace → Changes shows the task's diff.
    w.page.click(Some("#trace-task"), None, None).await.unwrap();
    let open = w.until("document.querySelector('#dlg-trace').open").await;
    w.page.click(Some("#trace-list > details:first-child > summary"), None, None).await.unwrap();
    let diff_shown = w
        .until("[...document.querySelectorAll('#trace-list details')].some(d => d.open && d.innerText.includes('engine/src/lib.rs') && d.innerText.includes('+pub fn greet'))")
        .await;
    let listed = w.js("document.querySelector('#trace-list').innerText").await;
    let _ = w.page.screenshot(Some("trace-changes"), false).await;
    c.text("trace-dialog.txt", listed.as_str().unwrap_or_default());
    c.check(
        "changesInTheTrace",
        open && diff_shown && listed.as_str().is_some_and(|t| t.contains("model call") && t.contains("Edit")),
        format!("trace dialog open: {open}; the Changes entry shows the patch: {diff_shown}"),
    );
    w.close(&mut c).await;
    c.finish();
}

// ── UI-03 ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn ui_03_cancel_from_the_window() {
    let mut c = Case::start("UI-03", "T2");
    c.real(&["dashboard server and web UI in a headless browser", "hub task service", "process cancellation"]);
    c.mocked(&["model output (fake-cli)"]);
    let step = json!({ "match": "[[UI-03]]", "text": "Working on it.", "hold": "never-released" });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step])], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let w = Window::open(&c, f.engine.clone(), &fx.path).await;
    assert!(w.ready().await, "the dashboard didn't open");

    w.page.fill("#prompt", "A long job. [[UI-03]]", None).await.unwrap();
    w.page.click(Some("#send"), None, None).await.unwrap();
    let running = w.until("!document.querySelector('#cancel').hidden").await
        && w.until("(() => { const t = S.tasks.get(S.sel); return !!t && t.status === 'running'; })()").await;
    let id = w.js("S.sel").await.as_u64().unwrap_or_default();
    // The agent is at work (its CLI was called) before the user cancels.
    let start = Instant::now();
    while f.calls("cl").iter().all(|x| !x["prompt"].as_str().is_some_and(|p| p.contains("[[UI-03]]"))) && start.elapsed() < WAIT {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    w.page.click(Some("#cancel"), None, None).await.unwrap();
    let shown = w.shows("⊘ cancelled").await;
    let gone = w.until("document.querySelector('#cancel').hidden").await;
    let task = w.api(&format!("/api/tasks/{id}")).await;
    c.json("task.json", &task);
    c.check(
        "cancelledFromTheWindow",
        running && shown && gone && task["status"] == "cancelled",
        format!(
            "Cancel offered while running: {running}; task {id} is {}; the conversation says so: {shown}; Cancel gone: {gone}",
            task["status"]
        ),
    );
    w.close(&mut c).await;
    c.finish();
}

// ── UI-04 ───────────────────────────────────────────────────────────────────

fn planner(tasks: Value) -> Value {
    let plan = json!({ "summary": "Plan for the dashboard case.", "tasks": tasks });
    json!({ "match": "You are the architect", "text": format!("Here is the plan.\n```json\n{plan}\n```") })
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_04_team_conflict_in_the_window() {
    let mut c = Case::start("UI-04", "T2");
    c.real(&["dashboard server and web UI in a headless browser", "fermi team runs", "git worktrees and merges", "the project's checks"]);
    c.mocked(&["model output (fake-cli)"]);
    let base = std::fs::read_to_string(acceptance_tests::repo_root().join("tests/fixtures/F-CODE/repo/engine/src/lib.rs")).unwrap();
    // Both change the same line.
    let v1 = base.replace("(normalize(name), helper())", "(normalize(name), helper().max(1))");
    let v2 = base.replace("(normalize(name), helper())", "(normalize(name).to_uppercase(), helper())");
    let both = base.replace("(normalize(name), helper())", "(normalize(name).to_uppercase(), helper().max(1))");
    let plan = planner(json!([
        { "id": "one", "role": "developer", "title": "Change one", "description": "Do [[ONE]]." },
        { "id": "two", "role": "developer", "title": "Change two", "description": "Do [[TWO]]." }
    ]));
    let a = json!({ "match": "[[ONE]]", "edits": [{ "path": "engine/src/lib.rs", "content": v1 }], "text": "one" });
    let b = json!({ "match": "[[TWO]]", "edits": [{ "path": "engine/src/lib.rs", "content": v2 }], "text": "two" });
    let dbg = json!({ "match": "Resolve merge conflict", "text": "I can't decide." });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![plan, a, b, dbg])], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let w = Window::open(&c, f.engine.clone(), &fx.path).await;
    assert!(w.ready().await, "the dashboard didn't open");

    // Teams → New team run: the goal, no final test or security pass, plan it, approve the plan.
    w.page.click(Some("#open-teams"), None, None).await.unwrap();
    w.page.fill(".tm-form textarea", "Two changes to greet", None).await.unwrap();
    w.page.click(None, Some("Final test pass on the merged result"), None).await.unwrap();
    w.page.click(None, Some("Final security review"), None).await.unwrap();
    let planned = w.page.click(None, Some("Plan it"), None).await;
    let started = match &planned {
        Ok(_) => w.page.click(None, Some("Approve and start"), Some(WAIT)).await,
        Err(e) => Err(anyhow::anyhow!("{e}")),
    };
    let form = w.js("document.querySelector('#tm-main').innerText.slice(0, 2000)").await;
    if !c.check("startedFromTheWindow", started.is_ok(), format!("{started:?}")) {
        c.text("team-view.txt", form.as_str().unwrap_or_default());
        w.close(&mut c).await;
        return c.finish();
    }

    // The conflict waits in the window: both patches, and the two ways out.
    let waiting = w.shows("I resolved it — continue").await && w.shows("Let an agent resolve it").await;
    let id = w.js("S.tm.sel").await.as_u64().unwrap_or_default();
    let run = w.api(&format!("/api/teams/{id}")).await;
    c.json("conflict-run.json", &run);
    let patches = w
        .js("[...document.querySelectorAll('#tm-main details')].map(d => d.querySelector('summary').innerText + '\\n' + d.querySelector('pre').textContent).join('\\n\\n')")
        .await;
    let patches = patches.as_str().unwrap_or_default().to_string();
    c.text("conflict-in-window.txt", &patches);
    let _ = w.page.screenshot(Some("conflict"), true).await;
    c.check(
        "conflictShownWithBothPatches",
        waiting
            && run["status"] == "attention"
            && patches.contains("Already merged")
            && patches.contains("engine/src/lib.rs")
            && patches.contains("+    (normalize(name), helper().max(1))")
            && patches.contains("+    (normalize(name).to_uppercase(), helper())"),
        format!("run {id} is {}; the window offers both choices: {waiting}; both sides' patches shown", run["status"]),
    );

    // Continuing before it's resolved is refused, and the window says why.
    w.allow(&format!("/api/teams/{id}/resolve"));
    w.page.click(None, Some("I resolved it — continue"), None).await.unwrap();
    let refused = w.until("document.querySelector('#toast').innerText.includes('still unmerged')").await;
    let toast = w.js("document.querySelector('#toast').innerText").await;
    c.check("unresolvedRefusedInTheWindow", refused, format!("toast: {toast}"));

    // The user resolves it in the integration worktree and continues.
    let wt = run["conflict"]["worktree"].as_str().map(std::path::PathBuf::from).unwrap_or_default();
    std::fs::write(wt.join("engine/src/lib.rs"), &both).unwrap();
    git(&wt, &["add", "engine/src/lib.rs"]).unwrap();
    w.page.click(None, Some("I resolved it — continue"), None).await.unwrap();
    let ready = w.shows("Checks passed: ready to merge").await;
    let run = w.api(&format!("/api/teams/{id}")).await;
    c.json("resolved-run.json", &run);
    let branch = run["integration"]["branch"].as_str().unwrap_or_default().to_string();
    let merged = git(&fx.path, &["show", &format!("{branch}:engine/src/lib.rs")]).unwrap_or_default();
    let merge_offered = w.js("[...document.querySelectorAll('#tm-main button')].some(b => b.innerText.startsWith('Merge into'))").await;
    let asked_agent = f.calls("cl").iter().filter_map(|x| x["prompt"].as_str()).any(|p| p.contains("Resolve merge conflict"));
    c.check(
        "resolvedFromTheWindow",
        ready && run["status"] == "ready" && run["conflict"].is_null() && merged == both.trim_end() && merge_offered == true && !asked_agent,
        format!("run {}; checks passed shown: {ready}; the integrated file is the user's resolution: {}; Merge offered: {merge_offered}; an agent was asked: {asked_agent}", run["status"], merged == both.trim_end()),
    );
    w.close(&mut c).await;
    c.finish();
}

// ── UI-05 ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn ui_05_phone_width() {
    let mut c = Case::start("UI-05", "T2");
    c.real(&["dashboard server and web UI in a headless browser (phone viewport, touch)"]);
    c.mocked(&["model output (fake-cli)"]);
    let f = Fermi::new(&c, &[account("cl", "claude", vec![])], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let w = Window::open(&c, f.engine.clone(), &fx.path).await;
    let layout = w.page.viewport(375, 812, true).await.unwrap_or_default();
    w.page.open(&format!("{}/", w.base)).await.unwrap(); // the key is kept for reloads
    let ready = w.ready().await;
    let state = w.page.state().await.unwrap_or_default();
    let prompt = w.page.check(Some("#prompt"), None).await.unwrap_or_default();
    let send = w.page.check(Some("#send"), None).await.unwrap_or_default();
    let _ = w.page.screenshot(Some("phone"), false).await;
    c.json("layout.json", &json!({ "after_resize": layout, "after_reload": state["layout"], "prompt": prompt, "send": send }));
    c.check(
        "usableAtPhoneWidth",
        ready
            && state["layout"]["width"] == 375
            && state["layout"]["overflow_x"] == false
            && [&prompt, &send].iter().all(|e| e["visible"] == true && e["in_viewport"] == true && e["uncovered"] == true),
        format!("opened: {ready}; layout {}; prompt and send visible, on screen and uncovered", state["layout"]),
    );
    w.close(&mut c).await;
    c.finish();
}
