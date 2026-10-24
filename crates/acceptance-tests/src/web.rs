//! The F-WEB fixture server: the pages in `tests/fixtures/F-WEB/site` and a
//! small API whose state the tests inspect (what was submitted, and a log of
//! every request). One per test, on a free local port.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

#[derive(Default)]
pub struct WebState {
    pub signups: Vec<Value>,
    pub confirms: Vec<Value>,
    /// Every request: method, path and body.
    pub requests: Vec<Value>,
}

pub struct Web {
    pub base: String,
    pub state: Arc<Mutex<WebState>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Web {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Web {
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

/// Starts the server and waits until it answers `/health`.
pub async fn web_fixture() -> Web {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let state = Arc::new(Mutex::new(WebState::default()));
    let s2 = state.clone();
    let task = tokio::spawn(async move {
        while let Ok((conn, _)) = listener.accept().await {
            let s = s2.clone();
            tokio::spawn(async move {
                let _ = serve(conn, s).await;
            });
        }
    });
    let web = Web { base, state, task };
    for _ in 0..100 {
        if reqwest::get(web.url("/health")).await.is_ok_and(|r| r.status().is_success()) {
            return web;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the F-WEB server didn't become ready");
}

fn site(file: &str) -> Option<Vec<u8>> {
    std::fs::read(crate::repo_root().join("tests/fixtures/F-WEB/site").join(file)).ok()
}

async fn serve(conn: TcpStream, state: Arc<Mutex<WebState>>) -> std::io::Result<()> {
    let mut r = BufReader::new(conn);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/").to_string());
        let mut len = 0usize;
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).await? == 0 || h == "\r\n" || h == "\n" {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                if k.trim().eq_ignore_ascii_case("content-length") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; len];
        r.read_exact(&mut body).await?;
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        state.lock().unwrap().requests.push(json!({ "method": method, "path": path, "body": body }));

        let page = |f: &str| site(f).map(|b| (200, "text/html; charset=utf-8", b));
        let api = |code: u16, v: Value| Some((code, "application/json", v.to_string().into_bytes()));
        let reply: Option<(u16, &str, Vec<u8>)> = match (method.as_str(), path.as_str()) {
            ("GET", "/health") => api(200, json!({ "ok": true })),
            ("GET", "/") => page("index.html"),
            ("GET", "/style.css") => site("style.css").map(|b| (200, "text/css", b)),
            ("GET", "/tour/1") => page("tour-1.html"),
            ("GET", "/tour/2") => page("tour-2.html"),
            ("GET", "/tour/3") => page("tour-3.html"),
            ("GET", "/form") => page("form.html"),
            ("GET", "/error") => page("error.html"),
            ("GET", "/fail") => page("fail.html"),
            ("GET", "/mobile") => page("mobile.html"),
            ("GET", "/modal") => page("modal.html"),
            ("POST", "/api/signup") => {
                let mut s = state.lock().unwrap();
                let rec = json!({ "id": s.signups.len() + 1, "name": body["name"], "email": body["email"] });
                s.signups.push(rec.clone());
                api(201, rec)
            }
            ("GET", "/api/fail") => api(500, json!({ "error": "fixture-failure-marker-500" })),
            ("GET", "/api/slow") => {
                tokio::time::sleep(Duration::from_millis(800)).await;
                api(200, json!({ "ready": true, "hint": "code-7" }))
            }
            ("POST", "/api/confirm") => {
                let mut s = state.lock().unwrap();
                let rec = json!({ "id": s.confirms.len() + 1, "code": body["code"] });
                s.confirms.push(rec.clone());
                api(200, rec)
            }
            ("GET", "/api/records") => {
                let s = state.lock().unwrap();
                api(200, json!({ "signups": s.signups, "confirms": s.confirms }))
            }
            _ => None,
        };
        let (code, kind, bytes) = reply.unwrap_or((404, "text/plain", b"not found".to_vec()));
        let reason = match code {
            200 => "OK",
            201 => "Created",
            404 => "Not Found",
            _ => "Internal Server Error",
        };
        let head = format!(
            "HTTP/1.1 {code} {reason}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\ncache-control: no-store\r\n\r\n",
            bytes.len()
        );
        let w = r.get_mut();
        w.write_all(head.as_bytes()).await?;
        w.write_all(&bytes).await?;
        w.flush().await?;
    }
}
