//! The F-PROVIDER mock: an HTTP model endpoint (Anthropic messages, OpenAI
//! responses and chat completions) that answers from a script, in order, and
//! logs every request. Stands in for the hosted APIs and for a local model
//! server. One per test, on a free local port.

use fermi_core::clock::Clock;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// One scripted answer.
#[derive(Clone)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl Reply {
    pub fn header(mut self, k: &str, v: &str) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }
}

/// HTTP 429, with a Retry-After (seconds) when given.
pub fn rate_limited(retry_after: Option<u64>) -> Reply {
    let r =
        Reply { status: 429, headers: vec![], body: json!({ "error": { "type": "rate_limit_error", "message": "Too many requests" } }) };
    match retry_after {
        Some(s) => r.header("retry-after", &s.to_string()),
        None => r,
    }
}

/// An Anthropic Messages answer.
pub fn messages_ok(text: &str) -> Reply {
    Reply {
        status: 200,
        headers: vec![],
        body: json!({
            "id": "msg_mock", "type": "message", "role": "assistant", "model": "mock",
            "content": [{ "type": "text", "text": text }],
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 12, "output_tokens": 7 }
        }),
    }
}

/// An OpenAI Responses answer.
pub fn responses_ok(text: &str) -> Reply {
    Reply {
        status: 200,
        headers: vec![],
        body: json!({
            "id": "resp_mock", "object": "response", "status": "completed", "model": "mock",
            "output": [{ "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": text }] }],
            "usage": { "input_tokens": 12, "output_tokens": 7, "input_tokens_details": { "cached_tokens": 0 } }
        }),
    }
}

impl Reply {
    /// Sets the token counts, in the answer's own format.
    pub fn usage(mut self, input: u64, output: u64) -> Self {
        let u = &mut self.body["usage"];
        if u.get("prompt_tokens").is_some() {
            u["prompt_tokens"] = json!(input);
            u["completion_tokens"] = json!(output);
            u["total_tokens"] = json!(input + output);
        } else {
            u["input_tokens"] = json!(input);
            u["output_tokens"] = json!(output);
        }
        self
    }
}

/// An OpenAI-compatible chat completion.
pub fn chat_ok(text: &str) -> Reply {
    Reply {
        status: 200,
        headers: vec![],
        body: json!({
            "id": "chatcmpl-mock", "object": "chat.completion", "model": "local-model",
            "choices": [{ "index": 0, "message": { "role": "assistant", "content": text }, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 21, "completion_tokens": 9, "total_tokens": 30 }
        }),
    }
}

pub struct MockApi {
    pub base: String,
    /// Every request: n, method, path, which credentials it carried, body,
    /// and `at` on the clock given (a manual clock's time).
    pub log: Arc<Mutex<Vec<Value>>>,
    script: Arc<Mutex<VecDeque<Reply>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for MockApi {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl MockApi {
    pub fn requests(&self) -> Vec<Value> {
        self.log.lock().unwrap().clone()
    }

    /// Scripted answers not served yet.
    pub fn left(&self) -> usize {
        self.script.lock().unwrap().len()
    }
}

/// Starts the endpoint. Past the end of the script it answers 500.
pub async fn mock_api(script: Vec<Reply>, clock: Option<Clock>) -> MockApi {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let log = Arc::new(Mutex::new(vec![]));
    let script = Arc::new(Mutex::new(VecDeque::from(script)));
    let (l2, s2) = (log.clone(), script.clone());
    let task = tokio::spawn(async move {
        while let Ok((conn, _)) = listener.accept().await {
            let (l, s, c) = (l2.clone(), s2.clone(), clock.clone());
            tokio::spawn(async move {
                let _ = serve(conn, l, s, c).await;
            });
        }
    });
    MockApi { base, log, script, task }
}

async fn serve(
    conn: TcpStream,
    log: Arc<Mutex<Vec<Value>>>,
    script: Arc<Mutex<VecDeque<Reply>>>,
    clock: Option<Clock>,
) -> std::io::Result<()> {
    let mut r = BufReader::new(conn);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("/").to_string());
        let mut len = 0usize;
        let mut creds = vec![];
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).await? == 0 || h == "\r\n" || h == "\n" {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                let k = k.trim().to_ascii_lowercase();
                if k == "content-length" {
                    len = v.trim().parse().unwrap_or(0);
                }
                if k == "authorization" || k == "x-api-key" {
                    creds.push(k);
                }
            }
        }
        let mut body = vec![0u8; len];
        r.read_exact(&mut body).await?;
        let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let reply = script.lock().unwrap().pop_front();
        let reply = reply.unwrap_or(Reply {
            status: 500,
            headers: vec![],
            body: json!({ "error": { "message": "the mock's script is exhausted" } }),
        });
        {
            let mut l = log.lock().unwrap();
            let n = l.len() + 1;
            l.push(json!({
                "n": n, "at": clock.as_ref().map(Clock::now), "method": method, "path": path,
                "credentials": creds, "body": body, "status": reply.status,
                "headers": reply.headers.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>(),
            }));
        }
        let bytes = reply.body.to_string().into_bytes();
        let reason = match reply.status {
            200 => "OK",
            429 => "Too Many Requests",
            _ => "Error",
        };
        let mut head =
            format!("HTTP/1.1 {} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n", reply.status, bytes.len());
        for (k, v) in &reply.headers {
            head += &format!("{k}: {v}\r\n");
        }
        head += "\r\n";
        let w = r.get_mut();
        w.write_all(head.as_bytes()).await?;
        w.write_all(&bytes).await?;
        w.flush().await?;
    }
}

/// An HTTP proxy that lets nothing through and records every request it
/// gets: given to a process as HTTP(S)_PROXY, it shows whether the process
/// tried to reach anything outside (local addresses bypass it, NO_PROXY).
pub struct Recorder {
    pub url: String,
    pub log: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn egress_recorder() -> Recorder {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let log = Arc::new(Mutex::new(vec![]));
    let l2 = log.clone();
    let task = tokio::spawn(async move {
        while let Ok((conn, _)) = listener.accept().await {
            let l = l2.clone();
            tokio::spawn(async move {
                let mut r = BufReader::new(conn);
                let mut line = String::new();
                if r.read_line(&mut line).await.is_ok() && !line.is_empty() {
                    l.lock().unwrap().push(line.trim().to_string());
                }
                let _ = r.get_mut().write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").await;
            });
        }
    });
    Recorder { url, log, task }
}
