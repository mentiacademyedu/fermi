//! `fermi ui`: local web dashboard. Same engine as the CLI/TUI, served on
//! 127.0.0.1 only. Every API call needs the per-run token that `fermi ui`
//! puts in the URL it opens, and the Host header must be local, so other
//! sites in the browser can't drive it (CSRF / DNS rebinding).

use crate::setup;
use anyhow::{bail, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Request, State};
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
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                   connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'";

struct Srv {
    hub: Arc<Hub>,
    token: String,
    port: u16,
    cwd: PathBuf,
    auth: Mutex<HashMap<String, AuthInfo>>,
    probing: Mutex<HashSet<String>>,
}

type S = State<Arc<Srv>>;

pub async fn run(engine: Arc<Engine>, port: u16, open: bool) -> Result<()> {
    let listener = bind(port).await?;
    let port = listener.local_addr()?.port();
    let srv = Arc::new(Srv {
        hub: Hub::new(engine),
        token: new_token()?,
        port,
        cwd: std::env::current_dir()?,
        auth: Mutex::new(HashMap::new()),
        probing: Mutex::new(HashSet::new()),
    });
    srv.probe_all();
    tokio::spawn(push_accounts_loop(srv.clone()));

    let app = Router::new()
        .route("/api/state", get(state))
        .route("/api/tasks", post(create_task))
        .route("/api/tasks/{id}", get(task))
        .route("/api/tasks/{id}/cancel", post(cancel))
        .route("/api/accounts", post(add_account))
        .route("/api/accounts/probe", post(probe))
        .route("/api/accounts/{id}/login", post(login))
        .route("/api/ws", get(ws))
        .route_layer(middleware::from_fn_with_state(srv.clone(), require_token))
        .route("/", get(index))
        .layer(middleware::from_fn_with_state(srv.clone(), check_host))
        .with_state(srv.clone());

    let url = format!("http://127.0.0.1:{port}/#t={}", srv.token);
    println!("fermi dashboard: {url}");
    println!("(this link includes your access key; Ctrl+C stops the dashboard and its running tasks)");
    if open {
        open_browser(&url);
    }
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    srv.hub.cancel_all();
    Ok(())
}

async fn bind(port: u16) -> Result<tokio::net::TcpListener> {
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
            .map(|p| json!({ "name": p.name, "mode": p.mode, "accounts": p.accounts, "models": p.models, "paths": p.paths }))
            .collect();
        let models: Vec<Value> =
            e.catalog.iter().map(|m| json!({ "id": m.id, "caps": m.caps, "tier": m.tier, "auto": m.auto })).collect();
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "cwd": self.cwd,
            "here": here,
            "profiles": profiles,
            "accounts": self.accounts_json(),
            "models": models,
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
    // Surface routing errors (unknown profile, no account…) right away.
    if let Err(e) = s.hub.engine().plan(&task) {
        return err(StatusCode::BAD_REQUEST, format!("{e:#}"));
    }
    Json(json!({ "id": s.hub.submit(task) })).into_response()
}

async fn task(State(s): S, Path(id): Path<u64>) -> Response {
    match s.hub.get(id) {
        Some(t) => Json(t).into_response(),
        None => err(StatusCode::NOT_FOUND, "no such task"),
    }
}

async fn cancel(State(s): S, Path(id): Path<u64>) -> Json<Value> {
    Json(json!({ "ok": s.hub.cancel(id) }))
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
    match open_login_terminal(&id) {
        Ok(()) => {
            s.watch_login(id);
            Json(json!({ "message": "A terminal window opened for the login. Finish there and this page updates by itself." }))
                .into_response()
        }
        Err(e) => err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("couldn't open a terminal ({e:#}). Run `fermi account login {id}` yourself."),
        ),
    }
}

/// The CLIs' logins are interactive, so they run in a new terminal window.
fn open_login_terminal(id: &str) -> Result<()> {
    // Account ids are restricted to [A-Za-z0-9_-] when added, but check anyway:
    // the id ends up on a command line.
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("invalid account id");
    }
    let exe = std::env::current_exe()?;
    let args = ["account", "login", id, "--pause"];
    if cfg!(windows) {
        std::process::Command::new("cmd").args(["/c", "start", "fermi login"]).arg(&exe).args(args).spawn()?;
    } else if cfg!(target_os = "macos") {
        let cmd = format!("'{}' {}", exe.display(), args.join(" "));
        let script = format!("tell application \"Terminal\" to do script \"{}\"", cmd.replace('\\', "\\\\").replace('"', "\\\""));
        std::process::Command::new("osascript").args(["-e", &script]).spawn()?;
    } else {
        let terms: [(&str, &[&str]); 4] =
            [("x-terminal-emulator", &["-e"]), ("gnome-terminal", &["--"]), ("konsole", &["-e"]), ("xterm", &["-e"])];
        let ok = terms.iter().any(|(t, pre)| std::process::Command::new(t).args(*pre).arg(&exe).args(args).spawn().is_ok());
        if !ok {
            bail!("no terminal emulator found");
        }
    }
    Ok(())
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
