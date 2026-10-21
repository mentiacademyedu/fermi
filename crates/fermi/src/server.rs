//! `fermi ui`: local web dashboard. Same engine as the CLI/TUI, served on
//! 127.0.0.1 only. Every API call needs the per-run token that `fermi ui`
//! puts in the URL it opens, and the Host header must be local, so other
//! sites in the browser can't drive it (CSRF / DNS rebinding).

use crate::{files, setup};
use anyhow::{bail, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fermi_core::auth::{self, AuthInfo};
use fermi_core::hub::Hub;
use fermi_core::util::{expand, now};
use fermi_core::{Auth, Engine, Task};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;

const INDEX: &str = include_str!("../web/index.html");
const XTERM_JS: &str = include_str!("../web/vendor/xterm.js");
const XTERM_CSS: &str = include_str!("../web/vendor/xterm.css");
const XTERM_FIT: &str = include_str!("../web/vendor/addon-fit.js");
const CSP: &str = "default-src 'none'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; \
                   connect-src 'self'; img-src 'self' data: blob:; font-src 'self' data:; frame-ancestors 'none'; base-uri 'none'";

struct Srv {
    hub: Arc<Hub>,
    token: String,
    port: u16,
    cwd: PathBuf,
    auth: Mutex<HashMap<String, AuthInfo>>,
    probing: Mutex<HashSet<String>>,
    /// Folders the user added as workspaces (`~/.fermi/workspaces.json`).
    added: Mutex<Vec<PathBuf>>,
}

type S = State<Arc<Srv>>;

impl Srv {
    fn workspaces_file(&self) -> PathBuf {
        self.hub.engine().home.join("workspaces.json")
    }

    /// Every folder the dashboard may browse: added ones, the launch folder
    /// (unless it's the home folder), profile folders and folders tasks ran in.
    fn workspaces(&self) -> Vec<PathBuf> {
        let e = self.hub.engine();
        let home = dirs_home();
        let mut out: Vec<PathBuf> = vec![];
        let mut push = |p: &std::path::Path| {
            if let Some(c) = files::canon(p) {
                if c.is_dir() && !out.contains(&c) && Some(&c) != home.as_ref() {
                    out.push(c);
                }
            }
        };
        for p in self.added.lock().unwrap().iter() {
            push(p);
        }
        push(&self.cwd);
        for prof in &e.cfg.profiles {
            for p in &prof.paths {
                push(&expand(p));
            }
        }
        for p in self.hub.cwds() {
            push(&p);
        }
        out
    }

    fn save_added(&self) {
        let list: Vec<String> = self.added.lock().unwrap().iter().map(|p| p.to_string_lossy().into_owned()).collect();
        if let Ok(b) = serde_json::to_vec_pretty(&list) {
            let _ = std::fs::write(self.workspaces_file(), b);
        }
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).and_then(|h| files::canon(std::path::Path::new(&h)))
}

/// A bound, not yet serving dashboard.
pub struct Server {
    /// Full URL including the access key.
    pub url: String,
    pub hub: Arc<Hub>,
    listener: tokio::net::TcpListener,
    app: Router,
}

impl Server {
    /// Serves until `shutdown` resolves, then cancels running tasks.
    pub async fn serve(self, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> Result<()> {
        let hub = self.hub.clone();
        axum::serve(self.listener, self.app).with_graceful_shutdown(shutdown).await?;
        hub.cancel_all();
        Ok(())
    }
}

/// `fermi ui`: serve until Ctrl+C.
pub async fn run(engine: Arc<Engine>, port: u16, open: bool) -> Result<()> {
    let server = start(engine, port, std::env::current_dir()?).await?;
    println!("fermi dashboard: {}", server.url);
    println!("(this link includes your access key; Ctrl+C stops the dashboard and its running tasks)");
    if open {
        open_browser(&server.url);
    }
    server
        .serve(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}

/// Binds 127.0.0.1 (`port` 0 = any free port) and builds the app. `cwd` is
/// the default folder for tasks and for picking the profile.
pub async fn start(engine: Arc<Engine>, port: u16, cwd: PathBuf) -> Result<Server> {
    let listener = bind(port).await?;
    let port = listener.local_addr()?.port();
    let srv = Arc::new(Srv {
        hub: Hub::new(engine),
        token: new_token()?,
        port,
        cwd,
        auth: Mutex::new(HashMap::new()),
        probing: Mutex::new(HashSet::new()),
        added: Mutex::new(vec![]),
    });
    let saved: Vec<String> =
        std::fs::read(srv.workspaces_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    *srv.added.lock().unwrap() = saved.into_iter().map(PathBuf::from).collect();
    srv.probe_all();
    tokio::spawn(push_accounts_loop(srv.clone()));

    let app = Router::new()
        .route("/api/state", get(state))
        .route("/api/tasks", post(create_task))
        .route("/api/tasks/{id}", get(task))
        .route("/api/tasks/{id}/cancel", post(cancel))
        .route("/api/tasks/{id}/reply", post(reply))
        .route("/api/tasks/{id}/delete", post(delete_task))
        .route("/api/accounts", post(add_account))
        .route("/api/accounts/probe", post(probe))
        .route("/api/accounts/{id}/login", post(login))
        .route("/api/accounts/{id}/remove", post(remove_account))
        .route("/api/profiles", post(save_profile))
        .route("/api/profiles/{name}/delete", post(delete_profile))
        .route("/api/workspaces", post(workspace_add))
        .route("/api/workspaces/remove", post(workspace_remove))
        .route("/api/workspaces/pick", post(workspace_pick))
        .route("/api/fs/list", get(fs_list))
        .route("/api/fs/read", get(fs_read))
        .route("/api/fs/raw", get(fs_raw))
        .route("/api/fs/write", post(fs_write))
        .route("/api/fs/search", get(fs_search))
        .route("/api/attachments", post(attach).layer(axum::extract::DefaultBodyLimit::max(25 * 1024 * 1024)))
        .route("/api/git/status", get(git_status))
        .route("/api/git/diff", get(git_diff))
        .route("/api/ws", get(ws))
        .route("/api/term", get(term))
        .route_layer(middleware::from_fn_with_state(srv.clone(), require_token))
        .route("/", get(index))
        .route("/vendor/xterm.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript")], XTERM_JS) }))
        .route("/vendor/addon-fit.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript")], XTERM_FIT) }))
        .route("/vendor/xterm.css", get(|| async { ([(header::CONTENT_TYPE, "text/css")], XTERM_CSS) }))
        .layer(middleware::from_fn_with_state(srv.clone(), check_host))
        .with_state(srv.clone());

    Ok(Server { url: format!("http://127.0.0.1:{port}/#t={}", srv.token), hub: srv.hub.clone(), listener, app })
}

async fn bind(port: u16) -> Result<tokio::net::TcpListener> {
    if port == 0 {
        return Ok(tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?);
    }
    for p in port..port.saturating_add(20) {
        if let Ok(l) = tokio::net::TcpListener::bind(("127.0.0.1", p)).await {
            return Ok(l);
        }
    }
    bail!("no free port in {port}..{}", port.saturating_add(20))
}

fn new_token() -> Result<String> {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("random token: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn open_browser(url: &str) {
    let r = if cfg!(windows) {
        std::process::Command::new("cmd").args(["/c", "start", "", url]).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if r.is_err() {
        println!("open the link above in your browser");
    }
}

fn err(code: StatusCode, msg: impl Into<String>) -> Response {
    (code, Json(json!({ "error": msg.into() }))).into_response()
}

// ── security ────────────────────────────────────────────────────────────────

async fn check_host(State(s): S, req: Request, next: Next) -> Response {
    let host = req.headers().get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
    if host != format!("127.0.0.1:{}", s.port) && host != format!("localhost:{}", s.port) {
        return err(StatusCode::FORBIDDEN, "unexpected Host header");
    }
    next.run(req).await
}

async fn require_token(State(s): S, req: Request, next: Next) -> Response {
    let header_token = req.headers().get("x-fermi-token").and_then(|v| v.to_str().ok());
    // Browsers can't set headers on WebSocket requests, so the socket passes it in the query.
    let query_token = req.uri().query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("t=")));
    if header_token.or(query_token) != Some(s.token.as_str()) {
        return err(StatusCode::UNAUTHORIZED, "missing or wrong access key; reopen the link `fermi ui` printed");
    }
    if let Some(origin) = req.headers().get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if origin != format!("http://127.0.0.1:{}", s.port) && origin != format!("http://localhost:{}", s.port) {
            return err(StatusCode::FORBIDDEN, "cross-origin request refused");
        }
    }
    next.run(req).await
}

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_SECURITY_POLICY, CSP), (header::CACHE_CONTROL, "no-store"), (header::X_FRAME_OPTIONS, "DENY")],
        Html(INDEX),
    )
}

// ── state ───────────────────────────────────────────────────────────────────

impl Srv {
    fn accounts_json(&self) -> Value {
        let e = self.hub.engine();
        let snap = e.store.snapshot();
        let auth = self.auth.lock().unwrap();
        let probing = self.probing.lock().unwrap();
        let t = now();
        let list: Vec<Value> = e
            .cfg
            .accounts
            .iter()
            .map(|a| {
                let st = snap.get(&a.id).cloned().unwrap_or_default();
                let info = auth.get(&a.id);
                json!({
                    "id": a.id,
                    "provider": a.provider.as_str(),
                    "auth": if a.auth == Auth::Api { "api" } else { "cli" },
                    "key_env": a.api_key_env,
                    "logged_in": info.and_then(|i| i.logged_in).or(a.credentials_present()),
                    "email": info.and_then(|i| i.email.clone()),
                    "plan": info.and_then(|i| i.plan.clone()),
                    "checking": probing.contains(&a.id),
                    "utilization": st.utilization,
                    "resets_at": st.resets_at,
                    "limited_until": st.limited_until.filter(|u| *u > t),
                    "tasks": st.tasks,
                    "tokens": st.input_tokens + st.output_tokens,
                    "running": e.store.running(&a.id),
                })
            })
            .collect();
        Value::Array(list)
    }

    fn snapshot(&self) -> Value {
        let e = self.hub.engine();
        let here = e.cfg.resolve_profile(None, &self.cwd).ok().map(|p| p.name);
        let profiles: Vec<Value> = e
            .cfg
            .profiles
            .iter()
            .map(|p| {
                json!({
                    "name": p.name, "mode": p.mode, "accounts": p.accounts, "models": p.models, "paths": p.paths,
                    "effort": p.effort, "permission_mode": p.permission_mode,
                    "default": e.cfg.default_profile.as_deref() == Some(p.name.as_str()),
                })
            })
            .collect();
        let models: Vec<Value> = e
            .catalog
            .iter()
            .map(|m| {
                json!({
                    "id": m.id, "label": m.name(), "provider": m.provider().map(|p| p.as_str()),
                    "caps": m.caps, "tier": m.tier, "tier_name": m.tier_name(), "auto": m.auto,
                })
            })
            .collect();
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "cwd": self.cwd,
            "here": here,
            "profiles": profiles,
            "accounts": self.accounts_json(),
            "models": models,
            "workspaces": self.workspaces().iter().map(|p| json!({
                "path": p,
                "name": p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned()),
                "profile": e.cfg.resolve_profile(None, p).ok().map(|pr| pr.name),
                "added": self.added.lock().unwrap().iter().any(|a| files::canon(a).as_deref() == Some(p.as_path())),
            })).collect::<Vec<_>>(),
            "home": dirs_home(),
            "permissions": fermi_core::engine::PERMISSIONS,
            "efforts": fermi_core::models::EFFORTS,
            "tasks": self.hub.list(),
            "now": now(),
        })
    }

    fn push_accounts(&self) {
        self.hub.publish(json!({ "kind": "accounts", "accounts": self.accounts_json() }));
    }

    fn probe(self: &Arc<Self>, id: &str) {
        let e = self.hub.engine();
        let Some(a) = e.cfg.account(id).cloned() else { return };
        self.probing.lock().unwrap().insert(a.id.clone());
        let s = self.clone();
        tokio::spawn(async move {
            let info = auth::probe(&e.cfg, &a).await;
            s.auth.lock().unwrap().insert(a.id.clone(), info);
            s.probing.lock().unwrap().remove(&a.id);
            s.push_accounts();
        });
    }

    fn probe_all(self: &Arc<Self>) {
        for a in self.hub.engine().cfg.accounts.iter() {
            self.probe(&a.id);
        }
    }

    /// After opening a login window, re-check every few seconds until it
    /// succeeds (or give up after 10 minutes).
    fn watch_login(self: &Arc<Self>, id: String) {
        let s = self.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                tokio::time::sleep(Duration::from_secs(3)).await;
                let e = s.hub.engine();
                let Some(a) = e.cfg.account(&id).cloned() else { return };
                let info = auth::probe(&e.cfg, &a).await;
                let done = info.logged_in == Some(true);
                let changed = s.auth.lock().unwrap().insert(id.clone(), info.clone()).map(|o| o.email) != Some(info.email.clone());
                if changed || done {
                    s.push_accounts();
                }
                if done {
                    s.hub.publish(json!({ "kind": "login_ok", "id": id, "email": info.email }));
                    return;
                }
            }
        });
    }
}

/// Usage numbers change while tasks run; push them when they do.
async fn push_accounts_loop(s: Arc<Srv>) {
    let mut last = Value::Null;
    loop {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let cur = s.accounts_json();
        if cur != last {
            s.hub.publish(json!({ "kind": "accounts", "accounts": cur }));
            last = cur;
        }
    }
}

// ── handlers ────────────────────────────────────────────────────────────────

async fn state(State(s): S) -> Json<Value> {
    Json(s.snapshot())
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|x| x.trim().to_string()).filter(|x| !x.is_empty())
}

#[derive(Deserialize)]
struct NewTask {
    prompt: String,
    profile: Option<String>,
    model: Option<String>,
    account: Option<String>,
    effort: Option<String>,
    permission: Option<String>,
    cwd: Option<String>,
}

async fn create_task(State(s): S, Json(b): Json<NewTask>) -> Response {
    let prompt = b.prompt.trim();
    if prompt.is_empty() {
        return err(StatusCode::BAD_REQUEST, "describe the task first");
    }
    let cwd = non_empty(b.cwd).map(|c| expand(&c)).unwrap_or_else(|| s.cwd.clone());
    if !cwd.is_dir() {
        return err(StatusCode::BAD_REQUEST, format!("folder not found: {}", cwd.display()));
    }
    let mut task = Task::new(prompt, cwd);
    task.profile = non_empty(b.profile);
    task.model = non_empty(b.model);
    task.account = non_empty(b.account);
    task.effort = non_empty(b.effort);
    task.permission = non_empty(b.permission);
    // Surface routing errors (unknown profile, no account…) right away.
    if let Err(e) = s.hub.engine().plan(&task) {
        return err(StatusCode::BAD_REQUEST, format!("{e:#}"));
    }
    Json(json!({ "id": s.hub.submit(task) })).into_response()
}

async fn task(State(s): S, Path(id): Path<u64>) -> Response {
    match s.hub.get(id) {
        // The full record plus the derived fields the list view gets.
        Some(t) => {
            let mut v = serde_json::to_value(&t).unwrap_or_default();
            if let (Some(obj), Value::Object(extra)) = (v.as_object_mut(), t.summary()) {
                obj.extend(extra);
            }
            Json(v).into_response()
        }
        None => err(StatusCode::NOT_FOUND, "no such task"),
    }
}

async fn cancel(State(s): S, Path(id): Path<u64>) -> Json<Value> {
    Json(json!({ "ok": s.hub.cancel(id) }))
}

#[derive(Deserialize)]
struct Reply {
    prompt: String,
}

async fn reply(State(s): S, Path(id): Path<u64>, Json(b): Json<Reply>) -> Response {
    let prompt = b.prompt.trim();
    if prompt.is_empty() {
        return err(StatusCode::BAD_REQUEST, "write a reply first");
    }
    match s.hub.follow_up(id, prompt) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn delete_task(State(s): S, Path(id): Path<u64>) -> Response {
    match s.hub.delete(id) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct NewAccount {
    id: String,
    provider: String,
    api_key_env: Option<String>,
    profile: Option<String>,
}

async fn add_account(State(s): S, Json(b): Json<NewAccount>) -> Response {
    let id = b.id.trim().to_string();
    let key = non_empty(b.api_key_env);
    let profile = non_empty(b.profile);
    let res = setup::add_account(&id, &b.provider, key.as_deref(), None, profile.as_deref()).and_then(|m| {
        s.hub.reload()?;
        Ok(m)
    });
    match res {
        Ok(msg) => {
            s.probe(&id);
            Json(json!({ "message": msg, "id": id })).into_response()
        }
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

/// Runs a config edit, then reloads so every client sees it.
fn edit_config(s: &Srv, edit: impl FnOnce() -> Result<String>) -> Response {
    match edit().and_then(|msg| {
        s.hub.reload()?;
        Ok(msg)
    }) {
        Ok(msg) => Json(json!({ "message": msg })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn save_profile(State(s): S, Json(p): Json<setup::ProfileEdit>) -> Response {
    edit_config(&s, || setup::save_profile(&p))
}

async fn delete_profile(State(s): S, Path(name): Path<String>) -> Response {
    edit_config(&s, || setup::delete_profile(&name))
}

async fn remove_account(State(s): S, Path(id): Path<String>) -> Response {
    let r = edit_config(&s, || setup::remove_account(&id));
    s.auth.lock().unwrap().remove(&id);
    s.push_accounts();
    r
}

// ── workspaces and files ─────────────────────────────────────────────────────

#[derive(Deserialize)]
struct PathBody {
    path: String,
}

async fn workspace_add(State(s): S, Json(b): Json<PathBody>) -> Response {
    match add_workspace(&s, &expand(b.path.trim())) {
        Ok(dir) => Json(json!({ "path": dir })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

fn add_workspace(s: &Srv, path: &std::path::Path) -> Result<PathBuf> {
    let Some(dir) = files::canon(path).filter(|d| d.is_dir()) else { bail!("folder not found: {}", path.display()) };
    {
        let mut added = s.added.lock().unwrap();
        if !added.contains(&dir) {
            added.insert(0, dir.clone());
        }
    }
    s.save_added();
    s.hub.publish(json!({ "kind": "config" }));
    Ok(dir)
}

/// Opens the system's "select folder" dialog on this machine and adds the
/// chosen folder. Only one dialog at a time.
async fn workspace_pick(State(s): S) -> Response {
    static OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if OPEN.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return err(StatusCode::CONFLICT, "a folder dialog is already open");
    }
    let start = s.workspaces().into_iter().next().or_else(dirs_home);
    let mut dialog = rfd::AsyncFileDialog::new().set_title("Add a workspace folder");
    if let Some(dir) = start.as_deref().and_then(|d| d.parent()) {
        dialog = dialog.set_directory(dir);
    }
    let picked = dialog.pick_folder().await;
    OPEN.store(false, std::sync::atomic::Ordering::SeqCst);
    let Some(folder) = picked else { return Json(json!({ "cancelled": true })).into_response() };
    match add_workspace(&s, folder.path()) {
        Ok(dir) => Json(json!({ "path": dir })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn workspace_remove(State(s): S, Json(b): Json<PathBody>) -> Json<Value> {
    let target = files::canon(std::path::Path::new(&b.path));
    s.added.lock().unwrap().retain(|p| files::canon(p) != target && p.to_string_lossy() != b.path);
    s.save_added();
    s.hub.publish(json!({ "kind": "config" }));
    Json(json!({ "ok": true }))
}

fn fs_err(e: anyhow::Error) -> Response {
    err(StatusCode::BAD_REQUEST, format!("{e:#}"))
}

async fn fs_list(State(s): S, Query(q): Query<PathBody>) -> Response {
    match files::check(&s.workspaces(), &q.path).and_then(|p| files::list(&p)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => fs_err(e),
    }
}

async fn fs_read(State(s): S, Query(q): Query<PathBody>) -> Response {
    match files::check(&s.workspaces(), &q.path).and_then(|p| files::read(&p)) {
        Ok(v) => Json(v).into_response(),
        Err(e) => fs_err(e),
    }
}

/// Raw bytes for image previews. Sandboxed so an SVG or HTML file opened
/// directly can't run script with the dashboard's access.
async fn fs_raw(State(s): S, Query(q): Query<PathBody>) -> Response {
    let path = match files::check(&s.workspaces(), &q.path) {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    if std::fs::metadata(&path).map(|m| m.len() > files::MAX_RAW).unwrap_or(true) {
        return err(StatusCode::BAD_REQUEST, "file too large to preview");
    }
    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [
                (header::CONTENT_TYPE, files::content_type(&path)),
                (header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        Err(e) => fs_err(e.into()),
    }
}

#[derive(Deserialize)]
struct WriteBody {
    path: String,
    text: String,
}

async fn fs_write(State(s): S, Json(b): Json<WriteBody>) -> Response {
    let res = files::check_new(&s.workspaces(), &b.path).and_then(|p| {
        std::fs::write(&p, b.text.as_bytes())?;
        Ok(p)
    });
    match res {
        Ok(p) => Json(json!({ "path": p, "size": b.text.len() })).into_response(),
        Err(e) => fs_err(e),
    }
}

#[derive(Deserialize)]
struct SearchQuery {
    root: String,
    q: String,
    #[serde(default)]
    mode: Option<String>,
}

async fn fs_search(State(s): S, Query(q): Query<SearchQuery>) -> Response {
    let root = match files::check(&s.workspaces(), &q.root) {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    if q.q.trim().is_empty() {
        return Json(Vec::<files::Hit>::new()).into_response();
    }
    let contents = q.mode.as_deref() == Some("contents");
    let needle = q.q.trim().to_string();
    match tokio::task::spawn_blocking(move || files::search(&root, &needle, contents)).await {
        Ok(hits) => Json(hits).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct AttachQuery {
    cwd: String,
    name: String,
}

/// Saves a pasted or dropped file into `<folder>/.fermi/attachments/` so every
/// agent can read it from the task's folder. A `.gitignore` there keeps the
/// folder out of git.
async fn attach(State(s): S, Query(q): Query<AttachQuery>, body: axum::body::Bytes) -> Response {
    let mut roots = s.workspaces();
    roots.extend(dirs_home());
    let cwd = match files::check(&roots, &q.cwd) {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    if body.is_empty() {
        return err(StatusCode::BAD_REQUEST, "empty file");
    }
    let dir = cwd.join(".fermi").join("attachments");
    let res = (|| -> Result<PathBuf> {
        std::fs::create_dir_all(&dir)?;
        let ignore = cwd.join(".fermi").join(".gitignore");
        if !ignore.exists() {
            std::fs::write(&ignore, "# fermi's scratch folder (attachments); not part of the project\n*\n")?;
        }
        // Keep a safe, short file name; prefix the time so names never collide.
        let clean: String = q.name.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).collect();
        let clean = clean.trim_start_matches('.');
        let clean = if clean.is_empty() { "file".to_string() } else { clean.chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect() };
        let path = dir.join(format!("{}-{clean}", fermi_core::util::now()));
        std::fs::write(&path, &body)?;
        Ok(path)
    })();
    match res {
        Ok(path) => {
            let rel = path.strip_prefix(&cwd).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            Json(json!({ "path": path, "rel": rel, "size": body.len() })).into_response()
        }
        Err(e) => fs_err(e),
    }
}

#[derive(Deserialize)]
struct RootQuery {
    root: String,
}

async fn git_status(State(s): S, Query(q): Query<RootQuery>) -> Response {
    let root = match files::check(&s.workspaces(), &q.root) {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    match tokio::task::spawn_blocking(move || crate::git::status(&root)).await {
        Ok(Ok(st)) => Json(st).into_response(),
        Ok(Err(e)) => fs_err(e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct DiffQuery {
    root: String,
    path: String,
}

async fn git_diff(State(s): S, Query(q): Query<DiffQuery>) -> Response {
    let roots = s.workspaces();
    // The file may be deleted, so check it like a new path (its folder must exist inside a workspace).
    let (root, file) = match files::check(&roots, &q.root).and_then(|r| Ok((r, files::check_new(&roots, &q.path)?))) {
        Ok(x) => x,
        Err(e) => return fs_err(e),
    };
    match tokio::task::spawn_blocking(move || crate::git::diff(&root, &file)).await {
        Ok(Ok(text)) => Json(json!({ "diff": text })).into_response(),
        Ok(Err(e)) => fs_err(e),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn probe(State(s): S) -> Json<Value> {
    s.probe_all();
    Json(json!({ "ok": true }))
}

async fn login(State(s): S, Path(id): Path<String>) -> Response {
    let e = s.hub.engine();
    let Some(a) = e.cfg.account(&id) else { return err(StatusCode::NOT_FOUND, "no such account") };
    if a.auth == Auth::Api {
        let var = a.api_key_env.clone().unwrap_or_default();
        return err(StatusCode::BAD_REQUEST, format!("API-key accounts don't log in: set {var} and restart fermi ui"));
    }
    match open_login_window(&e.cfg, a) {
        Ok(()) => {
            s.watch_login(id);
            let msg = if a.provider == fermi_core::Provider::Gemini {
                "A Gemini window opened: sign in with Google, then close it (or type /quit) once Gemini's prompt appears."
            } else {
                "A sign-in window opened. Finish there and fermi updates by itself."
            };
            Json(json!({ "message": msg })).into_response()
        }
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't open a sign-in window ({e:#}). Run `fermi account login {id}` in a terminal instead."),
        ),
    }
}

/// The CLIs' logins are interactive, so they get their own console window.
fn open_login_window(cfg: &fermi_core::Config, acct: &fermi_core::Account) -> Result<()> {
    let cmd = setup::login_command(cfg, acct)?;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = cmd;
        cmd.creation_flags(0x0000_0010); // CREATE_NEW_CONSOLE
        cmd.spawn()?;
    }
    #[cfg(not(windows))]
    {
        // Rebuild as one shell line for the terminal emulator.
        let quote = |s: &std::ffi::OsStr| format!("'{}'", s.to_string_lossy().replace('\'', "'\\''"));
        let mut line = String::new();
        for (k, v) in cmd.get_envs() {
            if let Some(v) = v {
                line += &format!("export {}={}; ", k.to_string_lossy(), quote(v));
            }
        }
        line += &quote(cmd.get_program());
        for a in cmd.get_args() {
            line += " ";
            line += &quote(a);
        }
        if cfg!(target_os = "macos") {
            let script = format!("tell application \"Terminal\" to do script \"{}\"", line.replace('\\', "\\\\").replace('"', "\\\""));
            std::process::Command::new("osascript").args(["-e", &script]).spawn()?;
        } else {
            let terms: [(&str, &[&str]); 4] =
                [("x-terminal-emulator", &["-e"]), ("gnome-terminal", &["--"]), ("konsole", &["-e"]), ("xterm", &["-e"])];
            let ok = terms.iter().any(|(t, pre)| std::process::Command::new(t).args(*pre).args(["sh", "-c", &line]).spawn().is_ok());
            if !ok {
                bail!("no terminal emulator found");
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
struct TermQuery {
    cwd: String,
    #[serde(default)]
    cols: Option<u16>,
    #[serde(default)]
    rows: Option<u16>,
}

/// A shell in a workspace folder (or the home folder), bridged to this socket.
async fn term(State(s): S, Query(q): Query<TermQuery>, up: WebSocketUpgrade) -> Response {
    let mut roots = s.workspaces();
    roots.extend(dirs_home());
    let cwd = match files::check(&roots, &q.cwd) {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    let (cols, rows) = (q.cols.unwrap_or(100), q.rows.unwrap_or(30));
    up.on_upgrade(move |sock| term_loop(sock, cwd, cols, rows))
}

async fn term_loop(mut sock: WebSocket, cwd: PathBuf, cols: u16, rows: u16) {
    let pty = match tokio::task::spawn_blocking(move || crate::term::spawn(&cwd, cols, rows)).await {
        Ok(Ok(p)) => p,
        Ok(Err(e)) => {
            let _ = sock.send(Message::Text(format!("\r\n[fermi] {e:#}\r\n").into())).await;
            return;
        }
        Err(_) => return,
    };
    let crate::term::Pty { master, mut child, mut reader, mut writer } = pty;
    // Shell output arrives on a blocking reader; a thread forwards it.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    use std::io::Write;
    loop {
        tokio::select! {
            out = out_rx.recv() => match out {
                Some(bytes) => {
                    if sock.send(Message::Binary(bytes.into())).await.is_err() {
                        break;
                    }
                }
                None => {
                    let _ = sock.send(Message::Text("\r\n[process exited]\r\n".into())).await;
                    break;
                }
            },
            incoming = sock.recv() => match incoming {
                // {"data": "..."} keystrokes, {"resize": [cols, rows]}
                Some(Ok(Message::Text(t))) => {
                    let Ok(v) = serde_json::from_str::<Value>(&t) else { continue };
                    if let Some(d) = v["data"].as_str() {
                        let _ = writer.write_all(d.as_bytes()).and_then(|_| writer.flush());
                    } else if let (Some(c), Some(r)) = (v["resize"][0].as_u64(), v["resize"][1].as_u64()) {
                        let _ = master.resize(crate::term::size(c as u16, r as u16));
                    }
                }
                Some(Ok(Message::Binary(b))) => {
                    let _ = writer.write_all(&b).and_then(|_| writer.flush());
                }
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                _ => {}
            },
        }
    }
    let _ = child.kill();
}

async fn ws(State(s): S, up: WebSocketUpgrade) -> Response {
    up.on_upgrade(move |sock| ws_loop(s, sock))
}

async fn ws_loop(s: Arc<Srv>, mut sock: WebSocket) {
    let mut rx = s.hub.subscribe();
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let out = match msg {
                    Ok(v) => v,
                    Err(RecvError::Lagged(_)) => json!({ "kind": "resync" }),
                    Err(RecvError::Closed) => break,
                };
                if sock.send(Message::Text(out.to_string().into())).await.is_err() {
                    break;
                }
            }
            incoming = sock.recv() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                _ => {}
            },
        }
    }
}
