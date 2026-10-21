//! The Preview tab's helpers: finding dev servers already running on this
//! PC, and serving a plain folder (static HTML) on its own local port.
//!
//! A preview always lives on a different port than the dashboard, so the
//! browser keeps it on a separate origin: the previewed page can't read the
//! dashboard or call its API (which also needs the access key).

use anyhow::{Context, Result};
use axum::extract::{Path as AxPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Ports dev servers usually pick (Vite, Next/CRA, Angular, Astro, Django, …).
pub const DEV_PORTS: &[u16] = &[3000, 3001, 3002, 4000, 4200, 4321, 5000, 5173, 5174, 5175, 8000, 8001, 8080, 8081, 8888, 9000];

#[derive(Debug, Clone, Serialize)]
pub struct DevServer {
    pub port: u16,
    pub url: String,
    pub title: Option<String>,
}

fn http() -> &'static reqwest::Client {
    static C: OnceLock<reqwest::Client> = OnceLock::new();
    C.get_or_init(|| reqwest::Client::builder().timeout(Duration::from_millis(1500)).build().expect("http client"))
}

/// Dev servers answering on the usual ports, with their page titles.
/// `skip` is the dashboard's own port.
pub async fn scan(skip: u16) -> Vec<DevServer> {
    let probes = DEV_PORTS.iter().copied().filter(|p| *p != skip).map(|port| async move {
        let open = tokio::time::timeout(Duration::from_millis(300), tokio::net::TcpStream::connect(("127.0.0.1", port))).await;
        if !matches!(open, Ok(Ok(_))) {
            return None;
        }
        let url = format!("http://localhost:{port}/");
        let title = match http().get(&url).send().await {
            Ok(r) => r.text().await.ok().and_then(|html| title_of(&html)),
            Err(_) => None,
        };
        Some(DevServer { port, url, title })
    });
    let mut found: Vec<DevServer> = futures_join(probes).await.into_iter().flatten().collect();
    found.sort_by_key(|d| d.port);
    found
}

/// Runs the probes concurrently (no futures crate needed).
async fn futures_join<F: std::future::Future<Output = T> + Send + 'static, T: Send + 'static>(
    futs: impl Iterator<Item = F>,
) -> Vec<T> {
    let handles: Vec<_> = futs.map(tokio::spawn).collect();
    let mut out = vec![];
    for h in handles {
        if let Ok(v) = h.await {
            out.push(v);
        }
    }
    out
}

fn title_of(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open_end = lower[start..].find('>')? + start + 1;
    let end = lower[open_end..].find("</title>")? + open_end;
    let t = html[open_end..end].trim();
    (!t.is_empty()).then(|| t.chars().take(80).collect())
}

// ── static folder server ────────────────────────────────────────────────────

/// Folders being served, by canonical root → port.
fn served() -> &'static Mutex<HashMap<PathBuf, u16>> {
    static S: OnceLock<Mutex<HashMap<PathBuf, u16>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

/// Serves `root` on its own 127.0.0.1 port (reused if already serving).
pub async fn serve(root: PathBuf) -> Result<u16> {
    if let Some(port) = served().lock().unwrap().get(&root) {
        return Ok(*port);
    }
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.context("binding a preview port")?;
    let port = listener.local_addr()?.port();
    let app = Router::new().route("/", get(file)).route("/{*path}", get(file)).with_state(Arc::new(root.clone()));
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    served().lock().unwrap().insert(root, port);
    Ok(port)
}

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("html") | Some("htm") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript; charset=utf-8",
        Some("json") | Some("map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("wasm") => "application/wasm",
        Some("txt") | Some("md") => "text/plain; charset=utf-8",
        Some("xml") => "application/xml",
        _ => "application/octet-stream",
    }
}

async fn file(State(root): State<Arc<PathBuf>>, path: Option<AxPath<String>>) -> Response {
    let rel = path.map(|p| p.0).unwrap_or_default();
    let mut target = root.join(rel.trim_start_matches('/'));
    if target.is_dir() {
        target = target.join("index.html");
    }
    // Only files inside the served folder.
    let Some(canon) = crate::files::canon(&target).filter(|c| c.starts_with(root.as_path())) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    match tokio::fs::read(&canon).await {
        Ok(bytes) => ([(header::CONTENT_TYPE, mime(&canon)), (header::CACHE_CONTROL, "no-store")], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles() {
        assert_eq!(title_of("<html><head><TITLE> Vite App </TITLE>").as_deref(), Some("Vite App"));
        assert_eq!(title_of("<title></title>"), None);
        assert_eq!(title_of("no title here"), None);
    }

    #[tokio::test]
    async fn serves_only_inside_the_root() {
        let base = crate::files::canon(&std::env::temp_dir()).unwrap().join(format!("fermi-preview-{}", std::process::id()));
        let site = base.join("site");
        std::fs::create_dir_all(&site).unwrap();
        std::fs::write(site.join("index.html"), "<title>Hi</title>").unwrap();
        std::fs::write(base.join("secret.txt"), "nope").unwrap();
        let port = serve(crate::files::canon(&site).unwrap()).await.unwrap();
        let get = |p: &str| {
            let url = format!("http://127.0.0.1:{port}{p}");
            async move { reqwest::get(url).await.unwrap() }
        };
        let home = get("/").await;
        assert_eq!(home.status(), 200);
        assert!(home.headers()["content-type"].to_str().unwrap().starts_with("text/html"));
        assert_eq!(get("/../secret.txt").await.status(), 404);
        assert_eq!(get("/%2e%2e/secret.txt").await.status(), 404);
        let _ = std::fs::remove_dir_all(&base);
    }
}
