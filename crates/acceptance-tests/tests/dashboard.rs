//! Dashboard tests (UI-01 … UI-08): the web UI (`crates/fermi/web/index.html`)
//! in a real headless browser against the real dashboard server, clicked
//! through as a user does. They aren't catalog cases (the 106 IDs live in
//! `tests/acceptance/systems.yaml`): the E2E workflows drive the dashboard's
//! HTTP API, and these guard the page on top of it. Only the model's output
//! comes from the fake CLI. Every case also fails on any page error.
//!
//! Each case also records which parts of the page's own script ran (the
//! browser's precise coverage, `frontend-coverage.json`);
//! `xtask dashboard-report` merges them into the frontend coverage.
//!
//!   cargo test -p acceptance-tests --locked --test dashboard

use acceptance_tests::{account, fixture, git, ground_truth, sha256, Case, Fermi};
use fermi_core::browser::{self, Browser};
use fermi_core::engine::Engine;
use serde_json::{json, Value};
use std::collections::BTreeMap;
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
    coverage: std::sync::Mutex<Coverage>,
    stop: tokio::sync::oneshot::Sender<()>,
    serving: tokio::task::JoinHandle<anyhow::Result<()>>,
}

/// What the page's own script ran, over every load in the case. Offsets are
/// the browser's: UTF-16 code units into the inline `<script>` of index.html.
#[derive(Default)]
struct Coverage {
    loads: usize,
    length: usize,
    ran: Vec<bool>,
    /// (start, end) → (name, ran at least once).
    functions: BTreeMap<(usize, usize), (String, bool)>,
}

impl Coverage {
    /// Adds one `Profiler.takePreciseCoverage` result for the script.
    fn add(&mut self, functions: &[Value]) {
        let range = |r: &Value| {
            let n = |k: &str| r[k].as_u64().unwrap_or(0) as usize;
            (n("startOffset"), n("endOffset"), n("count") > 0)
        };
        // Block ranges nest inside their function's range and override it,
        // so paint outer ranges first.
        let mut ranges: Vec<(usize, usize, bool)> =
            functions.iter().flat_map(|f| f["ranges"].as_array().into_iter().flatten().map(range)).collect();
        ranges.sort_by_key(|&(s, e, _)| (s, std::cmp::Reverse(e)));
        let length = ranges.iter().map(|r| r.1).max().unwrap_or(0);
        let mut ran = vec![false; length];
        for (s, e, hit) in ranges {
            ran[s..e].fill(hit);
        }
        if length > self.ran.len() {
            self.ran.resize(length, false);
        }
        self.ran.iter_mut().zip(&ran).for_each(|(a, b)| *a |= b);
        self.length = self.length.max(length);
        for f in functions {
            let Some(first) = f["ranges"].get(0).map(range) else { continue };
            // The script's own top level isn't a function.
            if first.0 == 0 && first.1 == length {
                continue;
            }
            let e = self
                .functions
                .entry((first.0, first.1))
                .or_insert_with(|| (f["functionName"].as_str().unwrap_or_default().to_string(), false));
            e.1 |= first.2;
        }
        self.loads += 1;
    }

    /// Executed stretches as `[start, end)` pairs.
    fn spans(&self) -> Vec<[usize; 2]> {
        let mut out: Vec<[usize; 2]> = vec![];
        for (i, &hit) in self.ran.iter().enumerate() {
            match out.last_mut() {
                Some(last) if hit && last[1] == i => last[1] = i + 1,
                _ if hit => out.push([i, i + 1]),
                _ => {}
            }
        }
        out
    }
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
        // Coverage from the start: what ran before enabling it would be missed.
        page.send("Profiler.enable", json!({})).await.unwrap();
        page.send("Profiler.startPreciseCoverage", json!({ "callCount": true, "detailed": true })).await.unwrap();
        let w = Window {
            base,
            token,
            http: reqwest::Client::new(),
            page,
            allowed: Default::default(),
            coverage: Default::default(),
            stop,
            serving,
        };
        w.page.open(&format!("{}/#t={}", w.base, w.token)).await.unwrap();
        w
    }

    /// Loads `url`, keeping the coverage of the page being left.
    async fn reopen(&self, url: &str) {
        self.take_coverage().await;
        self.page.open(url).await.unwrap();
    }

    /// Adds what the page's script ran since the last take (taking resets the counts).
    async fn take_coverage(&self) {
        let Ok(r) = self.page.send("Profiler.takePreciseCoverage", json!({})).await else { return };
        let mut cov = self.coverage.lock().unwrap();
        for s in r["result"].as_array().into_iter().flatten() {
            // The page's own inline script; the vendored terminal scripts aren't ours, and
            // evaluated snippets have no URL. (Its URL can hold the key, so it isn't kept.)
            let url = s["url"].as_str().unwrap_or_default();
            if url.starts_with(&self.base) && !url.contains("/vendor/") {
                cov.add(s["functions"].as_array().map(Vec::as_slice).unwrap_or_default());
            }
        }
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

    async fn api_q(&self, path: &str, query: &[(&str, &str)]) -> Value {
        let r = self.http.get(format!("{}{path}", self.base)).query(query).header("x-fermi-token", &self.token).send().await.unwrap();
        r.json().await.unwrap_or(Value::Null)
    }

    /// Clicks the first `selector` element whose text contains `text`, as a
    /// user picks a row (rows are plain elements the driver can't find by text).
    async fn pick(&self, selector: &str, text: &str) -> bool {
        let mark = format!(
            "(() => {{ document.querySelectorAll('[data-ui-pick]').forEach(e => delete e.dataset.uiPick);
               const e = [...document.querySelectorAll({})].find(e => e.innerText.includes({}));
               if (e) e.dataset.uiPick = ''; return !!e; }})()",
            json!(selector),
            json!(text)
        );
        self.until(&mark).await && self.page.click(Some("[data-ui-pick]"), None, None).await.is_ok()
    }

    /// The sections of a view under `root`: each heading with the names in its
    /// rows (as written: the headings are uppercased by CSS, which innerText applies).
    async fn sections(&self, root: &str) -> Value {
        self.js(&format!(
            "[...document.querySelectorAll({})].map(s => ({{ title: s.querySelector('h3')?.textContent || '',
               rows: [...s.querySelectorAll('.cx-row .n')].map(n => n.textContent) }}))",
            json!(format!("{root} .cx-sec"))
        ))
        .await
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
        self.take_coverage().await;
        let cov = std::mem::take(&mut *self.coverage.lock().unwrap());
        let functions: Vec<Value> = cov
            .functions
            .iter()
            .map(|(&(start, end), (name, ran))| json!({ "name": name, "start": start, "end": end, "ran": ran }))
            .collect();
        let ran_chars = cov.ran.iter().filter(|&&r| r).count();
        c.json(
            "frontend-coverage.json",
            &json!({ "script": "crates/fermi/web/index.html (inline script)", "offsets": "UTF-16 code units",
                     "loads": cov.loads, "length": cov.length, "ran": cov.spans(), "functions": functions }),
        );
        c.check(
            "frontendCoverageRecorded",
            cov.loads > 0 && ran_chars > 0,
            format!("{} load(s) of the page script ({} code units), {ran_chars} ran", cov.loads, cov.length),
        );
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

/// Nested block ranges override their function's count; loads add up.
#[test]
fn coverage_merges_nested_ranges_and_loads() {
    let r = |s: usize, e: usize, n: u64| json!({ "startOffset": s, "endOffset": e, "count": n });
    let load = |g: u64| {
        vec![
            json!({ "functionName": "", "ranges": [r(0, 100, 1)] }),
            json!({ "functionName": "f", "ranges": [r(10, 40, 1), r(20, 30, 0)] }),
            json!({ "functionName": "g", "ranges": [r(50, 60, g)] }),
        ]
    };
    let mut cov = Coverage::default();
    cov.add(&load(0));
    assert_eq!(cov.spans(), vec![[0, 20], [30, 50], [60, 100]]);
    assert_eq!(cov.functions.values().map(|f| (f.0.as_str(), f.1)).collect::<Vec<_>>(), vec![("f", true), ("g", false)]);
    cov.add(&load(1));
    assert_eq!(cov.spans(), vec![[0, 20], [30, 100]]);
    assert!(cov.functions[&(50, 60)].1);
    assert_eq!((cov.loads, cov.length), (2, 100));
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
    w.reopen(&format!("{}/", w.base)).await;
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
    w.reopen(&format!("{}/", w.base)).await; // the key is kept for reloads
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

/// The row names of the first section whose heading starts with `title`.
fn rows(sections: &Value, title: &str) -> Vec<String> {
    sections
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["title"].as_str().is_some_and(|t| t.starts_with(title)))
        .map(|s| s["rows"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

/// Whether each oracle id (`a::b::name`) names one of the rows.
fn lists_all(rows: &[String], ids: &[String]) -> bool {
    ids.iter().all(|id| {
        let name = id.rsplit("::").next().unwrap_or(id);
        rows.iter().any(|r| r == name || r.ends_with(&format!("::{name}")))
    })
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect()
}

// ── UI-06 ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn ui_06_code_map() {
    let mut c = Case::start("UI-06", "T2");
    c.real(&["dashboard server and web UI in a headless browser", "code index on the F-CODE fixture", "git (uncommitted changes)"]);
    let f = Fermi::new(&c, &[account("cl", "claude", vec![])], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let refs = ground_truth("F-CODE", "references.json");
    let graph = ground_truth("F-CODE", "dependency-graph.json");
    let w = Window::open(&c, f.engine.clone(), &fx.path).await;
    assert!(w.ready().await, "the dashboard didn't open");

    // Code map → search → the symbol, with its callers and the tests reaching it.
    w.page.click(Some("#open-code"), None, None).await.unwrap();
    let indexed = w.until("document.querySelector('#cx-stats').innerText.includes('symbols')").await;
    let stats = w.js("document.querySelector('#cx-stats').innerText").await;
    w.page.fill("#cx-q", "process", None).await.unwrap();
    let picked = w.pick("#cx-results .cx-hit", "engine/src/lib.rs:5").await;
    let shown = w.until("document.querySelector('#cx-main h2')?.innerText.endsWith('process')").await;
    let symbol = w.sections("#cx-main").await;
    let callers = strings(&json!(refs["callers"]["engine::process"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| c["caller"].clone())
        .collect::<Vec<_>>()));
    let tests: Vec<String> = callers.iter().filter(|c| c.contains("::tests::")).cloned().collect();
    let called_by = rows(&symbol, "Called by");
    c.json("symbol-view.json", &json!({ "stats": stats, "sections": symbol, "oracle_callers": callers }));
    c.check(
        "callersAndTestsShown",
        indexed
            && picked
            && shown
            && called_by.len() == callers.len()
            && lists_all(&called_by, &callers)
            && lists_all(&rows(&symbol, "Tests reaching it"), &tests),
        format!(
            "indexed: {indexed}; symbol shown: {shown}; called by {called_by:?}, the oracle {callers:?}; tests {:?}",
            rows(&symbol, "Tests reaching it")
        ),
    );

    // Its location opens the file in the editor.
    w.page.click(Some("#cx-main .cx-loc a"), None, None).await.unwrap();
    let disk = std::fs::read_to_string(fx.path.join("engine/src/lib.rs")).unwrap();
    let opened = w
        .until(&format!("!document.querySelector('#view-file').hidden && document.querySelector('#code').value === {}", json!(disk)))
        .await;
    c.check("opensTheDefinition", opened, format!("engine/src/lib.rs open in the editor with its contents: {opened}"));

    // An uncommitted change to normalize: what it could break and the tests to run.
    let lib = fx.path.join("util/src/lib.rs");
    let src = std::fs::read_to_string(&lib).unwrap();
    std::fs::write(&lib, src.replace("s.trim().to_lowercase()", "s.trim().to_lowercase().replace(' ', \"_\")")).unwrap();
    w.page.click(Some("#open-code"), None, None).await.unwrap();
    w.page.click(Some("#cx-impact"), None, None).await.unwrap();
    let impact_shown = w.until("document.querySelector('#cx-main h2')?.innerText.startsWith('Impact of your changes')").await;
    let impact = w.sections("#cx-main").await;
    let oracle = &graph["impact"]["util::normalize"];
    let (affected, to_run) = (strings(&oracle["affectedSymbols"]), strings(&oracle["tests"]));
    let status = git(&fx.path, &["status", "--porcelain"]).unwrap_or_default();
    c.json("impact-view.json", &json!({ "sections": impact, "oracle": oracle, "git_status": status }));
    c.check(
        "impactOfUncommittedChange",
        impact_shown
            && lists_all(&rows(&impact, "Changed"), &["util::normalize".to_string()])
            && lists_all(&rows(&impact, "Could break"), &affected)
            && lists_all(&rows(&impact, "Tests to run"), &to_run),
        format!(
            "changed {:?}; could break {:?} (oracle {affected:?}); tests to run {:?} (oracle {to_run:?})",
            rows(&impact, "Changed"),
            rows(&impact, "Could break"),
            rows(&impact, "Tests to run")
        ),
    );
    c.check("readOnly", status.trim() == "M util/src/lib.rs", format!("the repository after browsing: {:?}", status.trim()));
    w.close(&mut c).await;
    c.finish();
}

// ── UI-07 ───────────────────────────────────────────────────────────────────

/// Fills the `n`th field of the memory form.
async fn mm_field(w: &Window, n: usize, tag: &str, value: &str) {
    w.page.fill(&format!("#mm-main form label:nth-of-type({n}) {tag}"), value, None).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn ui_07_memory_reaches_the_next_task() {
    let mut c = Case::start("UI-07", "T2");
    c.real(&["dashboard server and web UI in a headless browser", "fermi memory", "hub (context assembly for a new task)"]);
    c.mocked(&["model output (fake-cli); the prompt it received is the assembled context"]);
    let step = json!({ "match": "[[UI-07]]", "text": "PostgreSQL, as the team decided." });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step])], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let w = Window::open(&c, f.engine.clone(), &fx.path).await;
    assert!(w.ready().await, "the dashboard didn't open");
    let root = fx.path.to_string_lossy().to_string();

    // Memory → New record: a decision resting on a file.
    w.page.click(Some("#open-memory"), None, None).await.unwrap();
    w.until("document.querySelector('#mm-stats').innerText.includes('record')").await;
    let title = "Use PostgreSQL for the storage layer";
    w.page.click(Some("#mm-new"), None, None).await.unwrap();
    mm_field(&w, 2, "input", title).await;
    mm_field(&w, 3, "input", "storage").await;
    mm_field(&w, 4, "textarea", "Chosen over SQLite: several writers at once.").await;
    mm_field(&w, 5, "input", "util/src/lib.rs:5").await;
    w.page.click(Some("#mm-main form button.primary"), None, None).await.unwrap();
    let saved = w.until(&format!("document.querySelector('#mm-main h2')?.innerText === {}", json!(title))).await;
    let view = w.js("document.querySelector('#mm-main').innerText").await;
    c.check(
        "recordedFromTheWindow",
        saved && view.as_str().is_some_and(|v| v.contains("recorded by user (you)") && v.contains("util/src/lib.rs:5")),
        format!("shown after saving: {saved}; the record reads {view}"),
    );

    // A new version, with its reason; the first stays in the history.
    let title2 = "Use PostgreSQL 16 for the storage layer";
    w.page.click(None, Some("New version"), None).await.unwrap();
    mm_field(&w, 1, "input", title2).await;
    mm_field(&w, 3, "input", "Pinned the major version").await;
    w.page.click(Some("#mm-main form button.primary"), None, None).await.unwrap();
    let shown = w.until(&format!("document.querySelector('#mm-main h2')?.innerText === {}", json!(title2))).await;
    let page = w.js("document.querySelector('#mm-main').textContent").await;
    let page = page.as_str().unwrap_or_default();
    let revised = shown
        && page.contains("version 2")
        && page.contains("Why it changed: Pinned the major version")
        && page.contains("Earlier versions")
        && page.contains(&format!("v1 · {title}"));
    c.text("revised-record.txt", page);
    c.check("revisedWithHistory", revised, format!("version 2 shown with the reason and version 1 in the history: {revised}"));

    // A private note on the same topic.
    let canary = "PRIV-CANARY-UI07";
    w.page.click(Some("#mm-new"), None, None).await.unwrap();
    mm_field(&w, 2, "input", &format!("Private: rotate the database password {canary}")).await;
    mm_field(&w, 3, "input", "storage").await;
    w.js("(() => { const s = document.querySelector('#mm-main form label:nth-of-type(6) select'); s.value = 'private'; s.dispatchEvent(new Event('change', { bubbles: true })); })()").await;
    w.page.click(Some("#mm-main form button.primary"), None, None).await.unwrap();
    w.until(&format!("document.querySelector('#mm-main h2')?.innerText.includes({})", json!(canary))).await;
    w.page.fill("#mm-q", "PostgreSQL", None).await.unwrap();
    let found = w.until(&format!("document.querySelector('#mm-list').innerText.includes({})", json!(title2))).await;
    let records = w.api_q("/api/memory", &[("root", &root)]).await;
    c.json("records.json", &records);
    let list = records["records"].as_array().cloned().unwrap_or_default();
    let decision = list.iter().find(|r| r["title"] == title2).cloned().unwrap_or_default();
    let private = list.iter().find(|r| r["title"].as_str().is_some_and(|t| t.contains(canary))).cloned().unwrap_or_default();
    c.check(
        "storedAsShown",
        found && decision["version"] == 2 && decision["author"] == "user" && private["audience"] == "private",
        format!(
            "search finds it: {found}; decision v{} by {}; the note is {}",
            decision["version"], decision["author"], private["audience"]
        ),
    );

    // The next task gets the decision with its author and source, never the private note.
    w.page.click(Some("#new-task"), None, None).await.unwrap();
    w.page.fill("#prompt", "Which database should the storage layer use? [[UI-07]]", None).await.unwrap();
    w.page.click(Some("#send"), None, None).await.unwrap();
    let done = w.shows("PostgreSQL, as the team decided.").await && w.until("!!document.querySelector('#log .ev.ok')").await;
    let prompt =
        f.calls("cl").iter().filter_map(|x| x["prompt"].as_str().map(String::from)).find(|p| p.contains("[[UI-07]]")).unwrap_or_default();
    c.text("assembled-context.txt", &prompt);
    let id = decision["id"].as_str().unwrap_or("?");
    c.check(
        "reachesTheNextTask",
        done && prompt.contains(id)
            && prompt.contains(title2)
            && prompt.contains("author=\"user\"")
            && prompt.contains("util/src/lib.rs:5"),
        format!("task done: {done}; {id} in its context with the author and source: {}", prompt.contains(id)),
    );
    c.check(
        "privateStaysPrivate",
        !prompt.is_empty() && !prompt.contains(canary),
        format!("the private note in the context: {}", prompt.contains(canary)),
    );
    w.close(&mut c).await;
    c.finish();
}

// ── UI-08 ───────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn ui_08_files_and_editor() {
    let mut c = Case::start("UI-08", "T2");
    c.real(&["dashboard server and web UI in a headless browser", "the workspace on disk", "git (the Changes panel)"]);
    let f = Fermi::new(&c, &[account("cl", "claude", vec![])], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let w = Window::open(&c, f.engine.clone(), &fx.path).await;
    assert!(w.ready().await, "the dashboard didn't open");

    // Files: util → src → text.rs opens in the editor with what's on disk.
    let path = fx.path.join("util/src/text.rs");
    let disk = std::fs::read_to_string(&path).unwrap();
    let walked = w.pick("#tree .node", "util").await && w.pick("#tree .node", "src").await && w.pick("#tree .node", "text.rs").await;
    let opened = w
        .until(&format!("!document.querySelector('#view-file').hidden && document.querySelector('#code').value === {}", json!(disk)))
        .await;
    c.check("opensFromTheTree", walked && opened, format!("walked the tree: {walked}; the editor holds the file: {opened}"));

    // Edit and save: the file on disk is what was typed.
    let edited = format!("{disk}\n/// Edited in the window.\npub fn shout(s: &str) -> String {{\n    s.to_uppercase()\n}}\n");
    w.page.fill("#code", &edited, None).await.unwrap();
    let dirty = w.js("!document.querySelector('#file-save').disabled").await == true;
    w.page.click(Some("#file-save"), None, None).await.unwrap();
    let toast = w.shows("Saved text.rs").await;
    let now = std::fs::read_to_string(&path).unwrap_or_default();
    let clean = w.js("document.querySelector('#file-save').disabled").await == true;
    c.check(
        "savedToDisk",
        dirty && toast && now == edited && clean,
        format!(
            "Save enabled after typing: {dirty}; saved: {toast}; the file on disk is the edit: {}; Save disabled again: {clean}",
            now == edited
        ),
    );

    // Changes lists the edit; a contents search finds a line.
    w.page.click(Some("#ptab-changes"), None, None).await.unwrap();
    let listed = w.until("document.querySelector('#changes').innerText.includes('text.rs')").await;
    w.page.click(Some("#ptab-files"), None, None).await.unwrap();
    w.page.click(Some("#find-contents"), None, None).await.unwrap();
    w.page.fill("#find", "to_lowercase", None).await.unwrap();
    let found = w
        .until("[...document.querySelectorAll('#tree .hit')].some(h => h.innerText.includes('lib.rs') && h.innerText.includes('6:'))")
        .await;
    let hits = w.js("document.querySelector('#tree').innerText").await;
    c.json("search.json", &json!({ "query": "to_lowercase", "shown": hits }));
    c.check(
        "changesAndSearch",
        listed && found,
        format!("Changes lists text.rs: {listed}; the search finds util/src/lib.rs line 6: {found}"),
    );

    // The window's API writes only inside the workspaces.
    let outside = c.work.join("outside.txt");
    w.allow("/api/fs/write");
    let refused = w
        .js(&format!(
            "api('/api/fs/write', {{ path: {}, text: 'x' }}).then(() => 'written', e => e.message)",
            json!(outside.to_string_lossy())
        ))
        .await;
    c.check(
        "writesStayInTheWorkspace",
        refused != "written" && !outside.exists(),
        format!("a write outside the workspace: {refused}; the file exists: {}", outside.exists()),
    );
    w.close(&mut c).await;
    c.finish();
}
