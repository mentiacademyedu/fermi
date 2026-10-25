//! Task traces: what an agent did, operation by operation, and how to replay it.
//!
//! - [`Ops`]: adapters report model calls and tool invocations through it, so
//!   every operation has an id, its parent call, inputs, a redacted output,
//!   a status and a duration measured on fermi's monotonic clock.
//! - [`Stamp`]: every saved event's sequence number, time and attempt (one
//!   run of the task: the first, a follow-up, a resume).
//! - Tapes: each CLI's responses, redacted, kept in `~/.fermi/tapes/<task>/`
//!   with the task each attempt ran; [`replay`] runs the task again from them
//!   (no CLI started, no network) and compares events and state.
//! - [`export`]: a task's operations as one JSON document.

use crate::config::Config;
use crate::engine::{Engine, Task};
use crate::events::{emit, Event, Tx};
use crate::hub::{self, TaskRec};
use crate::providers::RunCtx;
use crate::redact::Redactor;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Longest tool output kept in a trace (characters).
const OUTPUT_MAX: usize = 4000;

/// Where one saved event sits in its task's trace.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Stamp {
    /// 1, 2, 3… across every attempt, without gaps.
    pub seq: u64,
    /// Unix milliseconds (0 when unknown: events saved before stamps existed).
    pub at: u64,
    /// The run of the task it belongs to (0 when unknown).
    pub attempt: u32,
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max * 2 / 3).collect();
    let tail: Vec<char> = s.chars().rev().take(max / 3).collect();
    format!("{head}\n[… cut …]\n{}", tail.into_iter().rev().collect::<String>())
}

// ── operations, as adapters see them ────────────────────────────────────────

/// Tracks one CLI run's tool invocations so each gets exactly one result.
pub struct Ops<'a> {
    tx: &'a Tx,
    /// Started tools waiting for their result.
    open: Vec<(String, Instant)>,
    seen: std::collections::HashSet<String>,
    /// Every invocation so far: (id, `name detail`, how it ended).
    ran: Vec<(String, String, String)>,
    n: u32,
    /// Replays measure no durations (they would be the replay's, not the run's).
    timed: bool,
}

impl<'a> Ops<'a> {
    pub fn new(ctx: &RunCtx<'a>) -> Ops<'a> {
        Ops { tx: ctx.tx, open: vec![], seen: Default::default(), ran: vec![], n: 0, timed: ctx.engine.replay.is_none() }
    }

    /// A tool invocation starts. Returns its id (the provider's, else one made up).
    pub fn tool(&mut self, id: Option<&str>, name: &str, input: &Value, call: Option<&str>) -> String {
        let id = match id.filter(|i| !i.is_empty()) {
            Some(i) => i.to_string(),
            None => {
                self.n += 1;
                format!("op-{}", self.n)
            }
        };
        // The same invocation reported again is not a second one.
        if self.seen.insert(id.clone()) {
            self.open.push((id.clone(), Instant::now()));
            let line = format!("{name} {}", crate::providers::brief(input));
            self.ran.push((id.clone(), line.trim_end().to_string(), "started".into()));
            emit(
                self.tx,
                Event::Tool {
                    name: name.to_string(),
                    detail: crate::providers::brief(input),
                    id: Some(id.clone()),
                    call: call.map(str::to_string),
                    input: Some(input.clone()),
                },
            );
        }
        id
    }

    /// How an invocation ended (`ok`, `error`, `declined`).
    pub fn result(&mut self, id: &str, status: &str, output: &str, code: Option<String>) {
        let Some(i) = self.open.iter().position(|(o, _)| o == id) else { return };
        let (_, started) = self.open.remove(i);
        if let Some(r) = self.ran.iter_mut().find(|r| r.0 == id) {
            r.2 = status.to_string();
        }
        let ms = self.timed.then(|| started.elapsed().as_millis() as u64);
        emit(self.tx, Event::ToolResult { id: id.to_string(), status: status.to_string(), output: clip(output, OUTPUT_MAX), code, ms });
    }

    /// One model call finished; `usage` is the provider's own usage object.
    pub fn call(&mut self, id: &str, model: Option<&str>, usage: &Value) {
        emit(self.tx, Event::Call { id: id.to_string(), model: model.map(str::to_string), usage: usage.clone() });
    }

    /// The run is over: tools that never reported back are recorded as such.
    /// Returns every invocation, as `name detail (status)`.
    pub fn finish(&mut self) -> Vec<String> {
        for (id, _) in std::mem::take(&mut self.open) {
            emit(
                self.tx,
                Event::ToolResult { id, status: "no_result".into(), output: String::new(), code: Some("no_result".into()), ms: None },
            );
        }
        std::mem::take(&mut self.ran)
            .into_iter()
            .map(|(_, line, status)| format!("{line} ({})", if status == "started" { "no result" } else { &status }))
            .collect()
    }
}

/// The text of a tool result's content (a string, or text blocks).
pub fn content_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n"),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

/// `Exit code 2` at the start of a shell tool's error output.
pub fn exit_code(output: &str) -> Option<String> {
    let rest = output.trim_start().strip_prefix("Exit code ")?;
    let n: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '-').collect();
    (!n.is_empty()).then(|| format!("exit {n}"))
}

// ── tapes ───────────────────────────────────────────────────────────────────

pub fn tapes_dir(home: &Path, task: u64) -> PathBuf {
    home.join("tapes").join(task.to_string())
}

fn tapes_on(engine: &Engine) -> bool {
    engine.cfg.trace.tapes != Some(false) && engine.replay.is_none()
}

/// Saves the task an attempt runs (redacted), so it can be replayed.
pub fn save_task(engine: &Engine, task: &Task) {
    let Some(id) = task.id.filter(|_| tapes_on(engine)) else { return };
    let dir = tapes_dir(&engine.home, id);
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut v) = serde_json::to_value(task) {
        engine.redactor.value(&mut v);
        let _ = std::fs::write(dir.join(format!("{}.task.json", task.attempt)), serde_json::to_vec_pretty(&v).unwrap_or_default());
    }
}

/// One CLI invocation's responses, written as they are read.
pub struct TapeOut {
    file: std::fs::File,
    redactor: Arc<Redactor>,
}

// The lines a tape gets are already redacted (see `Proc::next_json`); the
// header and the stderr tail are redacted here.

impl TapeOut {
    pub fn create(ctx: &RunCtx<'_>, args: &[String]) -> Option<TapeOut> {
        let e = ctx.engine;
        let id = ctx.task.id.filter(|_| tapes_on(e))?;
        let dir = tapes_dir(&e.home, id);
        std::fs::create_dir_all(&dir).ok()?;
        let a = ctx.task.attempt;
        let k = (1..).find(|k| !dir.join(format!("{a}-{k}.jsonl")).exists())?;
        let mut file = std::fs::File::create(dir.join(format!("{a}-{k}.jsonl"))).ok()?;
        let mut head =
            json!({ "tape": { "task": id, "attempt": a, "call": k, "account": ctx.account.id, "model": ctx.model.id, "args": args } });
        e.redactor.value(&mut head);
        writeln!(file, "{head}").ok()?;
        Some(TapeOut { file, redactor: e.redactor.clone() })
    }

    /// A (redacted) line of the CLI's output. Streamed text chunks are left
    /// out: the finished messages carry the same text, and history never keeps chunks.
    pub fn line(&mut self, line: &str) {
        if line.trim().is_empty() || line.contains("\"type\":\"stream_event\"") {
            return;
        }
        let _ = writeln!(self.file, "{line}");
    }

    pub fn end(&mut self, success: bool, stderr: &str) {
        let _ = writeln!(self.file, "{}", json!({ "tape_end": { "success": success, "stderr": self.redactor.text(stderr) } }));
    }
}

/// A recorded CLI invocation, read back.
pub struct Tape {
    pub lines: VecDeque<String>,
    /// (success, stderr tail); None when the recording stops early (the run was interrupted).
    pub end: Option<(bool, String)>,
}

/// Logged by a replay where a recording stops early; the replay ends the attempt there.
pub const TAPE_CUT: &str = "replay: the recording ends here (the original run was interrupted)";

/// Serves recorded invocations in order, in place of the CLIs.
pub struct Replay {
    dir: PathBuf,
    next: Mutex<HashMap<u32, u32>>,
}

impl Replay {
    pub fn new(dir: PathBuf) -> Replay {
        Replay { dir, next: Mutex::new(HashMap::new()) }
    }

    fn path(&self, attempt: u32) -> PathBuf {
        let k = *self.next.lock().unwrap().get(&attempt).unwrap_or(&1);
        self.dir.join(format!("{attempt}-{k}.jsonl"))
    }

    fn header(path: &Path) -> Result<Value> {
        let text = std::fs::read_to_string(path).with_context(|| format!("no recording {} to replay", path.display()))?;
        let first = text.lines().next().unwrap_or_default();
        Ok(serde_json::from_str::<Value>(first)?["tape"].clone())
    }

    /// The provider session the next recorded invocation used (`--session-id`).
    pub fn session(&self, task: &Task) -> Result<Option<String>> {
        let head = Self::header(&self.path(task.attempt))?;
        let args: Vec<&str> = head["args"].as_array().into_iter().flatten().filter_map(|a| a.as_str()).collect();
        Ok(args.iter().position(|a| *a == "--session-id").and_then(|i| args.get(i + 1)).map(|s| s.to_string()))
    }

    /// The next recorded invocation, which must have run on the account the replay routed to.
    pub fn open(&self, ctx: &RunCtx<'_>) -> Result<Tape> {
        let path = self.path(ctx.task.attempt);
        let head = Self::header(&path)?;
        if head["account"].as_str() != Some(ctx.account.id.as_str()) {
            bail!("the replay diverged: the recorded call ran on `{}`, the replay routed to `{}`", head["account"], ctx.account.id);
        }
        *self.next.lock().unwrap().entry(ctx.task.attempt).or_insert(1) += 1;
        let text = std::fs::read_to_string(&path)?;
        let mut lines: VecDeque<String> = text.lines().skip(1).map(str::to_string).collect();
        let end = match lines.back().and_then(|l| serde_json::from_str::<Value>(l).ok()) {
            Some(v) if v["tape_end"].is_object() => {
                lines.pop_back();
                Some((
                    v["tape_end"]["success"].as_bool().unwrap_or(false),
                    v["tape_end"]["stderr"].as_str().unwrap_or_default().to_string(),
                ))
            }
            _ => None,
        };
        Ok(Tape { lines, end })
    }
}

// ── export ──────────────────────────────────────────────────────────────────

/// A task's operations (model calls and tool invocations), correlated by id,
/// with the task, attempt and sequence number of each.
pub fn export(rec: &TaskRec) -> Value {
    let stamps = rec.stamps();
    let mut ops: Vec<Value> = vec![];
    // Everything in order, from the request to the outcome; tools and model
    // calls appear by id (their details are in `operations`).
    let mut timeline: Vec<Value> = vec![];
    let mut revision: Option<Value> = None;
    // Which try within the attempt (a failover to another account starts the next).
    let (mut tries, mut attempt) = (0u32, u32::MAX);
    for (ev, st) in rec.events.iter().zip(&stamps) {
        if st.attempt != attempt {
            (tries, attempt) = (0, st.attempt);
        }
        let at = |kind: &str| json!({ "kind": kind, "attempt": st.attempt, "seq": st.seq, "at": st.at });
        let mut step = |kind: &str, fields: Value| {
            let mut v = at(kind);
            if let (Some(o), Value::Object(f)) = (v.as_object_mut(), fields) {
                o.extend(f);
            }
            timeline.push(v);
        };
        match ev {
            Event::User { text } => step("request", json!({ "text": text })),
            Event::Routed { profile, model, account, why, .. } => {
                tries += 1;
                step("route", json!({ "try": tries, "profile": profile, "model": model, "account": account, "why": why }));
            }
            Event::Failover { account, reason } => step("failover", json!({ "try": tries, "account": account, "reason": reason })),
            Event::Verify { attempt: repair, passed, report } => {
                for c in report["checks"].as_array().into_iter().flatten() {
                    step(
                        "check",
                        json!({ "repair": repair, "name": c["name"], "command": c["command"], "status": c["status"], "exit_code": c["exit_code"],
                            "duration_ms": c["duration_ms"], "revision": report["fingerprint"] }),
                    );
                }
                step("checks", json!({ "repair": repair, "passed": passed, "revision": report["fingerprint"] }));
                revision = Some(
                    json!({ "fingerprint": report["fingerprint"], "commit": report["revision"], "dirty": report["dirty"], "passed": passed }),
                );
            }
            Event::Done { ok, summary } => step("outcome", json!({ "ok": ok, "summary": summary })),
            _ => {}
        }
        match ev {
            Event::Tool { id, .. } => {
                timeline.push(json!({ "kind": "tool", "attempt": st.attempt, "seq": st.seq, "at": st.at, "id": id.clone().unwrap_or_else(|| format!("seq-{}", st.seq)) }))
            }
            Event::Call { id, .. } => timeline.push(json!({ "kind": "model_call", "attempt": st.attempt, "seq": st.seq, "at": st.at, "id": id })),
            _ => {}
        }
        match ev {
            Event::Tool { name, detail, id, call, input } => ops.push(json!({
                "kind": "tool", "id": id.clone().unwrap_or_else(|| format!("seq-{}", st.seq)), "task": rec.id,
                "attempt": st.attempt, "try": tries, "seq": st.seq, "at": st.at, "call": call, "name": name, "detail": detail,
                "input": input, "status": "unknown", "output": null, "code": null, "ms": null, "result_seq": null,
            })),
            Event::ToolResult { id, status, output, code, ms } => {
                let op = ops.iter_mut().rev().find(|o| o["kind"] == "tool" && o["id"] == id.as_str() && o["result_seq"].is_null());
                if let Some(o) = op {
                    o["status"] = json!(status);
                    o["output"] = json!(output);
                    o["code"] = json!(code);
                    o["ms"] = json!(ms);
                    o["result_seq"] = json!(st.seq);
                }
            }
            Event::Call { id, model, usage } => ops.push(json!({
                "kind": "model_call", "id": id, "task": rec.id, "attempt": st.attempt, "try": tries, "seq": st.seq, "at": st.at,
                "model": model, "usage": usage, "status": "ok",
            })),
            _ => {}
        }
    }
    let failures: Vec<&Value> = ops.iter().filter(|o| o["status"] != "ok").collect();
    json!({
        "task": rec.id, "status": rec.status, "attempts": rec.attempt, "events": rec.events.len(), "prompt": rec.prompt, "cwd": rec.cwd,
        "workspace": rec.workspace, "last_seq": stamps.last().map(|s| s.seq), "operations": ops, "failures": failures,
        "timeline": timeline, "tested_revision": revision,
    })
}

// ── replay ──────────────────────────────────────────────────────────────────

/// What a replay found.
#[derive(Debug, Serialize)]
pub struct ReplayReport {
    pub task: u64,
    pub attempts: Vec<u32>,
    /// Each recording used, with its SHA-256.
    pub tapes: Vec<(String, String)>,
    pub matches: bool,
    pub differences: Vec<String>,
    pub original_hash: String,
    pub replay_hash: String,
    /// Events compared (log lines aside).
    pub compared: usize,
    /// Always "tape": the text is the recorded text, not regenerated.
    pub text_source: &'static str,
    #[serde(skip)]
    pub replayed: Option<TaskRec>,
}

/// The parts of a task a replay must reproduce: its events (without log lines,
/// whose stderr timing isn't recorded, and without durations) and its state.
pub fn normalized(rec: &TaskRec) -> Value {
    let events: Vec<Value> = rec
        .events
        .iter()
        .zip(rec.stamps())
        .filter(|(e, _)| !matches!(e, Event::Log { .. }))
        .map(|(e, st)| {
            let mut v = serde_json::to_value(e).unwrap_or_default();
            if let Some(o) = v.as_object_mut() {
                o.remove("ms");
            }
            json!({ "attempt": st.attempt, "event": v })
        })
        .collect();
    json!({
        "status": rec.status, "route": rec.route, "ran_on": rec.ran_on, "session": rec.session,
        "past_sessions": rec.past_sessions, "attempts": rec.attempt, "events": events,
    })
}

pub fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

/// Runs task `id` of `home` again from its tapes, in `scratch` (its own fermi
/// home, so the real usage log and budgets are untouched), on a fixed clock.
/// No CLI is started and nothing goes over the network.
pub async fn replay(cfg: &Config, home: &Path, id: u64, scratch: &Path) -> Result<ReplayReport> {
    let original = hub::read_task(&hub::history_dir(home), id).with_context(|| format!("no task {id}"))?;
    let dir = tapes_dir(home, id);
    let mut attempts: Vec<u32> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".task.json")?.parse().ok())
        .collect();
    attempts.sort();
    if attempts.is_empty() {
        bail!("task {id} has no recordings (it ran before fermi kept them, or with [trace] tapes = false)");
    }
    let mut tapes = vec![];
    let mut names: Vec<String> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok()?.file_name().to_str().map(str::to_string))
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    names.sort();
    for n in names {
        tapes.push((n.clone(), sha256(&std::fs::read(dir.join(&n))?)));
    }

    std::fs::create_dir_all(scratch)?;
    let mut engine = Engine::new(cfg.clone(), scratch.to_path_buf(), 0);
    engine.replay = Some(Replay::new(dir.clone()));
    let mut clock = 1_000u64; // the fixed clock: one tick per event
    let mut tick = || {
        clock += 1;
        clock
    };
    let mut rec: Option<TaskRec> = None;
    for &a in &attempts {
        let task: Task = serde_json::from_slice(&std::fs::read(dir.join(format!("{a}.task.json")))?)?;
        if task.verify.is_some() {
            bail!("task {id} ran with verification; replaying the checks isn't supported yet");
        }
        let r = match rec.as_mut() {
            None => rec.insert(TaskRec::new_at(id, &task, "replay", tick())),
            Some(r) => {
                r.reopen(a, &task.prompt, tick());
                r
            }
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let _ = engine.run(&task, &tx).await;
        drop(tx);
        while let Some(ev) = rx.recv().await {
            if matches!(&ev, Event::Log { line } if line == TAPE_CUT) {
                r.cancel_at(tick());
                break;
            }
            r.apply_at(&engine.redactor.event(&ev), tick());
        }
    }
    let replayed = rec.expect("at least one attempt");
    let (o, n) = (normalized(&original), normalized(&replayed));
    let mut differences = vec![];
    for k in ["status", "route", "ran_on", "session", "past_sessions", "attempts"] {
        if o[k] != n[k] {
            differences.push(format!("{k}: recorded {}, replayed {}", o[k], n[k]));
        }
    }
    let (oe, ne) = (o["events"].as_array().cloned().unwrap_or_default(), n["events"].as_array().cloned().unwrap_or_default());
    if oe.len() != ne.len() {
        differences.push(format!("{} events recorded, {} replayed", oe.len(), ne.len()));
    }
    for (i, (x, y)) in oe.iter().zip(&ne).enumerate().filter(|(_, (x, y))| x != y).take(5) {
        differences.push(format!("event {}: recorded {x}, replayed {y}", i + 1));
    }
    Ok(ReplayReport {
        task: id,
        attempts,
        tapes,
        matches: differences.is_empty(),
        differences,
        original_hash: sha256(o.to_string().as_bytes()),
        replay_hash: sha256(n.to_string().as_bytes()),
        compared: oe.len(),
        text_source: "tape",
        replayed: Some(replayed),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_and_content() {
        assert_eq!(exit_code("Exit code 2\nerror[E0425]").as_deref(), Some("exit 2"));
        assert_eq!(exit_code("all good"), None);
        assert_eq!(content_text(&json!([{ "type": "text", "text": "a" }, { "type": "text", "text": "b" }])), "a\nb");
    }
}
