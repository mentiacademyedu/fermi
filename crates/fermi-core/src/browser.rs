//! A browser agents can drive: open a page, click, fill, wait, read, check
//! layout, take screenshots, and see what went wrong (console errors, failed
//! requests). fermi starts a headless Chromium-based browser (Chrome, Edge or
//! Chromium, or `[browser] path`) with its own profile and talks to it over
//! the DevTools protocol.
//!
//! Actions wait for what they need: an element is clicked only once it is
//! there, visible, enabled and not covered, and a wait that runs out is an
//! error, never a success. Opening a page fails if it doesn't load or the
//! server answers with an error. Everything is recorded in a trace (actions,
//! navigations, console messages, failed requests; redacted) next to the
//! screenshots, in `~/.fermi/browser/<task>/`.

use crate::redact::Redactor;
use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use tokio_tungstenite::tungstenite::Message;

const DEFAULT_WAIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(50);

/// The browser to start: `[browser] path`, else the first one found.
pub fn find(cfg: &crate::config::BrowserCfg) -> Option<PathBuf> {
    if let Some(p) = cfg.path.as_deref() {
        return Some(crate::util::expand(p));
    }
    let mut cands: Vec<PathBuf> = vec![];
    if cfg!(windows) {
        for var in ["ProgramFiles(x86)", "ProgramFiles", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var).map(PathBuf::from) {
                cands.push(base.join("Microsoft/Edge/Application/msedge.exe"));
                cands.push(base.join("Google/Chrome/Application/chrome.exe"));
            }
        }
    } else if cfg!(target_os = "macos") {
        for app in ["Google Chrome", "Microsoft Edge", "Chromium"] {
            cands.push(PathBuf::from(format!("/Applications/{app}.app/Contents/MacOS/{app}")));
        }
    }
    for name in ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge", "msedge", "chrome"] {
        if let Ok(p) = crate::util::resolve_bin(name, None) {
            cands.push(p);
        }
    }
    cands.into_iter().find(|p| p.is_file())
}

/// Whether agents get the browser tools.
pub fn available(cfg: &crate::config::BrowserCfg) -> bool {
    cfg.enabled != Some(false) && find(cfg).is_some()
}

/// What the browser has reported so far.
#[derive(Default)]
struct Seen {
    console: Vec<Value>,
    failed: Vec<Value>,
    requests: HashMap<String, (String, String)>,
    /// Main-document status by loader id.
    documents: HashMap<String, i64>,
    navigations: Vec<String>,
    started_loading: u64,
    loads: u64,
}

struct Trace {
    file: std::fs::File,
    start: Instant,
    redactor: Arc<Redactor>,
}

impl Trace {
    fn write(&mut self, mut v: Value) {
        v["t_ms"] = json!(self.start.elapsed().as_millis() as u64);
        v["at"] = json!(crate::util::now_ms());
        self.redactor.value(&mut v);
        let _ = std::io::Write::write_all(&mut self.file, format!("{v}\n").as_bytes());
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;
type Sink =
    futures_util::stream::SplitSink<tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, Message>;

/// One running browser and its page.
pub struct Browser {
    child: tokio::process::Child,
    sink: tokio::sync::Mutex<Sink>,
    next: AtomicU64,
    pending: Pending,
    seen: Arc<Mutex<Seen>>,
    trace: Arc<Mutex<Trace>>,
    pub dir: PathBuf,
    shots: AtomicU64,
}

impl Drop for Browser {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            crate::providers::proc::kill_tree(pid);
        }
    }
}

fn first_text(args: &Value) -> String {
    args.as_array()
        .into_iter()
        .flatten()
        .map(|a| {
            a["value"]
                .as_str()
                .map(str::to_string)
                .or_else(|| a["description"].as_str().map(str::to_string))
                .unwrap_or_else(|| a["value"].to_string())
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl Browser {
    /// Starts a headless browser whose trace and screenshots go to `dir`.
    pub async fn launch(cfg: &crate::config::BrowserCfg, dir: &Path, redactor: Arc<Redactor>) -> Result<Browser> {
        let bin = find(cfg).ok_or_else(|| anyhow!("no Chrome, Edge or Chromium found; set [browser] path"))?;
        std::fs::create_dir_all(dir)?;
        let profile = dir.join("profile");
        let _ = std::fs::remove_dir_all(&profile);
        std::fs::create_dir_all(&profile)?;
        let mut cmd = tokio::process::Command::new(&bin);
        cmd.args([
            "--headless=new",
            "--remote-debugging-port=0",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-gpu",
            "--disable-extensions",
            "--disable-background-networking",
            "--disable-sync",
            "--disable-features=Translate,MediaRouter",
            "--mute-audio",
            "--hide-scrollbars",
            "--window-size=1280,800",
        ])
        .arg(format!("--user-data-dir={}", profile.display()));
        if cfg.no_sandbox || std::env::var_os("FERMI_BROWSER_NO_SANDBOX").is_some() {
            cmd.arg("--no-sandbox");
        }
        cmd.arg("about:blank")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().with_context(|| format!("starting {}", bin.display()))?;

        // The browser writes its debugging port to the profile once it's ready.
        // A cold start on a busy machine can take a while; a browser that
        // exits is reported at once.
        let port_file = profile.join("DevToolsActivePort");
        let deadline = Instant::now() + Duration::from_secs(60);
        let port: u16 = loop {
            if let Some(p) = std::fs::read_to_string(&port_file).ok().and_then(|t| t.lines().next()?.trim().parse().ok()) {
                break p;
            }
            if let Ok(Some(status)) = child.try_wait() {
                bail!("{} exited while starting ({status})", bin.display());
            }
            if Instant::now() > deadline {
                bail!("{} didn't start (no DevToolsActivePort after 60 s)", bin.display());
            }
            tokio::time::sleep(POLL).await;
        };
        let list: Value = reqwest::get(format!("http://127.0.0.1:{port}/json/list")).await?.json().await?;
        let ws_url = list
            .as_array()
            .into_iter()
            .flatten()
            .find(|t| t["type"] == "page")
            .and_then(|t| t["webSocketDebuggerUrl"].as_str())
            .ok_or_else(|| anyhow!("the browser has no page to drive"))?
            .to_string();
        let (ws, _) = tokio_tungstenite::connect_async(ws_url.as_str()).await.context("connecting to the browser")?;
        let (sink, mut stream) = ws.split();

        let trace_file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("trace.jsonl"))?;
        let trace = Arc::new(Mutex::new(Trace { file: trace_file, start: Instant::now(), redactor }));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let seen = Arc::new(Mutex::new(Seen::default()));
        let (p2, s2, t2) = (pending.clone(), seen.clone(), trace.clone());
        tokio::spawn(async move {
            while let Some(Ok(msg)) = stream.next().await {
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(text.as_str()) else { continue };
                if let Some(id) = v["id"].as_u64() {
                    if let Some(tx) = p2.lock().unwrap().remove(&id) {
                        let _ = tx.send(v);
                    }
                    continue;
                }
                Self::observe(&v, &s2, &t2);
            }
        });

        let b = Browser {
            child,
            sink: tokio::sync::Mutex::new(sink),
            next: AtomicU64::new(0),
            pending,
            seen,
            trace,
            dir: dir.to_path_buf(),
            shots: AtomicU64::new(0),
        };
        for domain in ["Page.enable", "Runtime.enable", "Network.enable", "Log.enable"] {
            b.send(domain, json!({})).await?;
        }
        b.record(json!({ "kind": "launch", "browser": bin.display().to_string() }));
        Ok(b)
    }

    /// Folds one browser event into what's been seen, and the trace.
    fn observe(v: &Value, seen: &Mutex<Seen>, trace: &Mutex<Trace>) {
        let p = &v["params"];
        let mut s = seen.lock().unwrap();
        let log = |e: Value| trace.lock().unwrap().write(e);
        match v["method"].as_str().unwrap_or_default() {
            "Runtime.consoleAPICalled" if matches!(p["type"].as_str(), Some("error") | Some("warning") | Some("assert")) => {
                let e = json!({ "kind": "console", "level": p["type"], "text": first_text(&p["args"]),
                    "source": p["stackTrace"]["callFrames"][0]["url"] });
                s.console.push(e.clone());
                log(e);
            }
            "Runtime.exceptionThrown" => {
                let d = &p["exceptionDetails"];
                let text = d["exception"]["description"].as_str().or(d["text"].as_str()).unwrap_or("exception").to_string();
                let e = json!({ "kind": "console", "level": "exception", "text": text, "source": d["url"], "line": d["lineNumber"] });
                s.console.push(e.clone());
                log(e);
            }
            "Log.entryAdded" if p["entry"]["level"] == "error" => {
                let e = json!({ "kind": "console", "level": "error", "text": p["entry"]["text"], "source": p["entry"]["url"] });
                s.console.push(e.clone());
                log(e);
            }
            "Network.requestWillBeSent" => {
                let id = p["requestId"].as_str().unwrap_or_default().to_string();
                s.requests.insert(
                    id,
                    (p["request"]["method"].as_str().unwrap_or("GET").into(), p["request"]["url"].as_str().unwrap_or_default().into()),
                );
            }
            "Network.responseReceived" => {
                let status = p["response"]["status"].as_i64().unwrap_or(0);
                if p["type"] == "Document" {
                    if let Some(l) = p["loaderId"].as_str() {
                        s.documents.insert(l.to_string(), status);
                    }
                }
                if status >= 400 {
                    let (method, _) = s.requests.get(p["requestId"].as_str().unwrap_or_default()).cloned().unwrap_or_default();
                    let e =
                        json!({ "kind": "network", "url": p["response"]["url"], "method": method, "status": status, "type": p["type"] });
                    s.failed.push(e.clone());
                    log(e);
                }
            }
            "Network.loadingFailed" if p["canceled"] != true => {
                let (method, url) = s.requests.get(p["requestId"].as_str().unwrap_or_default()).cloned().unwrap_or_default();
                let e =
                    json!({ "kind": "network", "url": url, "method": method, "status": null, "error": p["errorText"], "type": p["type"] });
                s.failed.push(e.clone());
                log(e);
            }
            "Page.frameNavigated" if p["frame"]["parentId"].is_null() => {
                let url = p["frame"]["url"].as_str().unwrap_or_default().to_string();
                s.navigations.push(url.clone());
                log(json!({ "kind": "navigation", "url": url }));
            }
            "Page.frameStartedLoading" => s.started_loading += 1,
            "Page.loadEventFired" => s.loads += 1,
            _ => {}
        }
    }

    fn record(&self, v: Value) {
        self.trace.lock().unwrap().write(v);
    }

    pub async fn send(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({ "id": id, "method": method, "params": params });
        self.sink.lock().await.send(Message::text(msg.to_string())).await.context("the browser connection closed")?;
        let reply = tokio::time::timeout(Duration::from_secs(30), rx)
            .await
            .map_err(|_| anyhow!("the browser didn't answer {method} within 30 s"))?
            .map_err(|_| anyhow!("the browser connection closed"))?;
        if let Some(e) = reply.get("error") {
            bail!("{method}: {}", e["message"].as_str().unwrap_or("error"));
        }
        Ok(reply["result"].clone())
    }

    /// Runs JavaScript in the page and returns its (JSON) value.
    async fn eval(&self, expr: &str) -> Result<Value> {
        let r = self.send("Runtime.evaluate", json!({ "expression": expr, "returnByValue": true, "awaitPromise": true })).await?;
        if let Some(d) = r.get("exceptionDetails") {
            bail!("script error: {}", d["exception"]["description"].as_str().or(d["text"].as_str()).unwrap_or("error"));
        }
        Ok(r["result"]["value"].clone())
    }

    async fn page(&self) -> Value {
        self.eval("({ url: location.href, title: document.title, page: (document.body && document.body.dataset.page) || null })")
            .await
            .unwrap_or(Value::Null)
    }

    async fn wait_loaded(&self, deadline: Instant) -> Result<()> {
        loop {
            if self.eval("document.readyState").await.ok().and_then(|v| v.as_str().map(str::to_string)).as_deref() == Some("complete") {
                return Ok(());
            }
            if Instant::now() > deadline {
                bail!("the page didn't finish loading");
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Opens `url`. Fails if it can't be reached or the server answers with an error.
    pub async fn open(&self, url: &str) -> Result<Value> {
        let started = Instant::now();
        let r = self.send("Page.navigate", json!({ "url": url })).await?;
        if let Some(err) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
            self.record(json!({ "kind": "action", "action": "open", "url": url, "ok": false, "error": err }));
            bail!("couldn't open {url}: {err}");
        }
        self.wait_loaded(started + Duration::from_secs(30)).await?;
        let loader = r["loaderId"].as_str().unwrap_or_default().to_string();
        let mut status = None;
        for _ in 0..40 {
            status = self.seen.lock().unwrap().documents.get(&loader).copied();
            if status.is_some() {
                break;
            }
            tokio::time::sleep(POLL).await;
        }
        let page = self.page().await;
        let ok = status.is_none_or(|s| s < 400);
        self.record(json!({ "kind": "action", "action": "open", "url": url, "ok": ok, "status": status, "page": page, "ms": started.elapsed().as_millis() as u64 }));
        if !ok {
            bail!("{url} answered HTTP {}", status.unwrap_or_default());
        }
        Ok(json!({ "url": page["url"], "title": page["title"], "page": page["page"], "status": status }))
    }

    /// The state of the element `selector` (CSS) or the control labelled `text`.
    async fn probe(&self, selector: Option<&str>, text: Option<&str>, scroll: bool) -> Result<Value> {
        let js = format!(
            r#"(() => {{
  const sel = {sel}, text = {text};
  const label = (e) => (e.innerText || e.value || e.getAttribute('aria-label') || '').trim().toLowerCase();
  let el = null;
  if (sel) el = document.querySelector(sel);
  else if (text) {{
    const want = text.trim().toLowerCase();
    const all = [...document.querySelectorAll('button, a, [role=button], input[type=submit], input[type=button], summary, label')];
    el = all.find(e => label(e) === want) || all.find(e => label(e).includes(want));
  }}
  if (!el) return {{ exists: false }};
  if ({scroll}) el.scrollIntoView({{ block: 'center', inline: 'center' }});
  const r = el.getBoundingClientRect(), cs = getComputedStyle(el);
  const visible = r.width > 0 && r.height > 0 && cs.visibility !== 'hidden' && cs.display !== 'none' && Number(cs.opacity) > 0;
  const enabled = !el.disabled && el.getAttribute('aria-disabled') !== 'true' && !el.closest('fieldset[disabled]');
  const cx = r.x + r.width / 2, cy = r.y + r.height / 2;
  const hit = document.elementFromPoint(cx, cy);
  const uncovered = !!hit && (hit === el || el.contains(hit) || hit.contains(el));
  const in_viewport = r.bottom > 0 && r.right > 0 && r.top < innerHeight && r.left < innerWidth;
  return {{ exists: true, visible, enabled, uncovered, in_viewport, x: cx, y: cy,
    box: {{ x: r.x, y: r.y, width: r.width, height: r.height }}, tag: el.tagName.toLowerCase(),
    text: (el.innerText || el.value || '').trim().slice(0, 200) }};
}})()"#,
            sel = serde_json::to_string(&selector).unwrap_or_default(),
            text = serde_json::to_string(&text).unwrap_or_default(),
            scroll = scroll
        );
        self.eval(&js).await
    }

    fn what(selector: Option<&str>, text: Option<&str>) -> String {
        match (selector, text) {
            (Some(s), _) => format!("`{s}`"),
            (None, Some(t)) => format!("\"{t}\""),
            _ => "the element".into(),
        }
    }

    /// Waits until the element is in `state` (visible, enabled, hidden).
    /// Returns its last observed state; running out of time is an error.
    pub async fn wait(&self, selector: Option<&str>, text: Option<&str>, state: &str, timeout: Option<Duration>) -> Result<Value> {
        let started = Instant::now();
        let deadline = started + timeout.unwrap_or(DEFAULT_WAIT);
        let mut changes: Vec<Value> = vec![];
        let last = loop {
            let s = self.probe(selector, text, false).await.unwrap_or(json!({ "exists": false }));
            let key = json!({ "exists": s["exists"], "visible": s["visible"], "enabled": s["enabled"] });
            if changes.last().map(|c| &c["state"]) != Some(&key) {
                changes.push(json!({ "t_ms": started.elapsed().as_millis() as u64, "state": key }));
            }
            let done = match state {
                "hidden" => s["exists"] != true || s["visible"] != true,
                "visible" => s["visible"] == true,
                _ => s["visible"] == true && s["enabled"] == true,
            };
            if done {
                break s;
            }
            if Instant::now() > deadline {
                self.record(json!({ "kind": "action", "action": "wait", "target": Self::what(selector, text), "state": state, "ok": false, "changes": changes }));
                bail!("{} wasn't {state} after {} ms (last seen: {s})", Self::what(selector, text), started.elapsed().as_millis());
            }
            tokio::time::sleep(POLL).await;
        };
        let waited = started.elapsed().as_millis() as u64;
        self.record(json!({ "kind": "action", "action": "wait", "target": Self::what(selector, text), "state": state, "ok": true, "waited_ms": waited, "changes": changes }));
        Ok(json!({ "waited_ms": waited, "element": last, "changes": changes }))
    }

    /// Clicks once the element is visible, enabled and not covered.
    pub async fn click(&self, selector: Option<&str>, text: Option<&str>, timeout: Option<Duration>) -> Result<Value> {
        let started = Instant::now();
        let deadline = started + timeout.unwrap_or(DEFAULT_WAIT);
        let target = loop {
            let s = self.probe(selector, text, true).await.unwrap_or(json!({ "exists": false }));
            if s["visible"] == true && s["enabled"] == true && s["uncovered"] == true {
                break s;
            }
            if Instant::now() > deadline {
                self.record(json!({ "kind": "action", "action": "click", "target": Self::what(selector, text), "ok": false, "last": s }));
                bail!(
                    "couldn't click {}: not ready after {} ms (last seen: {s})",
                    Self::what(selector, text),
                    started.elapsed().as_millis()
                );
            }
            tokio::time::sleep(POLL).await;
        };
        let (loads, starts) = {
            let s = self.seen.lock().unwrap();
            (s.loads, s.started_loading)
        };
        let (x, y) = (target["x"].as_f64().unwrap_or(0.0), target["y"].as_f64().unwrap_or(0.0));
        self.send("Input.dispatchMouseEvent", json!({ "type": "mouseMoved", "x": x, "y": y })).await?;
        for kind in ["mousePressed", "mouseReleased"] {
            self.send("Input.dispatchMouseEvent", json!({ "type": kind, "x": x, "y": y, "button": "left", "clickCount": 1 })).await?;
        }
        // A click that starts a navigation returns once the new page has loaded.
        let mut navigated = false;
        let soon = Instant::now() + Duration::from_millis(400);
        while Instant::now() < soon {
            if self.seen.lock().unwrap().started_loading > starts {
                navigated = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if navigated {
            let until = Instant::now() + Duration::from_secs(30);
            while self.seen.lock().unwrap().loads <= loads && Instant::now() < until {
                tokio::time::sleep(POLL).await;
            }
            self.wait_loaded(until).await?;
        }
        let page = self.page().await;
        self.record(json!({ "kind": "action", "action": "click", "target": Self::what(selector, text), "ok": true,
            "waited_ms": started.elapsed().as_millis() as u64, "navigated": navigated, "url": page["url"], "element": target["text"] }));
        Ok(json!({ "clicked": target["text"], "navigated": navigated, "url": page["url"], "title": page["title"], "page": page["page"],
            "waited_ms": started.elapsed().as_millis() as u64 }))
    }

    /// Types `value` into a field once it's visible and enabled (replacing what's there).
    pub async fn fill(&self, selector: &str, value: &str, timeout: Option<Duration>) -> Result<Value> {
        let w = self.wait(Some(selector), None, "enabled", timeout).await?;
        let sel = serde_json::to_string(selector)?;
        self.eval(&format!(
            "(() => {{ const e = document.querySelector({sel}); e.focus(); if ('value' in e) {{ e.value = ''; e.dispatchEvent(new Event('input', {{ bubbles: true }})); }} return true; }})()"
        ))
        .await?;
        self.send("Input.insertText", json!({ "text": value })).await?;
        let now = self.eval(&format!(
            "(() => {{ const e = document.querySelector({sel}); e.dispatchEvent(new Event('change', {{ bubbles: true }})); return e.value ?? e.innerText; }})()"
        ))
        .await?;
        self.record(json!({ "kind": "action", "action": "fill", "target": selector, "ok": now == value, "waited_ms": w["waited_ms"] }));
        if now != value {
            bail!("`{selector}` holds {now} after typing, not {value:?}");
        }
        Ok(json!({ "filled": selector, "value": now, "waited_ms": w["waited_ms"] }))
    }

    /// The text of the page (or of `selector`), with where we are.
    pub async fn read(&self, selector: Option<&str>) -> Result<Value> {
        let sel = serde_json::to_string(&selector)?;
        let text = self
            .eval(&format!(
                "(() => {{ const s = {sel}; const e = s ? document.querySelector(s) : document.body; return e ? e.innerText : null; }})()"
            ))
            .await?;
        if text.is_null() {
            bail!("no element matches {}", selector.unwrap_or("the page"));
        }
        let page = self.page().await;
        self.record(json!({ "kind": "action", "action": "read", "target": selector, "ok": true, "url": page["url"] }));
        Ok(json!({ "url": page["url"], "title": page["title"], "page": page["page"], "text": text }))
    }

    /// Whether an element is there, visible, enabled and inside the viewport.
    pub async fn check(&self, selector: Option<&str>, text: Option<&str>) -> Result<Value> {
        let s = self.probe(selector, text, false).await?;
        self.record(json!({ "kind": "action", "action": "check", "target": Self::what(selector, text), "ok": true, "state": s }));
        Ok(s)
    }

    /// Sets the viewport (CSS pixels); `mobile` also emulates touch and a phone's viewport rules.
    pub async fn viewport(&self, width: u32, height: u32, mobile: bool) -> Result<Value> {
        self.send(
            "Emulation.setDeviceMetricsOverride",
            json!({ "width": width, "height": height, "deviceScaleFactor": 1, "mobile": mobile }),
        )
        .await?;
        self.send("Emulation.setTouchEmulationEnabled", json!({ "enabled": mobile })).await?;
        self.record(json!({ "kind": "action", "action": "viewport", "width": width, "height": height, "mobile": mobile, "ok": true }));
        self.layout().await
    }

    async fn layout(&self) -> Result<Value> {
        self.eval("({ width: innerWidth, height: innerHeight, scroll_width: document.documentElement.scrollWidth, overflow_x: document.documentElement.scrollWidth > innerWidth })").await
    }

    /// Saves a PNG of the viewport (or the whole page).
    pub async fn screenshot(&self, name: Option<&str>, full_page: bool) -> Result<Value> {
        use base64::Engine as _;
        let n = self.shots.fetch_add(1, Ordering::SeqCst) + 1;
        let file = match name {
            Some(s) => format!(
                "{}.png",
                s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect::<String>()
            ),
            None => format!("shot-{n}.png"),
        };
        let r = self.send("Page.captureScreenshot", json!({ "format": "png", "captureBeyondViewport": full_page })).await?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(r["data"].as_str().unwrap_or_default())?;
        if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
            bail!("the browser returned no PNG");
        }
        let (w, h) = (u32::from_be_bytes(bytes[16..20].try_into()?), u32::from_be_bytes(bytes[20..24].try_into()?));
        let path = self.dir.join(&file);
        std::fs::write(&path, &bytes)?;
        let sha = crate::trace::sha256(&bytes);
        let page = self.page().await;
        self.record(json!({ "kind": "action", "action": "screenshot", "ok": true, "file": file, "width": w, "height": h, "sha256": sha, "url": page["url"] }));
        Ok(json!({ "path": path, "width": w, "height": h, "sha256": sha, "url": page["url"] }))
    }

    /// Where the page is, what went wrong so far, and its layout.
    pub async fn state(&self) -> Result<Value> {
        let page = self.page().await;
        let layout = self.layout().await.unwrap_or(Value::Null);
        let s = self.seen.lock().unwrap();
        Ok(json!({ "url": page["url"], "title": page["title"], "page": page["page"], "console": s.console, "failed_requests": s.failed,
            "navigations": s.navigations, "layout": layout, "trace": self.dir.join("trace.jsonl") }))
    }
}

// ── sessions (one browser per agent folder) ─────────────────────────────────

type Session = Arc<tokio::sync::Mutex<Option<Browser>>>;
static SESSIONS: Mutex<Option<HashMap<PathBuf, Session>>> = Mutex::new(None);

fn session(cwd: &Path) -> Session {
    let mut all = SESSIONS.lock().unwrap();
    all.get_or_insert_with(HashMap::new).entry(cwd.to_path_buf()).or_default().clone()
}

/// Where a session keeps its trace and screenshots: per task when there is one.
pub fn session_dir(home: &Path, cwd: &Path) -> PathBuf {
    let key = std::env::var("FERMI_TASK_ID").unwrap_or_else(|_| crate::trace::sha256(cwd.to_string_lossy().as_bytes())[..12].to_string());
    home.join("browser").join(key)
}

/// Stops every browser this process started.
pub fn close_all() {
    if let Some(all) = SESSIONS.lock().unwrap().take() {
        for (_, s) in all {
            if let Ok(mut b) = s.try_lock() {
                b.take();
            }
        }
    }
}

fn ms(args: &Value) -> Option<Duration> {
    args["timeout_ms"].as_u64().map(Duration::from_millis)
}

/// Runs one browser tool for the agent working in `cwd`.
pub async fn tool(engine: &crate::engine::Engine, cwd: &Path, name: &str, args: &Value) -> Result<Value> {
    let s = session(cwd);
    let mut guard = s.lock().await;
    if name == "close" {
        let had = guard.take().is_some();
        return Ok(json!({ "closed": had }));
    }
    if guard.is_none() {
        let dir = session_dir(&engine.home, cwd);
        *guard = Some(Browser::launch(&engine.cfg.browser, &dir, engine.redactor.clone()).await?);
    }
    let b = guard.as_ref().expect("launched");
    let sel = args["selector"].as_str();
    let text = args["text"].as_str();
    match name {
        "open" => {
            if let (Some(w), Some(h)) = (args["width"].as_u64(), args["height"].as_u64()) {
                b.viewport(w as u32, h as u32, args["mobile"] == true).await?;
            }
            b.open(args["url"].as_str().ok_or_else(|| anyhow!("give a url"))?).await
        }
        "click" => b.click(sel, text, ms(args)).await,
        "fill" => b.fill(sel.ok_or_else(|| anyhow!("give a selector"))?, args["value"].as_str().unwrap_or_default(), ms(args)).await,
        "wait" => b.wait(sel, text, args["state"].as_str().unwrap_or("visible"), ms(args)).await,
        "read" => b.read(sel).await,
        "check" => b.check(sel, text).await,
        "viewport" => {
            b.viewport(args["width"].as_u64().unwrap_or(1280) as u32, args["height"].as_u64().unwrap_or(800) as u32, args["mobile"] == true)
                .await
        }
        "screenshot" => b.screenshot(args["name"].as_str(), args["full_page"] == true).await,
        "state" => b.state().await,
        other => bail!("unknown browser tool `{other}`"),
    }
}

/// The browser tools, for fermi's MCP bridge.
pub fn tool_defs() -> Vec<Value> {
    let target = json!({ "selector": { "type": "string", "description": "A CSS selector" }, "text": { "type": "string", "description": "Or the visible label of a button or link" },
        "timeout_ms": { "type": "integer", "description": "How long to wait for it (default 10000)" } });
    let obj = |props: Value, required: &[&str]| json!({ "type": "object", "properties": props, "required": required });
    vec![
        json!({ "name": "browser_open", "description": "Open a URL in fermi's headless browser (started on first use). Fails if the page can't be reached or answers with an HTTP error. Optional width/height/mobile set the viewport first.",
            "inputSchema": obj(json!({ "url": { "type": "string" }, "width": { "type": "integer" }, "height": { "type": "integer" }, "mobile": { "type": "boolean" } }), &["url"]) }),
        json!({ "name": "browser_click", "description": "Click a button or link (by selector or visible label) once it is visible, enabled and not covered; waits for a page it opens to load.", "inputSchema": obj(target.clone(), &[]) }),
        json!({ "name": "browser_fill", "description": "Type into a field (replacing its value) once it is visible and enabled.",
            "inputSchema": obj(json!({ "selector": { "type": "string" }, "value": { "type": "string" }, "timeout_ms": { "type": "integer" } }), &["selector", "value"]) }),
        json!({ "name": "browser_wait", "description": "Wait until an element is visible, enabled or hidden (state, default visible). Running out of time is an error.",
            "inputSchema": obj({ let mut t = target.clone(); t["state"] = json!({ "type": "string", "enum": ["visible", "enabled", "hidden"] }); t }, &[]) }),
        json!({ "name": "browser_read", "description": "The text of the page (or of a selector), with its URL and title.", "inputSchema": obj(json!({ "selector": { "type": "string" } }), &[]) }),
        json!({ "name": "browser_check", "description": "Whether an element exists and is visible, enabled, uncovered and inside the viewport, with its box.", "inputSchema": obj(target, &[]) }),
        json!({ "name": "browser_viewport", "description": "Set the viewport size; mobile emulates a phone (touch, mobile viewport rules).",
            "inputSchema": obj(json!({ "width": { "type": "integer" }, "height": { "type": "integer" }, "mobile": { "type": "boolean" } }), &["width", "height"]) }),
        json!({ "name": "browser_screenshot", "description": "Save a PNG of the viewport (or the full page); returns its path and size.",
            "inputSchema": obj(json!({ "name": { "type": "string" }, "full_page": { "type": "boolean" } }), &[]) }),
        json!({ "name": "browser_state", "description": "Where the page is, console errors and failed requests so far, navigations, and layout (viewport, horizontal overflow).", "inputSchema": obj(json!({}), &[]) }),
        json!({ "name": "browser_close", "description": "Close the browser.", "inputSchema": obj(json!({}), &[]) }),
    ]
}
