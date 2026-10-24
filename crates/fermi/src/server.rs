//! `fermi ui`: local web dashboard. Same engine as the CLI/TUI, served on
//! 127.0.0.1 only. Every API call needs the per-run token that `fermi ui`
//! puts in the URL it opens, and the Host header must be local, so other
//! sites in the browser can't drive it (CSRF / DNS rebinding).

use crate::skills::{self, Kind, Place};
use crate::{files, setup, slides};
use anyhow::{bail, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fermi_core::auth::{self, AuthInfo};
use fermi_core::hub::{FollowUp, Hub};
use fermi_core::sessions;
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
                   connect-src 'self'; img-src 'self' data: blob:; font-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; \
                   frame-src http: https:";

struct Srv {
    hub: Arc<Hub>,
    teams: Arc<fermi_core::team::Teams>,
    token: String,
    port: u16,
    cwd: PathBuf,
    auth: Mutex<HashMap<String, AuthInfo>>,
    probing: Mutex<HashSet<String>>,
    /// Folders the user added as workspaces (`~/.fermi/workspaces.json`).
    added: Mutex<Vec<PathBuf>>,
    /// Trending results by (period, language), with the time fetched.
    trending: Mutex<HashMap<String, (u64, Value)>>,
    /// Imports being reviewed (unpacked under `~/.fermi/staging`), by place id.
    staged: Mutex<HashMap<String, Place>>,
    /// Code indexes by repository folder, kept loaded.
    code: Mutex<HashMap<PathBuf, Arc<Mutex<fermi_core::code::Index>>>>,
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
    let server = start(engine, port, std::env::current_dir()?, "ui").await?;
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
/// the default folder for tasks and for picking the profile; `via` names this
/// front end in the shared history ("ui", "desktop").
pub async fn start(engine: Arc<Engine>, port: u16, cwd: PathBuf, via: &str) -> Result<Server> {
    let listener = bind(port).await?;
    let port = listener.local_addr()?.port();
    let hub = Hub::new(engine, via);
    let srv = Arc::new(Srv {
        teams: fermi_core::team::Teams::new(hub.clone()),
        hub,
        token: new_token()?,
        port,
        cwd,
        auth: Mutex::new(HashMap::new()),
        probing: Mutex::new(HashSet::new()),
        added: Mutex::new(vec![]),
        trending: Mutex::new(HashMap::new()),
        staged: Mutex::new(HashMap::new()),
        code: Mutex::new(HashMap::new()),
    });
    let saved: Vec<String> = std::fs::read(srv.workspaces_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    *srv.added.lock().unwrap() = saved.into_iter().map(PathBuf::from).collect();
    srv.probe_all();
    let _ = std::fs::remove_dir_all(srv.hub.engine().home.join("staging")); // reviews from an earlier run
    tokio::spawn(push_accounts_loop(srv.clone()));

    let app = Router::new()
        .route("/api/state", get(state))
        .route("/api/tasks", post(create_task))
        .route("/api/tasks/{id}", get(task))
        .route("/api/tasks/{id}/cancel", post(cancel))
        .route("/api/tasks/{id}/trace", get(task_trace))
        .route("/api/tasks/{id}/resume", post(resume))
        .route("/api/tasks/{id}/reply", post(reply))
        .route("/api/tasks/{id}/delete", post(delete_task))
        .route("/api/sessions", get(sessions_list))
        .route("/api/sessions/import", post(sessions_import))
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
        .route("/api/code/{query}", get(code_query))
        .route("/api/checks", get(checks_list).post(checks_run))
        .route("/api/memory", get(memory_list).post(memory_record))
        .route("/api/approvals", get(approvals_list).post(approvals_decide))
        .route("/api/memory/ingest", post(memory_ingest))
        .route("/api/evals", get(evals_list))
        .route("/api/evals/compare", get(evals_compare))
        .route("/api/evals/runs/{id}", get(evals_run))
        .route("/api/evals/proposals/{id}/adopt", post(evals_adopt))
        .route("/api/plugins", get(plugins_list))
        .route("/api/plugins/run", post(plugins_run))
        .route("/api/plugins/install", post(plugins_install))
        .route("/api/plugins/add-mcp", post(plugins_add_mcp))
        .route("/api/plugins/{name}/{action}", post(plugins_action))
        .route("/api/memory/{id}", get(memory_get))
        .route("/api/memory/{id}/{action}", post(memory_action))
        .route("/api/teams", get(teams_list).post(team_create))
        .route("/api/teams/{id}", get(team_get))
        .route("/api/teams/{id}/diff", get(team_diff))
        .route("/api/teams/{id}/{action}", post(team_action))
        .route("/api/skills", get(skills_list))
        .route("/api/skills/copy", post(skills_copy))
        .route("/api/skills/remove", post(skills_remove))
        .route("/api/skills/export", post(skills_export))
        .route("/api/skills/import", post(skills_import))
        .route("/api/skills/upload", post(skills_upload).layer(axum::extract::DefaultBodyLimit::max(100 * 1024 * 1024)))
        .route("/api/slides/pptx", get(pptx_read))
        .route("/api/slides/media", get(pptx_media))
        .route("/api/slides/create", post(deck_create))
        .route("/api/slides/save", post(pptx_save).layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)))
        .route("/api/attachments", post(attach).layer(axum::extract::DefaultBodyLimit::max(25 * 1024 * 1024)))
        .route("/api/usage", get(usage))
        .route("/api/usage/report", get(usage_report))
        .route("/api/trending", get(trending))
        .route("/api/repos/clone", post(clone_repo))
        .route("/api/open-url", post(open_url))
        .route("/api/preview/scan", get(preview_scan))
        .route("/api/preview/serve", post(preview_serve))
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
    // Not `cmd /c start`: cmd.exe would treat `&` in a URL as a second command.
    let r = if cfg!(windows) {
        std::process::Command::new("rundll32.exe").args(["url.dll,FileProtocolHandler", url]).spawn()
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
    ([(header::CONTENT_SECURITY_POLICY, CSP), (header::CACHE_CONTROL, "no-store"), (header::X_FRAME_OPTIONS, "DENY")], Html(INDEX))
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
                    "effort": p.effort, "permission_mode": p.permission_mode, "budget": p.budget, "routing": p.routing,
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
            // Where each budget stands (settled, reserved by running calls, left).
            "budgets": e.cfg.budgets.iter().map(|b| e.ledger.state(b, now())).collect::<Vec<_>>(),
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
    /// Check the agent's work and send failures back (see `fermi_core::verify`).
    #[serde(default)]
    verify: Option<fermi_core::verify::VerifyOpts>,
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
    task.verify = b.verify;
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

/// Every model call and tool invocation, correlated (see `trace::export`).
async fn task_trace(State(s): S, Path(id): Path<u64>) -> Response {
    match s.hub.get(id) {
        Some(t) => Json(fermi_core::trace::export(&t)).into_response(),
        None => err(StatusCode::NOT_FOUND, "no such task"),
    }
}

/// Continues an interrupted task in its provider session, as its next attempt.
async fn resume(State(s): S, Path(id): Path<u64>) -> Response {
    match s.hub.resume(id) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn cancel(State(s): S, Path(id): Path<u64>) -> Json<Value> {
    Json(json!({ "ok": s.hub.cancel(id) }))
}

#[derive(Deserialize)]
struct Reply {
    prompt: String,
    /// Switch to this model; empty keeps the current one.
    model: Option<String>,
    /// Empty keeps the effort, "auto" goes back to the default.
    effort: Option<String>,
    /// Tool rules to allow from now on (after the agent was blocked).
    #[serde(default)]
    allow: Vec<String>,
    /// New permission mode for the conversation.
    permission: Option<String>,
}

async fn reply(State(s): S, Path(id): Path<u64>, Json(b): Json<Reply>) -> Response {
    let prompt = b.prompt.trim();
    if prompt.is_empty() {
        return err(StatusCode::BAD_REQUEST, "write a reply first");
    }
    let opts = FollowUp {
        model: non_empty(b.model),
        effort: non_empty(b.effort).map(|e| if e == "auto" { String::new() } else { e }),
        allow: b.allow.into_iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty() && r.len() < 400).collect(),
        permission: non_empty(b.permission),
    };
    match s.hub.follow_up(id, prompt, opts) {
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

// ── sessions from other tools ───────────────────────────────────────────────

/// Sessions Claude Code, Codex and the Gemini CLI saved on this PC.
async fn sessions_list(State(s): S) -> Response {
    let cfg = s.hub.engine().cfg.clone();
    let Ok(found) = tokio::task::spawn_blocking(move || sessions::discover(&cfg)).await else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "couldn't read the session folders");
    };
    let known = s.hub.known_sessions();
    let rows: Vec<Value> = found
        .iter()
        .map(|f| {
            json!({
                "source": f.source, "tool": f.source.name(), "session": f.session, "title": f.title,
                "cwd": f.cwd, "updated": f.updated, "size": f.size, "account": f.account,
                // A reply resumes it in place (else the conversation is passed on).
                "resumes": f.account.is_some() && f.cwd.as_ref().is_some_and(|c| c.is_dir()),
                "task": known.get(&f.session),
            })
        })
        .collect();
    Json(json!({ "sessions": rows })).into_response()
}

#[derive(Deserialize)]
struct ImportReq {
    /// `<source>:<session id>` for each session.
    sessions: Vec<String>,
}

async fn sessions_import(State(s): S, Json(b): Json<ImportReq>) -> Response {
    if b.sessions.is_empty() {
        return err(StatusCode::BAD_REQUEST, "pick a session to import");
    }
    // Only sessions the scan finds can be imported: no arbitrary paths.
    let cfg = s.hub.engine().cfg.clone();
    let Ok(found) = tokio::task::spawn_blocking(move || sessions::discover(&cfg)).await else {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "couldn't read the session folders");
    };
    let hub = s.hub.clone();
    let res = tokio::task::spawn_blocking(move || {
        let mut ids = vec![];
        let mut errors = vec![];
        for want in &b.sessions {
            match found.iter().find(|f| &f.origin() == want) {
                Some(f) => match hub.import(f) {
                    Ok(id) => ids.push(id),
                    Err(e) => errors.push(format!("{}: {e:#}", f.title)),
                },
                None => errors.push(format!("{want}: not found")),
            }
        }
        (ids, errors)
    })
    .await;
    match res {
        Ok((ids, errors)) if ids.is_empty() => err(StatusCode::BAD_REQUEST, errors.join("; ")),
        Ok((ids, errors)) => Json(json!({ "ids": ids, "errors": errors })).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "import failed"),
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

// ── code intelligence ───────────────────────────────────────────────────────

#[derive(Deserialize)]
struct CodeQuery {
    root: String,
    #[serde(default)]
    q: String,
    #[serde(default)]
    target: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    base: String,
}

/// Index queries for a workspace: stats, search, symbol, file, impact, deps.
// ── approvals (actions the policy holds) ────────────────────────────────────

#[derive(Deserialize)]
struct ApprovalsQuery {
    task: Option<u64>,
}

/// Held actions nobody decided yet, and a task's whole approval history.
async fn approvals_list(State(s): S, Query(q): Query<ApprovalsQuery>) -> Response {
    use fermi_core::approvals::{Approvals, Entry};
    let a = Approvals::open(&s.hub.engine().home);
    let history: Vec<Entry> = a
        .entries()
        .into_iter()
        .filter(|e| {
            let t = match e {
                Entry::Request { task, .. } | Entry::Decision { task, .. } | Entry::Use { task, .. } => *task,
            };
            q.task.is_none() || t == q.task
        })
        .collect();
    Json(json!({ "pending": a.pending(q.task), "history": history })).into_response()
}

#[derive(Deserialize)]
struct ApprovalBody {
    task: Option<u64>,
    tool: String,
    resource: String,
    allow: bool,
    /// Also tell the agent to go ahead (approvals only).
    #[serde(default)]
    go: bool,
}

/// You approve (once) or deny exactly one held action of one task.
async fn approvals_decide(State(s): S, Json(b): Json<ApprovalBody>) -> Response {
    use fermi_core::approvals::{actor, Approvals};
    let id = match Approvals::open(&s.hub.engine().home).decide(b.task, &b.tool, &b.resource, b.allow, &actor()) {
        Ok(id) => id,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    };
    if let (true, true, Some(task)) = (b.allow, b.go, b.task) {
        let prompt = format!("The user approved this once: `{}`. Run exactly that now, then continue the task.", b.resource);
        if let Err(e) = s.hub.follow_up(task, &prompt, FollowUp::default()) {
            return err(StatusCode::BAD_REQUEST, format!("approved ({id}), but the task couldn't continue: {e:#}"));
        }
    }
    Json(json!({ "id": id })).into_response()
}

// ── project memory ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct MemoryQuery {
    root: String,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    topic: Option<String>,
}

fn memory_for(s: &Srv, root: &str) -> Result<fermi_core::memory::Memory, Box<Response>> {
    let dir = files::check(&s.workspaces(), root).map_err(|e| Box::new(fs_err(e)))?;
    fermi_core::memory::Memory::open(&s.hub.engine().home, &dir).map_err(|e| Box::new(err(StatusCode::BAD_REQUEST, format!("{e:#}"))))
}

// ── evaluations ─────────────────────────────────────────────────────────────

/// Runs (summaries), datasets with their versions, proposals and the active configuration.
async fn evals_list(State(s): S) -> Json<Value> {
    use fermi_core::eval;
    let home = s.hub.engine().home.clone();
    let runs: Vec<Value> = eval::reports(&home)
        .into_iter()
        .map(|r| {
            json!({ "id": r.id, "dataset": r.dataset, "config": r.config, "fingerprint": r.fingerprint, "evaluator": r.evaluator,
            "started": r.started, "summary": r.summary })
        })
        .collect();
    Json(json!({ "runs": runs, "datasets": eval::datasets(&home), "proposals": eval::proposals(&home), "active": eval::active(&home) }))
}

async fn evals_run(State(s): S, Path(id): Path<String>) -> Response {
    match fermi_core::eval::load_report(&s.hub.engine().home, &id) {
        Ok(r) => Json(json!(r)).into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct ComparePair {
    a: String,
    b: String,
}

/// Two runs of the same dataset version side by side (refused across versions).
async fn evals_compare(State(s): S, Query(q): Query<ComparePair>) -> Response {
    use fermi_core::eval;
    let home = s.hub.engine().home.clone();
    let r = eval::load_report(&home, &q.a).and_then(|a| eval::load_report(&home, &q.b).and_then(|b| eval::compare(&a, &b)));
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct Adopt {
    reviewer: String,
}

/// A person approves an evaluated proposal; refused unless it passed its evaluation.
async fn evals_adopt(State(s): S, Path(id): Path<String>, Json(b): Json<Adopt>) -> Response {
    match fermi_core::eval::adopt(&s.hub.engine().home, &id, &b.reviewer) {
        Ok(a) => Json(json!(a)).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

// ── plugins ─────────────────────────────────────────────────────────────────

/// Installed plugins, the registry and its collisions.
async fn plugins_list(State(s): S) -> Json<Value> {
    let e = s.hub.engine();
    Json(json!({ "installed": fermi_core::plugins::installed(&e.home), "registry": fermi_core::plugins::registry(&e) }))
}

async fn plugins_action(State(s): S, Path((name, action)): Path<(String, String)>) -> Response {
    use fermi_core::plugins;
    let e = s.hub.engine();
    let r = match action.as_str() {
        "enable" => plugins::enable(&e, &name).await.map(|_| ()),
        "disable" => plugins::disable(&e, &name),
        "remove" => plugins::remove(&e, &name),
        _ => return err(StatusCode::NOT_FOUND, "unknown action"),
    };
    match r {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(x) => err(StatusCode::BAD_REQUEST, format!("{x:#}")),
    }
}

#[derive(Deserialize)]
struct PluginInstall {
    path: std::path::PathBuf,
}

async fn plugins_install(State(s): S, Json(b): Json<PluginInstall>) -> Response {
    match fermi_core::plugins::install(&s.hub.engine(), &b.path).await {
        Ok(entries) => Json(json!({ "ok": true, "registered": entries })).into_response(),
        Err(x) => err(StatusCode::BAD_REQUEST, format!("{x:#}")),
    }
}

#[derive(Deserialize)]
struct AddMcp {
    name: String,
    command: Vec<String>,
    #[serde(flatten)]
    access: fermi_core::plugins::Access,
}

async fn plugins_add_mcp(State(s): S, Json(b): Json<AddMcp>) -> Response {
    if b.command.is_empty() {
        return err(StatusCode::BAD_REQUEST, "give the command that starts the server");
    }
    match fermi_core::plugins::add_mcp(&s.hub.engine(), &b.name, b.command, b.access).await {
        Ok(entries) => Json(json!({ "ok": true, "registered": entries })).into_response(),
        Err(x) => err(StatusCode::BAD_REQUEST, format!("{x:#}")),
    }
}

#[derive(Deserialize)]
struct PluginRun {
    capability: String,
    #[serde(default)]
    arguments: Value,
    /// The folder it runs in (default: the first workspace, else home).
    cwd: Option<std::path::PathBuf>,
}

/// Runs a plugin command (or tool) for the user.
async fn plugins_run(State(s): S, Json(b): Json<PluginRun>) -> Response {
    use fermi_core::plugins::{self, Kind};
    let e = s.hub.engine();
    let reg = plugins::registry(&e);
    let kind = if reg.resolve(Kind::Command, &b.capability).is_ok() { Kind::Command } else { Kind::Tool };
    let cwd = b.cwd.or_else(|| s.hub.cwds().into_iter().next()).or_else(dirs::home_dir).unwrap_or_default();
    let args = if b.arguments.is_null() { json!({}) } else { b.arguments };
    match plugins::call(&e, kind, &b.capability, &args, &cwd).await {
        Ok(o) => Json(json!(o)).into_response(),
        Err(x) => err(StatusCode::BAD_REQUEST, format!("{x:#}")),
    }
}

/// The project's records (the user sees private ones too), a search or a topic.
async fn memory_list(State(s): S, Query(q): Query<MemoryQuery>) -> Response {
    use fermi_core::memory::{Caller, RETRIEVAL_VERSION};
    let mem = match memory_for(&s, &q.root) {
        Ok(m) => m,
        Err(r) => return *r,
    };
    if let Some(t) = q.topic.filter(|t| !t.trim().is_empty()) {
        return Json(mem.topic(Caller::User, &t)).into_response();
    }
    match q.q.filter(|x| !x.trim().is_empty()) {
        Some(query) => {
            Json(json!({ "query": query, "retrieval": RETRIEVAL_VERSION, "hits": mem.search(Caller::User, &query, 20) })).into_response()
        }
        None => Json(json!({ "project": mem.key, "root": mem.root, "records": mem.list(Caller::User) })).into_response(),
    }
}

#[derive(Deserialize)]
struct MemoryNew {
    root: String,
    record: fermi_core::memory::NewRecord,
}

async fn memory_record(State(s): S, Json(b): Json<MemoryNew>) -> Response {
    let mem = match memory_for(&s, &b.root) {
        Ok(m) => m,
        Err(r) => return *r,
    };
    match mem.record(b.record, "user", fermi_core::memory::Provenance::User) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct MemoryRoot {
    root: String,
}

async fn memory_get(State(s): S, Path(id): Path<String>, Query(q): Query<MemoryRoot>) -> Response {
    let mem = match memory_for(&s, &q.root) {
        Ok(m) => m,
        Err(r) => return *r,
    };
    match mem.get(fermi_core::memory::Caller::User, &id) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::NOT_FOUND, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct MemoryEdit {
    root: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    change: String,
}

async fn memory_action(State(s): S, Path((id, action)): Path<(String, String)>, Json(b): Json<MemoryEdit>) -> Response {
    use fermi_core::memory::Caller;
    let mem = match memory_for(&s, &b.root) {
        Ok(m) => m,
        Err(r) => return *r,
    };
    let res = match action.as_str() {
        "revise" => mem.revise(Caller::User, &id, b.title, b.body, &b.change, "user").map(|v| json!(v)),
        "retire" => mem.retire(Caller::User, &id, "user").map(|_| json!({ "ok": true })),
        other => Err(anyhow::anyhow!("unknown action {other}")),
    };
    match res {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn memory_ingest(State(s): S, Json(b): Json<MemoryRoot>) -> Response {
    let mem = match memory_for(&s, &b.root) {
        Ok(m) => m,
        Err(r) => return *r,
    };
    let deny = s.hub.engine().cfg.security.deny_read.clone();
    match tokio::task::spawn_blocking(move || mem.ingest_comments(&deny)).await {
        Ok(Ok(n)) => Json(json!({ "added": n })).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct ChecksQuery {
    root: String,
    #[serde(default)]
    checks: Vec<String>,
}

/// The checks fermi would run for a folder's repository.
async fn checks_list(State(s): S, Query(q): Query<ChecksQuery>) -> Response {
    let dir = match files::check(&s.workspaces(), &q.root) {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    let root = fermi_core::code::repo_root(&dir);
    match fermi_core::verify::load(&root) {
        Ok(c) => Json(json!({
            "root": root, "source": c.source, "checks": c.checks, "protect": c.protect,
            "max_repairs": c.max_repairs.unwrap_or(fermi_core::verify::DEFAULT_REPAIRS),
            "config_file": fermi_core::verify::CONFIG_FILE,
        }))
        .into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

/// Runs a folder's checks now and returns the report.
async fn checks_run(State(s): S, Json(q): Json<ChecksQuery>) -> Response {
    let dir = match files::check(&s.workspaces(), &q.root) {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    match fermi_core::verify::run(&dir, fermi_core::verify::RunOpts { only: &q.checks, ..Default::default() }).await {
        Ok(r) => Json(serde_json::to_value(r.trimmed()).unwrap_or_default()).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn code_query(State(s): S, Path(query): Path<String>, Query(q): Query<CodeQuery>) -> Response {
    let dir = match files::check(&s.workspaces(), &q.root) {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    let s2 = s.clone();
    let res = tokio::task::spawn_blocking(move || -> Result<Value> {
        use fermi_core::code::{canonical, repo_root, Index};
        let root = canonical(&repo_root(&dir))?;
        let slot = s2.code.lock().unwrap().get(&root).cloned();
        let idx = match slot {
            Some(i) => i,
            None => {
                let i = Arc::new(Mutex::new(Index::open(&root)?));
                s2.code.lock().unwrap().insert(root.clone(), i.clone());
                i
            }
        };
        let mut idx = idx.lock().unwrap();
        if idx.updated.elapsed() > Duration::from_secs(2) {
            idx.update()?;
        }
        Ok(match query.as_str() {
            "stats" => idx.stats(),
            "search" => json!({ "results": idx.search(&q.q, 60) }),
            "symbol" => idx.symbol(&q.target),
            "file" => idx.file(&q.path),
            "impact" => idx.impact(&idx.git_changes((!q.base.is_empty()).then_some(q.base.as_str()))?),
            "deps" => idx.deps(),
            other => bail!("unknown query {other}"),
        })
    })
    .await;
    match res {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "the code index stopped unexpectedly"),
    }
}

// ── team runs ───────────────────────────────────────────────────────────────

/// Runs a blocking team operation (they call git) off the async threads.
async fn team_op<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f).await.map_err(|_| anyhow::anyhow!("the operation stopped unexpectedly"))?
}

async fn teams_list(State(s): S) -> Json<Value> {
    Json(json!({ "runs": s.teams.list() }))
}

async fn team_get(State(s): S, Path(id): Path<u64>) -> Response {
    match s.teams.get(id) {
        Some(r) => Json(json!(r)).into_response(),
        None => err(StatusCode::NOT_FOUND, "no such team run"),
    }
}

async fn team_create(State(s): S, Json(b): Json<fermi_core::team::NewRun>) -> Response {
    let teams = s.teams.clone();
    match team_op(move || teams.create(b)).await {
        Ok(id) => Json(json!({ "id": id })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

#[derive(Deserialize, Default)]
struct TeamActionBody {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    plan: Option<Value>,
}

async fn team_action(State(s): S, Path((id, action)): Path<(u64, String)>, body: Option<Json<TeamActionBody>>) -> Response {
    let b = body.map(|j| j.0).unwrap_or_default();
    let teams = s.teams.clone();
    let res = team_op(move || -> Result<Value> {
        let key = || b.key.clone().ok_or_else(|| anyhow::anyhow!("which task?"));
        match action.as_str() {
            "approve" => teams.approve(id, b.plan).map(|_| json!({ "ok": true })),
            "cancel" => teams.cancel(id).map(|_| json!({ "ok": true })),
            "resume" => teams.resume(id).map(|_| json!({ "ok": true })),
            "retry" => teams.retry(id, &key()?).map(|_| json!({ "ok": true })),
            "skip" => teams.skip(id, &key()?).map(|_| json!({ "ok": true })),
            "merge" => teams.merge(id).map(|branch| json!({ "branch": branch })),
            "verify" => teams.verify(id).map(|_| json!({ "ok": true })),
            "discard" => teams.discard(id).map(|_| json!({ "ok": true })),
            other => bail!("unknown action {other}"),
        }
    })
    .await;
    match res {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct TeamDiffQuery {
    key: Option<String>,
    file: Option<String>,
}

async fn team_diff(State(s): S, Path(id): Path<u64>, Query(q): Query<TeamDiffQuery>) -> Response {
    let teams = s.teams.clone();
    match team_op(move || teams.diff(id, q.key.as_deref().filter(|k| !k.is_empty()), q.file.as_deref())).await {
        Ok(text) => Json(json!({ "diff": text })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

// ── skills and subagents ─────────────────────────────────────────────────────

impl Srv {
    fn skill_places(&self) -> Vec<Place> {
        let mut all = skills::places(&self.hub.engine().cfg, &self.workspaces());
        all.extend(self.staged.lock().unwrap().values().cloned());
        all
    }
    fn skill_place(&self, id: &str) -> Result<Place> {
        match self.skill_places().into_iter().find(|p| p.id == id) {
            Some(p) => Ok(p),
            None => bail!("unknown place {id}"),
        }
    }
    /// Unpacks an import for review and returns what's in it.
    fn stage_import(&self, src: &std::path::Path) -> Result<Value> {
        let id = format!("{:x}", now_ms());
        let dir = self.hub.engine().home.join("staging").join(&id);
        let place = skills::stage(src, &dir, &id).inspect_err(|_| {
            let _ = std::fs::remove_dir_all(&dir);
        })?;
        let items = skills::scan(&place);
        self.staged.lock().unwrap().insert(place.id.clone(), place.clone());
        Ok(json!({ "place": place, "items": items }))
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

async fn skills_list(State(s): S) -> Response {
    let places: Vec<Place> = s.skill_places().into_iter().filter(|p| p.scope != "import").collect();
    let res = tokio::task::spawn_blocking(move || {
        let items: Vec<skills::Item> = places.iter().flat_map(skills::scan).collect();
        json!({ "places": places, "items": items })
    })
    .await;
    match res {
        Ok(v) => Json(v).into_response(),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "couldn't read the skill folders"),
    }
}

#[derive(Deserialize)]
struct SkillCopy {
    kind: Kind,
    name: String,
    from: String,
    to: Vec<String>,
    #[serde(default)]
    replace: bool,
}

async fn skills_copy(State(s): S, Json(b): Json<SkillCopy>) -> Response {
    let from = match s.skill_place(&b.from) {
        Ok(p) => p,
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    };
    let targets: Vec<Result<Place>> = b.to.iter().map(|id| s.skill_place(id)).collect();
    let results = tokio::task::spawn_blocking(move || {
        b.to.iter()
            .zip(targets)
            .map(|(id, t)| match t.and_then(|t| skills::copy(&from, b.kind, &b.name, &t, b.replace)) {
                Ok(note) => json!({ "to": id, "ok": true, "note": note }),
                Err(e) => json!({ "to": id, "ok": false, "error": format!("{e:#}") }),
            })
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    Json(json!({ "results": results })).into_response()
}

#[derive(Deserialize)]
struct SkillRef {
    kind: Kind,
    name: String,
    place: String,
}

async fn skills_remove(State(s): S, Json(b): Json<SkillRef>) -> Response {
    match s.skill_place(&b.place).and_then(|p| skills::remove(&p, b.kind, &b.name)) {
        Ok(trashed) => Json(json!({ "trashed": trashed })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct SkillExport {
    items: Vec<SkillRef>,
}

static DIALOG_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Runs one native file dialog at a time.
async fn with_dialog<T>(f: impl std::future::Future<Output = T>) -> Option<T> {
    if DIALOG_OPEN.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return None;
    }
    let out = f.await;
    DIALOG_OPEN.store(false, std::sync::atomic::Ordering::SeqCst);
    Some(out)
}

async fn skills_export(State(s): S, Json(b): Json<SkillExport>) -> Response {
    if b.items.is_empty() {
        return err(StatusCode::BAD_REQUEST, "pick something to export");
    }
    let mut picked = vec![];
    for r in &b.items {
        match s.skill_place(&r.place) {
            Ok(p) => picked.push((p, r.kind, r.name.clone())),
            Err(e) => return err(StatusCode::BAD_REQUEST, format!("{e:#}")),
        }
    }
    let default = if picked.len() == 1 { format!("{}.zip", picked[0].2) } else { "skills-and-agents.zip".into() };
    let dialog =
        rfd::AsyncFileDialog::new().set_title("Export skills and agents").set_file_name(&default).add_filter("Zip archive", &["zip"]);
    let Some(chosen) = with_dialog(dialog.save_file()).await else { return err(StatusCode::CONFLICT, "a file dialog is already open") };
    let Some(file) = chosen else { return Json(json!({ "cancelled": true })).into_response() };
    let mut out = file.path().to_path_buf();
    if out.extension().is_none() {
        out.set_extension("zip");
    }
    let target = out.clone();
    let res = tokio::task::spawn_blocking(move || {
        let refs: Vec<(&Place, Kind, String)> = picked.iter().map(|(p, k, n)| (p, *k, n.clone())).collect();
        skills::export(&refs, &target)
    })
    .await;
    match res {
        Ok(Ok(n)) => Json(json!({ "path": out, "count": n })).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "export failed"),
    }
}

#[derive(Deserialize)]
struct SkillImport {
    #[serde(default)]
    folder: bool,
}

/// Picks a bundle, archive, folder or .md with the native dialog and stages it for review.
async fn skills_import(State(s): S, Json(b): Json<SkillImport>) -> Response {
    let dialog = rfd::AsyncFileDialog::new().set_title("Import skills and agents");
    let picked = if b.folder {
        with_dialog(dialog.pick_folder()).await
    } else {
        with_dialog(dialog.add_filter("Skills and agents", &["zip", "skill", "md"]).pick_file()).await
    };
    let Some(picked) = picked else { return err(StatusCode::CONFLICT, "a file dialog is already open") };
    let Some(src) = picked else { return Json(json!({ "cancelled": true })).into_response() };
    let src = src.path().to_path_buf();
    let s2 = s.clone();
    match tokio::task::spawn_blocking(move || s2.stage_import(&src)).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "import failed"),
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    name: String,
}

/// A file dropped on the page: saved, then staged like a picked one.
async fn skills_upload(State(s): S, Query(q): Query<UploadQuery>, body: axum::body::Bytes) -> Response {
    let name: String = q.name.chars().filter(|c| c.is_alphanumeric() || "._- ".contains(*c)).collect();
    let lower = name.to_lowercase();
    if !(lower.ends_with(".zip") || lower.ends_with(".skill") || lower.ends_with(".md")) {
        return err(StatusCode::BAD_REQUEST, "drop a .zip or .skill archive, or a .md file");
    }
    let dir = s.hub.engine().home.join("staging").join(format!("upload-{:x}", now_ms()));
    let file = dir.join(if name.trim().is_empty() { "upload.zip".to_string() } else { name });
    let s2 = s.clone();
    let res = tokio::task::spawn_blocking(move || -> Result<Value> {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&file, &body)?;
        let v = s2.stage_import(&file);
        let _ = std::fs::remove_dir_all(&dir);
        v
    })
    .await;
    match res {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "import failed"),
    }
}

// ── slides (.pptx) ──────────────────────────────────────────────────────────

fn pptx_path(s: &Srv, path: &str) -> Result<PathBuf> {
    let p = files::check(&s.workspaces(), path)?;
    if !p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pptx")) {
        bail!("not a .pptx file");
    }
    Ok(p)
}

async fn pptx_read(State(s): S, Query(q): Query<PathBody>) -> Response {
    let path = match pptx_path(&s, &q.path) {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    match tokio::task::spawn_blocking(move || slides::read(&path)).await {
        Ok(Ok(deck)) => Json(deck).into_response(),
        Ok(Err(e)) => fs_err(e),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "couldn't read the deck"),
    }
}

#[derive(Deserialize)]
struct MediaQuery {
    path: String,
    name: String,
}

async fn pptx_media(State(s): S, Query(q): Query<MediaQuery>) -> Response {
    let path = match pptx_path(&s, &q.path) {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    match tokio::task::spawn_blocking(move || slides::media(&path, &q.name)).await {
        Ok(Ok((bytes, mime))) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'; style-src 'unsafe-inline'"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(e)) => fs_err(e),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "couldn't read the deck"),
    }
}

#[derive(Deserialize)]
struct DeckCreate {
    path: String,
    spec: fermi_core::deck::DeckSpec,
}

/// Writes a designed .pptx (e.g. from a Markdown deck). Images resolve next to it.
async fn deck_create(State(s): S, Json(b): Json<DeckCreate>) -> Response {
    let path = match files::check_new(&s.workspaces(), &b.path) {
        Ok(p) if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pptx")) => p,
        Ok(_) => return err(StatusCode::BAD_REQUEST, "the file must end in .pptx"),
        Err(e) => return fs_err(e),
    };
    let out = path.clone();
    let res = tokio::task::spawn_blocking(move || {
        let base = out.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
        fermi_core::deck::write(&b.spec, &out, &base)
    })
    .await;
    match res {
        Ok(Ok(n)) => Json(json!({ "path": path, "slides": n })).into_response(),
        Ok(Err(e)) => fs_err(e),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "couldn't write the deck"),
    }
}

#[derive(Deserialize)]
struct PptxSave {
    path: String,
    #[serde(default)]
    parts: HashMap<String, String>,
    #[serde(default)]
    remove: Vec<String>,
}

async fn pptx_save(State(s): S, Json(b): Json<PptxSave>) -> Response {
    let path = match pptx_path(&s, &b.path) {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    match tokio::task::spawn_blocking(move || slides::save(&path, &b.parts, &b.remove)).await {
        Ok(Ok(())) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) => fs_err(e),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "couldn't save the deck"),
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

// ── trending repositories ───────────────────────────────────────────────────

impl Srv {
    fn clone_root_file(&self) -> PathBuf {
        self.hub.engine().home.join("clone-root.txt")
    }
    /// Where clones go, once chosen.
    fn clone_root(&self) -> Option<PathBuf> {
        let s = std::fs::read_to_string(self.clone_root_file()).ok()?;
        Some(PathBuf::from(s.trim())).filter(|p| p.is_dir())
    }
}

#[derive(Deserialize)]
struct TrendingQuery {
    #[serde(default)]
    period: Option<String>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    refresh: Option<bool>,
}

async fn trending(State(s): S, Query(q): Query<TrendingQuery>) -> Response {
    let period = crate::trending::period(q.period.as_deref().unwrap_or("daily"));
    let language = q.language.filter(|l| !l.trim().is_empty());
    let key = format!("{period}|{}", language.as_deref().unwrap_or(""));
    let fresh = |t: u64| now().saturating_sub(t) < 20 * 60;
    let cached = s.trending.lock().unwrap().get(&key).filter(|(t, _)| fresh(*t)).map(|(_, v)| v.clone());
    let data = match cached.filter(|_| q.refresh != Some(true)) {
        Some(v) => v,
        None => match crate::trending::fetch(period, language.as_deref()).await {
            Ok((repos, source)) => {
                let v = json!({ "repos": repos, "source": source, "fetched": now() });
                s.trending.lock().unwrap().insert(key, (now(), v.clone()));
                v
            }
            Err(e) => return err(StatusCode::BAD_GATEWAY, format!("couldn't load trending repositories: {e:#}")),
        },
    };
    let mut data = data;
    data["clone_root"] = json!(s.clone_root());
    Json(data).into_response()
}

#[derive(Deserialize)]
struct CloneBody {
    owner: String,
    name: String,
    /// Ask for the folder even if one was chosen before.
    #[serde(default)]
    choose: bool,
}

/// Shallow-clones a GitHub repository and adds it as a workspace. The first
/// time (or when asked), the system folder picker chooses where clones go.
async fn clone_repo(State(s): S, Json(b): Json<CloneBody>) -> Response {
    let root = match s.clone_root().filter(|_| !b.choose) {
        Some(r) => r,
        None => {
            let mut dialog = rfd::AsyncFileDialog::new().set_title("Choose where cloned repositories go");
            if let Some(h) = dirs_home() {
                dialog = dialog.set_directory(h);
            }
            let Some(picked) = dialog.pick_folder().await else { return Json(json!({ "cancelled": true })).into_response() };
            let dir = picked.path().to_path_buf();
            let _ = std::fs::write(s.clone_root_file(), dir.to_string_lossy().as_bytes());
            dir
        }
    };
    let (owner, name) = (b.owner.clone(), b.name.clone());
    let res = tokio::task::spawn_blocking(move || crate::trending::clone(&owner, &name, &root)).await;
    match res {
        Ok(Ok(dest)) => match add_workspace(&s, &dest) {
            Ok(dir) => Json(json!({ "path": dir })).into_response(),
            Err(e) => fs_err(e),
        },
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

#[derive(Deserialize)]
struct UrlBody {
    url: String,
}

/// Opens a web page in the default browser (the desktop window can't
/// navigate away from the app). Only plain http(s) URLs.
async fn open_url(Json(b): Json<UrlBody>) -> Response {
    let ok = (b.url.starts_with("https://") || b.url.starts_with("http://"))
        && b.url.len() < 2000
        && b.url.chars().all(|c| c.is_ascii_graphic() && !"\"'<>\\^`{|}".contains(c));
    if !ok {
        return err(StatusCode::BAD_REQUEST, "only plain http(s) links can be opened");
    }
    open_browser(&b.url);
    Json(json!({ "ok": true })).into_response()
}

async fn preview_scan(State(s): S) -> Json<Value> {
    Json(json!({ "servers": crate::preview::scan(s.port).await }))
}

#[derive(Deserialize)]
struct ServeBody {
    root: String,
    /// A file inside the root to open (e.g. an .html file).
    #[serde(default)]
    file: Option<String>,
}

/// Serves a workspace folder (static files) on its own local port.
async fn preview_serve(State(s): S, Json(b): Json<ServeBody>) -> Response {
    let root = match files::check(&s.workspaces(), &b.root) {
        Ok(p) if p.is_dir() => p,
        Ok(_) => return err(StatusCode::BAD_REQUEST, "not a folder"),
        Err(e) => return fs_err(e),
    };
    let port = match crate::preview::serve(root.clone()).await {
        Ok(p) => p,
        Err(e) => return fs_err(e),
    };
    let rel = b
        .file
        .and_then(|f| files::canon(std::path::Path::new(&f)))
        .and_then(|f| f.strip_prefix(&root).ok().map(|r| r.to_string_lossy().replace('\\', "/")))
        .unwrap_or_default();
    Json(json!({ "url": format!("http://127.0.0.1:{port}/{rel}") })).into_response()
}

#[derive(Deserialize)]
struct UsageQuery {
    #[serde(default)]
    days: Option<u64>,
}

/// Runs from `usage.jsonl` in the last `days` days. The page groups them by
/// day in the viewer's time zone.
async fn usage(State(s): S, Query(q): Query<UsageQuery>) -> Response {
    let since = now().saturating_sub(q.days.unwrap_or(14).clamp(1, 365) * 86_400);
    let path = s.hub.engine().home.join("usage.jsonl");
    let rows = tokio::task::spawn_blocking(move || {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|v| v["ts"].as_u64().unwrap_or(0) >= since)
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    Json(json!({ "since": since, "rows": rows })).into_response()
}

#[derive(Deserialize)]
struct ReportQuery {
    by: Option<String>,
    days: Option<u64>,
}

/// Usage grouped by task, team run, agent, provider, model or account, reconciled.
async fn usage_report(State(s): S, Query(q): Query<ReportQuery>) -> Response {
    let since = q.days.map(|d| now().saturating_sub(d.clamp(1, 3650) * 86_400)).unwrap_or(0);
    let by = q.by.unwrap_or_else(|| "provider".into());
    match fermi_core::billing::report(&s.hub.engine().home, &by, since) {
        Ok(v) => Json(v).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
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
        let clean = if clean.is_empty() {
            "file".to_string()
        } else {
            clean.chars().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect()
        };
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
