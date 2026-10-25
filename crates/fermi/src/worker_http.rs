//! `fermi worker`: the HTTP side of an execution service (see
//! `fermi_core::worker`). Every request needs the worker's token and a
//! compatible protocol version (`x-fermi-protocol`).

use anyhow::Result;
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use fermi_core::worker::{JobSpec, Service, HEADER};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

type S = State<Arc<Service>>;

fn fail(code: u16, v: Value) -> Response {
    (StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_REQUEST), Json(v)).into_response()
}

fn err(e: anyhow::Error) -> Response {
    let msg = format!("{e:#}");
    let code = if msg.starts_with("no job") || msg.starts_with("no artifact") { 404 } else { 400 };
    fail(code, json!({ "error": msg }))
}

async fn admit(State(s): S, req: Request, next: Next) -> Response {
    let token = req.headers().get("authorization").and_then(|v| v.to_str().ok()).map(str::to_string);
    let version = req.headers().get(HEADER).and_then(|v| v.to_str().ok()).map(str::to_string);
    let refused = s.admit(token.as_deref(), version.as_deref()).err();
    match refused {
        Some((code, body)) => fail(code, body),
        None => next.run(req).await,
    }
}

async fn hello(State(s): S) -> Json<Value> {
    Json(s.hello())
}

#[derive(Deserialize)]
struct Submit {
    spec: JobSpec,
    /// The workspace, a base64 zip.
    bundle: String,
}

async fn submit(State(s): S, Json(b): Json<Submit>) -> Response {
    use base64::Engine as _;
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&b.bundle) else {
        return fail(400, json!({ "error": "the bundle isn't base64" }));
    };
    match s.submit(b.spec, &bytes) {
        Ok((job, created)) => (StatusCode::ACCEPTED, Json(json!({ "job": job, "created": created }))).into_response(),
        Err(e) => err(e),
    }
}

async fn job(State(s): S, Path(id): Path<String>) -> Response {
    match s.state(&id) {
        Ok(st) => Json(json!(st)).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct After {
    #[serde(default)]
    after: u64,
}

async fn events(State(s): S, Path(id): Path<String>, Query(q): Query<After>) -> Response {
    match s.events(&id, q.after) {
        Ok((evs, st)) => Json(json!({ "events": evs, "job": st })).into_response(),
        Err(e) => err(e),
    }
}

async fn cancel(State(s): S, Path(id): Path<String>) -> Response {
    match s.cancel(&id) {
        Ok(st) => Json(json!(st)).into_response(),
        Err(e) => err(e),
    }
}

async fn artifacts(State(s): S, Path(id): Path<String>) -> Response {
    match s.artifacts(&id) {
        Ok(m) => Json(m).into_response(),
        Err(e) => err(e),
    }
}

async fn artifact(State(s): S, Path((id, name)): Path<(String, String)>) -> Response {
    match s.artifact(&id, &name) {
        Ok(bytes) => ([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], bytes).into_response(),
        Err(e) => err(e),
    }
}

pub fn router(s: Arc<Service>) -> Router {
    Router::new()
        .route("/v1/hello", get(hello))
        .route("/v1/jobs", post(submit))
        .route("/v1/jobs/{id}", get(job))
        .route("/v1/jobs/{id}/events", get(events))
        .route("/v1/jobs/{id}/cancel", post(cancel))
        .route("/v1/jobs/{id}/artifacts", get(artifacts))
        .route("/v1/jobs/{id}/artifacts/{name}", get(artifact))
        .layer(middleware::from_fn_with_state(s.clone(), admit))
        .layer(axum::extract::DefaultBodyLimit::max(512 * 1024 * 1024))
        .with_state(s)
}

/// Serves the worker until the process ends. `on_ready` gets the bound address.
pub async fn serve(
    engine: Arc<fermi_core::Engine>,
    listen: &str,
    token: &str,
    name: &str,
    dir: &std::path::Path,
    on_ready: impl FnOnce(std::net::SocketAddr),
) -> Result<()> {
    if token.is_empty() {
        anyhow::bail!("a worker needs a token (--token or FERMI_WORKER_TOKEN): clients must prove they may run jobs here");
    }
    let service = Service::open(engine, dir, name, token)?;
    let listener = tokio::net::TcpListener::bind(listen).await?;
    on_ready(listener.local_addr()?);
    axum::serve(listener, router(service)).await?;
    Ok(())
}
