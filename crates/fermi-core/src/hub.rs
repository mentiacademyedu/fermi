//! Tasks and their shared history. Every front end on this PC (desktop app,
//! `fermi ui`, the TUI and `fermi run`) writes its tasks to
//! `~/.fermi/history/<id>.json` and picks up the others' changes, so a
//! conversation started anywhere shows up everywhere.
//!
//! The [`Hub`] runs tasks for long-lived front ends and broadcasts updates to
//! their clients; [`Recorder`] lets one-shot front ends write the same files.

use crate::engine::{Engine, Task};
use crate::events::Event;
use crate::trace::Stamp;
use crate::util::now;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{broadcast, mpsc};
use tokio::task::AbortHandle;

/// How many past tasks are loaded at startup (older ones stay on disk).
const HISTORY_LIMIT: usize = 500;
/// A running task re-saves at least this often, so others know it's alive.
pub const HEARTBEAT: Duration = Duration::from_secs(5);
/// A "running" task whose file hasn't been touched this long was interrupted
/// (its program closed or crashed).
const STALE_SECS: u64 = 20;
/// While a task runs, its file is rewritten at most this often.
const SAVE_EVERY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRec {
    pub id: u64,
    /// The first prompt; the conversation's title.
    pub prompt: String,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub account: Option<String>,
    pub effort: Option<String>,
    #[serde(default)]
    pub permission: Option<String>,
    pub cwd: PathBuf,
    pub status: TaskStatus,
    /// `Model · effort @ account` once routed.
    pub route: Option<String>,
    pub started: u64,
    pub ended: Option<u64>,
    /// Where the latest turn ran, and its provider session (for follow-ups).
    #[serde(default)]
    pub ran_on: Option<RanOn>,
    #[serde(default)]
    pub session: Option<String>,
    /// The team run this agent belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<crate::engine::TeamRef>,
    /// Tool rules the user allowed for this conversation (see `Task::allowed`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
    /// fermi checks the agent's work and sends failures back (see `verify`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify: Option<crate::verify::VerifyOpts>,
    /// Sessions earlier turns ran in (before a switch to another provider).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub past_sessions: Vec<String>,
    /// Last write (unix seconds); how other programs tell a live task from an abandoned one.
    #[serde(default)]
    pub updated: u64,
    /// Which front end ran it: "desktop", "ui", "tui", "cli", or the tool
    /// an imported session came from ("claude-code", "codex", "gemini").
    #[serde(default)]
    pub via: Option<String>,
    /// For an imported session, `<tool>:<session id>` (see `sessions::Found::origin`).
    #[serde(default)]
    pub origin: Option<String>,
    pub events: Vec<Event>,
    /// Runs of this task so far (the first, follow-ups, resumes).
    #[serde(default)]
    pub attempt: u32,
    /// Sequence number, time and attempt of each event (see `trace::Stamp`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stamps: Vec<Stamp>,
    /// The worker job this task runs as (see `remote`), and the last of its events seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<RemoteRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoteRef {
    pub worker: String,
    pub job: String,
    pub seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RanOn {
    pub profile: String,
    pub model: String,
    pub account: String,
}

impl TaskRec {
    pub fn new(id: u64, task: &Task, via: &str) -> Self {
        Self::new_at(id, task, via, now_ms())
    }

    /// A new record on a given clock (unix milliseconds).
    pub fn new_at(id: u64, task: &Task, via: &str, at: u64) -> Self {
        let mut rec = TaskRec {
            id,
            prompt: task.prompt.clone(),
            profile: task.profile.clone(),
            model: task.model.clone(),
            account: task.account.clone(),
            effort: task.effort.clone(),
            permission: task.permission.clone(),
            cwd: task.cwd.clone(),
            status: TaskStatus::Running,
            route: None,
            started: now(),
            ended: None,
            ran_on: None,
            session: None,
            past_sessions: vec![],
            allowed: task.allowed.clone(),
            verify: task.verify.clone(),
            team: task.team.clone(),
            updated: now(),
            via: Some(via.to_string()),
            origin: None,
            events: vec![],
            attempt: task.attempt.max(1),
            stamps: vec![],
            remote: None,
        };
        rec.started = at / 1000;
        rec.updated = at / 1000;
        rec.push_at(Event::User { text: task.prompt.clone() }, at);
        rec
    }

    /// Adds an event to the trace with the next sequence number.
    pub fn push_at(&mut self, ev: Event, at: u64) {
        // Events saved before stamps existed get theirs now (time and attempt unknown).
        while self.stamps.len() < self.events.len() {
            let seq = self.stamps.len() as u64 + 1;
            self.stamps.push(Stamp { seq, at: 0, attempt: 0 });
        }
        let seq = self.stamps.last().map_or(0, |s| s.seq) + 1;
        self.events.push(ev);
        self.stamps.push(Stamp { seq, at, attempt: self.attempt });
    }

    /// Every event's stamp (made up for events saved before stamps existed).
    pub fn stamps(&self) -> Vec<Stamp> {
        let missing = self.events.len().saturating_sub(self.stamps.len());
        let mut out: Vec<Stamp> = (1..=missing as u64).map(|seq| Stamp { seq, at: 0, attempt: 0 }).collect();
        out.extend(self.stamps.iter().copied());
        out.truncate(self.events.len());
        out
    }

    /// Starts the next attempt with the user's message.
    pub fn reopen(&mut self, attempt: u32, prompt: &str, at: u64) {
        self.attempt = attempt;
        self.status = TaskStatus::Running;
        self.ended = None;
        self.push_at(Event::User { text: prompt.to_string() }, at);
    }

    /// Removes secrets from what this record keeps so far (see `redact`).
    pub fn redact(&mut self, r: &crate::redact::Redactor) {
        self.prompt = r.text(&self.prompt).into_owned();
        for ev in &mut self.events {
            *ev = r.event(ev);
        }
    }

    /// Everything but the event log.
    pub fn summary(&self) -> Value {
        json!({
            "id": self.id, "prompt": self.prompt, "profile": self.profile, "model": self.model,
            "account": self.account, "effort": self.effort, "permission": self.permission, "cwd": self.cwd, "status": self.status,
            "route": self.route, "started": self.started, "ended": self.ended, "via": self.via,
            "resumable": self.session.is_some() && self.ran_on.is_some(),
            "ran_model": self.ran_on.as_ref().map(|r| &r.model), "imported": self.origin.is_some(), "team": self.team,
            "turns": self.events.iter().filter(|e| matches!(e, Event::User { .. })).count(),
            "verify": self.verify,
            "checks": self.events.iter().rev().find_map(|e| match e { Event::Verify { passed, .. } => Some(*passed), _ => None }),
        })
    }

    /// Folds one event into the record. Returns true when the run finished.
    pub fn apply(&mut self, ev: &Event) -> bool {
        self.apply_at(ev, now_ms())
    }

    pub fn apply_at(&mut self, ev: &Event, at: u64) -> bool {
        if self.status != TaskStatus::Running {
            return false; // cancelled: drop late events
        }
        let mut finished = false;
        match ev {
            Event::Routed { profile, model, label, effort, account, .. } => {
                let effort = effort.as_deref().map(|e| format!(" · {e}")).unwrap_or_default();
                self.route = Some(format!("{label}{effort} @ {account}"));
                self.ran_on = Some(RanOn { profile: profile.clone(), model: model.clone(), account: account.clone() });
            }
            Event::Session { id } => {
                if let Some(old) = self.session.replace(id.clone()).filter(|old| old != id) {
                    self.past_sessions.push(old);
                }
            }
            Event::Done { ok, .. } => {
                self.status = if *ok { TaskStatus::Done } else { TaskStatus::Failed };
                self.ended = Some(at / 1000);
                finished = true;
            }
            Event::Remote { worker, job, seq } => {
                self.remote = Some(RemoteRef { worker: worker.clone(), job: job.clone(), seq: *seq });
            }
            _ => {}
        }
        // Session ids are bookkeeping, streamed chunks are superseded by the
        // finished `Text`, and a worker's progress marks only move `remote`:
        // none of them belongs in the saved history (a dispatch does).
        let progress = matches!(ev, Event::Remote { seq, .. } if *seq > 0);
        if !matches!(ev, Event::Session { .. } | Event::Delta { .. }) && !progress {
            self.push_at(ev.clone(), at);
        }
        finished
    }

    fn cancel(&mut self) {
        self.cancel_at(now_ms());
    }

    pub fn cancel_at(&mut self, at: u64) {
        if self.status == TaskStatus::Running {
            self.push_at(Event::Log { line: "cancelled".into() }, at);
            self.status = TaskStatus::Cancelled;
            self.ended = Some(at / 1000);
        }
    }

    /// Running according to the file, but nobody has touched it in a while.
    /// A task on a worker isn't: its job runs on without any window.
    pub fn is_stale(&self, at: u64) -> bool {
        self.status == TaskStatus::Running && self.remote.is_none() && at.saturating_sub(self.updated) > STALE_SECS
    }

    fn mark_interrupted(&mut self) {
        self.status = TaskStatus::Cancelled;
        self.ended.get_or_insert(self.updated.max(self.started));
        self.push_at(Event::Log { line: "interrupted: the fermi window running this was closed".into() }, now_ms());
    }
}

// ── the history folder ──────────────────────────────────────────────────────

pub fn history_dir(home: &Path) -> PathBuf {
    home.join("history")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Unique across every program writing the folder: milliseconds plus a random
/// part (and still a safe integer in JavaScript).
pub fn new_task_id(dir: &Path) -> u64 {
    loop {
        let mut b = [0u8; 2];
        let _ = getrandom::fill(&mut b);
        let id = now_ms() * 1000 + u16::from_le_bytes(b) as u64 % 1000;
        if !dir.join(format!("{id}.json")).exists() {
            return id;
        }
    }
}

/// An unused id for something that started at `ms` (imports keep their date order).
pub fn id_at(dir: &Path, ms: u64) -> u64 {
    let mut id = ms * 1000;
    while dir.join(format!("{id}.json")).exists() {
        id += 1;
    }
    id
}

/// Atomic write; returns the file's new modification time.
fn write_rec(dir: &Path, rec: &mut TaskRec) -> Option<SystemTime> {
    rec.updated = now();
    let bytes = serde_json::to_vec(rec).ok()?;
    let path = dir.join(format!("{}.json", rec.id));
    let tmp = dir.join(format!("{}.{}.tmp", rec.id, std::process::id()));
    std::fs::write(&tmp, bytes).ok()?;
    std::fs::rename(&tmp, &path).ok()?;
    std::fs::metadata(&path).and_then(|m| m.modified()).ok()
}

/// For a task another program was running: if its file has gone quiet,
/// re-checks the file and, when still quiet, marks it interrupted and saves.
/// Returns the settled record when it changed.
pub fn settle_if_stale(dir: &Path, id: u64) -> Option<TaskRec> {
    let mut t = read_task(dir, id)?;
    if !t.is_stale(now()) {
        return None;
    }
    t.mark_interrupted();
    write_rec(dir, &mut t);
    Some(t)
}

pub fn read_task(dir: &Path, id: u64) -> Option<TaskRec> {
    serde_json::from_slice(&std::fs::read(dir.join(format!("{id}.json"))).ok()?).ok()
}

fn task_id(path: &Path) -> Option<u64> {
    if path.extension().is_some_and(|x| x == "json") {
        path.file_stem()?.to_str()?.parse().ok()
    } else {
        None
    }
}

/// The newest `limit` tasks, oldest first. Abandoned "running" tasks are
/// marked interrupted (and saved that way).
pub fn load_history(dir: &Path, limit: usize) -> Vec<TaskRec> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut ids: Vec<u64> = entries.filter_map(|e| task_id(&e.ok()?.path())).collect();
    ids.sort_unstable();
    let skip = ids.len().saturating_sub(limit);
    let at = now();
    ids[skip..]
        .iter()
        .filter_map(|id| read_task(dir, *id))
        .map(|mut t| {
            if t.is_stale(at) {
                t.mark_interrupted();
                write_rec(dir, &mut t);
            }
            t
        })
        .collect()
}

/// Spots history files other programs created, changed or deleted.
#[derive(Default)]
pub struct Scanner {
    seen: HashMap<u64, SystemTime>,
}

impl Scanner {
    pub fn new(dir: &Path) -> Self {
        let mut s = Scanner::default();
        s.scan(dir); // everything present now counts as seen
        s
    }

    /// Records our own write so it doesn't come back as someone else's change.
    pub fn note(&mut self, id: u64, mtime: SystemTime) {
        self.seen.insert(id, mtime);
    }

    /// Changed or new records (stale ones already marked interrupted) and
    /// ids whose files disappeared.
    pub fn scan(&mut self, dir: &Path) -> (Vec<TaskRec>, Vec<u64>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return (vec![], vec![]) };
        let mut present = HashMap::new();
        for e in entries.filter_map(|e| e.ok()) {
            if let (Some(id), Ok(mtime)) = (task_id(&e.path()), e.metadata().and_then(|m| m.modified())) {
                present.insert(id, mtime);
            }
        }
        let at = now();
        let mut changed = vec![];
        for (id, mtime) in &present {
            if self.seen.get(id) == Some(mtime) {
                continue;
            }
            if let Some(mut t) = read_task(dir, *id) {
                if t.is_stale(at) {
                    t.mark_interrupted();
                    if let Some(m) = write_rec(dir, &mut t) {
                        self.seen.insert(*id, m);
                        changed.push(t);
                        continue;
                    }
                }
                changed.push(t);
            }
            self.seen.insert(*id, *mtime);
        }
        let removed: Vec<u64> = self.seen.keys().filter(|id| !present.contains_key(id)).copied().collect();
        for id in &removed {
            self.seen.remove(id);
        }
        changed.sort_by_key(|t| t.id);
        (changed, removed)
    }
}

/// Writes one task's history from a front end that doesn't use the Hub
/// (`fermi run`, the TUI). Dropping it while the task runs marks it cancelled.
pub struct Recorder {
    dir: PathBuf,
    rec: TaskRec,
    last_save: Instant,
    redactor: Arc<crate::redact::Redactor>,
}

impl Recorder {
    /// Gives `task` its id and records it (and, for replay, what it runs).
    pub fn start(engine: &Engine, task: &mut Task, via: &str) -> Self {
        let dir = history_dir(&engine.home);
        let _ = std::fs::create_dir_all(&dir);
        let mut rec = TaskRec::new(new_task_id(&dir), task, via);
        rec.redact(&engine.redactor);
        task.id = Some(rec.id);
        task.attempt = 1;
        crate::trace::save_task(engine, task);
        write_rec(&dir, &mut rec);
        Recorder { dir, rec, last_save: Instant::now(), redactor: engine.redactor.clone() }
    }

    pub fn id(&self) -> u64 {
        self.rec.id
    }

    pub fn record(&mut self, ev: &Event) {
        let finished = self.rec.apply(&self.redactor.event(ev));
        if finished || self.last_save.elapsed() >= SAVE_EVERY {
            self.save();
        }
    }

    /// Call every few seconds while waiting, so a long tool run isn't mistaken for a crash.
    pub fn heartbeat(&mut self) {
        if self.rec.status == TaskStatus::Running && self.last_save.elapsed() >= HEARTBEAT {
            self.save();
        }
    }

    fn save(&mut self) {
        // Resumed elsewhere since: leave the newer attempt alone.
        if read_task(&self.dir, self.rec.id).is_some_and(|disk| disk.attempt > self.rec.attempt) {
            return;
        }
        write_rec(&self.dir, &mut self.rec);
        self.last_save = Instant::now();
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.rec.status == TaskStatus::Running {
            self.rec.cancel();
            self.save();
        } else {
            // Make sure the final state is on disk even if the last save was throttled.
            self.save();
        }
    }
}

// ── continuing a conversation ───────────────────────────────────────────────

/// Changes a follow-up may make.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FollowUp {
    /// Switch to this model (`claude:sonnet`); None keeps the current one.
    pub model: Option<String>,
    /// New effort; None keeps it, `""` goes back to the default.
    pub effort: Option<String>,
    /// Tool rules to allow from now on (after the agent was blocked).
    #[serde(default)]
    pub allow: Vec<String>,
    /// New permission mode for this conversation (e.g. `bypassPermissions`).
    #[serde(default)]
    pub permission: Option<String>,
}

fn provider_of(model: &str) -> &str {
    model.split(':').next().unwrap_or("")
}

/// The task that continues `t` with `prompt`.
pub fn continuation(engine: &Engine, t: &TaskRec, prompt: &str, opts: &FollowUp) -> Task {
    let mut task = Task::new(prompt, t.cwd.clone());
    task.team = t.team.clone();
    task.verify = t.verify.clone();
    task.permission = opts.permission.clone().or_else(|| t.permission.clone());
    task.allowed = t.allowed.clone();
    for r in &opts.allow {
        if !task.allowed.contains(r) {
            task.allowed.push(r.clone());
        }
    }
    task.effort = match &opts.effort {
        Some(e) if e.is_empty() => None,
        Some(e) => Some(e.clone()),
        None => t.effort.clone(),
    };
    let current = t.ran_on.as_ref().map(|r| r.model.clone()).or_else(|| t.model.clone());
    let target = opts.model.clone().or(current);
    match (&t.session, &t.ran_on) {
        // Same provider: the session lives on that account, and its CLI can
        // carry on with a different model.
        (Some(session), Some(on)) if target.as_deref().is_none_or(|m| provider_of(m) == provider_of(&on.model)) => {
            task.profile = Some(on.profile.clone());
            task.model = target.or(Some(on.model.clone()));
            task.account = Some(on.account.clone());
            task.resume = Some(session.clone());
        }
        _ => {
            task.profile = t.ran_on.as_ref().map(|r| r.profile.clone()).or_else(|| t.profile.clone());
            task.model = target;
            task.context = Some(transcript(engine, t));
        }
    }
    task
}

/// Longest transcript passed on (characters); older messages are left out first.
const TRANSCRIPT_MAX: usize = 120_000;
/// Longest single message kept in a transcript.
const MESSAGE_MAX: usize = 16_000;

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max * 2 / 3).collect();
    let tail: Vec<char> = s.chars().rev().take(max / 3).collect();
    format!("{head}\n[… cut …]\n{}", tail.into_iter().rev().collect::<String>())
}

/// The conversation so far, written out for a model that wasn't part of it.
pub fn transcript(engine: &Engine, t: &TaskRec) -> String {
    let mut msgs: Vec<String> = vec![];
    let mut tools: Vec<String> = vec![];
    let flush = |tools: &mut Vec<String>, msgs: &mut Vec<String>| {
        if tools.is_empty() {
            return;
        }
        let shown: Vec<&str> = tools.iter().take(8).map(String::as_str).collect();
        let more = match tools.len().saturating_sub(8) {
            0 => String::new(),
            n => format!("; and {n} more"),
        };
        msgs.push(format!("(Assistant used tools: {}{more})", shown.join("; ")));
        tools.clear();
    };
    for ev in &t.events {
        match ev {
            Event::User { text } => {
                flush(&mut tools, &mut msgs);
                msgs.push(format!("User: {}", clip(text, MESSAGE_MAX)));
            }
            Event::Text { text } => {
                flush(&mut tools, &mut msgs);
                msgs.push(format!("Assistant: {}", clip(text, MESSAGE_MAX)));
            }
            Event::Tool { name, detail, .. } => tools.push(if detail.is_empty() { name.clone() } else { format!("{name} {detail}") }),
            Event::Artifact { path } => tools.push(format!("created {path}")),
            Event::Blocked { tool, detail, .. } => tools.push(format!("{tool} {detail} (blocked: needed approval)")),
            Event::Done { ok: false, summary } => {
                flush(&mut tools, &mut msgs);
                msgs.push(format!("(That turn failed: {})", clip(summary, 500)));
            }
            Event::Repair { prompt, .. } => {
                flush(&mut tools, &mut msgs);
                msgs.push(format!("fermi (the project's checks failed): {}", clip(prompt, MESSAGE_MAX)));
            }
            _ => {}
        }
    }
    flush(&mut tools, &mut msgs);
    // Keep the first message (the task) and as many recent ones as fit.
    let total: usize = msgs.iter().map(|m| m.len() + 2).sum();
    if total > TRANSCRIPT_MAX && msgs.len() > 2 {
        let mut kept = vec![];
        let mut size = msgs[0].len();
        for m in msgs[1..].iter().rev() {
            if size + m.len() + 2 > TRANSCRIPT_MAX {
                break;
            }
            size += m.len() + 2;
            kept.push(m.clone());
        }
        let left_out = msgs.len() - 1 - kept.len();
        kept.push(format!("[… {left_out} earlier messages left out …]"));
        kept.push(msgs[0].clone());
        kept.reverse();
        msgs = kept;
    }
    let model = t
        .ran_on
        .as_ref()
        .map(|r| r.model.as_str())
        .or(t.model.as_deref())
        .and_then(|id| engine.catalog.iter().find(|m| m.id == id))
        .map(|m| m.name().to_string());
    let tool = t.origin.as_deref().and_then(|o| crate::sessions::Source::parse(o.split(':').next()?));
    let from = match (model, tool) {
        (Some(m), Some(src)) => format!("{m} in {}", src.name()),
        (None, Some(src)) => src.name().to_string(),
        (Some(m), None) => m,
        (None, None) => "another assistant".into(),
    };
    format!(
        "{HANDOFF_START} that started with {from}. The conversation so far is below. \
         Work it already did, such as file changes, is already on disk in this folder. \
         Continue from where it left off and answer the user's new message at the end.]\n\n\
         <conversation>\n{}\n</conversation>\n\n{HANDOFF_MARK}",
        msgs.join("\n\n")
    )
}

/// How a passed-on conversation starts, and the line before the new message
/// (so a session that began with one can be recognized later).
pub const HANDOFF_START: &str = "[You're taking over a conversation";
pub const HANDOFF_MARK: &str = "The user's new message:";
/// What a resumed agent is told.
pub const RESUME_PROMPT: &str = "You were interrupted. Continue the task from where you left off.";

// ── the hub ─────────────────────────────────────────────────────────────────

pub struct Hub {
    engine: RwLock<Arc<Engine>>,
    tasks: Mutex<Vec<TaskRec>>,
    /// Tasks this process is running.
    handles: Mutex<HashMap<u64, AbortHandle>>,
    bus: broadcast::Sender<Value>,
    dir: PathBuf,
    via: String,
    scanner: Mutex<Scanner>,
    last_save: Mutex<HashMap<u64, Instant>>,
}

impl Hub {
    /// `via` names this front end in the history ("desktop", "ui").
    pub fn new(engine: Arc<Engine>, via: &str) -> Arc<Self> {
        let dir = history_dir(&engine.home);
        let _ = std::fs::create_dir_all(&dir);
        let tasks = load_history(&dir, HISTORY_LIMIT);
        let hub = Arc::new(Hub {
            engine: RwLock::new(engine),
            tasks: Mutex::new(tasks),
            handles: Mutex::new(HashMap::new()),
            bus: broadcast::channel(4096).0,
            scanner: Mutex::new(Scanner::new(&dir)),
            dir,
            via: via.to_string(),
            last_save: Mutex::new(HashMap::new()),
        });
        hub.reattach();
        // Once a second: heartbeat our running tasks and pick up other programs' changes.
        let weak = Arc::downgrade(&hub);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            loop {
                tick.tick().await;
                let Some(hub) = weak.upgrade() else { break };
                hub.sync();
            }
        });
        hub
    }

    pub fn engine(&self) -> Arc<Engine> {
        self.engine.read().unwrap().clone()
    }

    /// Re-reads the config. Running tasks keep the engine they started with.
    pub fn reload(&self) -> Result<()> {
        *self.engine.write().unwrap() = Engine::load()?;
        self.publish(json!({ "kind": "config" }));
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.bus.subscribe()
    }

    pub fn publish(&self, msg: Value) {
        let _ = self.bus.send(msg);
    }

    pub fn list(&self) -> Vec<Value> {
        self.tasks.lock().unwrap().iter().map(TaskRec::summary).collect()
    }

    pub fn get(&self, id: u64) -> Option<TaskRec> {
        self.tasks.lock().unwrap().iter().find(|t| t.id == id).cloned()
    }

    /// Folders tasks have run in, most recent first (team agents' worktrees aren't workspaces).
    pub fn cwds(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = vec![];
        for t in self.tasks.lock().unwrap().iter().rev().filter(|t| t.team.is_none()) {
            if !out.contains(&t.cwd) {
                out.push(t.cwd.clone());
            }
        }
        out
    }

    pub fn running(&self) -> usize {
        self.tasks.lock().unwrap().iter().filter(|t| t.status == TaskStatus::Running).count()
    }

    fn owns(&self, id: u64) -> bool {
        self.handles.lock().unwrap().contains_key(&id)
    }

    pub fn submit(self: &Arc<Self>, mut task: Task) -> u64 {
        let id = new_task_id(&self.dir);
        let mut rec = TaskRec::new(id, &task, &self.via);
        rec.redact(&self.engine().redactor);
        task.attempt = 1;
        let note = self.with_memory(&mut task);
        self.publish(json!({ "kind": "task_new", "task": rec.summary() }));
        self.tasks.lock().unwrap().push(rec);
        if let Some(line) = note {
            self.record(id, Event::Log { line });
        }
        self.launch(id, task);
        self.save(id);
        id
    }

    /// Puts the project's most relevant memory ahead of a new task's prompt
    /// (records for agents only, framed as information). Returns what was included.
    fn with_memory(&self, task: &mut Task) -> Option<String> {
        let engine = self.engine();
        if engine.cfg.memory.auto_context == Some(false) || task.resume.is_some() {
            return None;
        }
        let mem = crate::memory::Memory::existing(&engine.home, &task.cwd)?;
        let hits = mem.search(crate::memory::Caller::Agent, &task.prompt, engine.cfg.memory.brief_size.unwrap_or(5));
        let brief = mem.brief(&task.prompt, engine.cfg.memory.brief_size.unwrap_or(5))?;
        task.context = Some(match task.context.take() {
            Some(c) => format!("{brief}\n{c}"),
            None => brief,
        });
        let ids: Vec<String> =
            hits.iter().map(|h| format!("{} ({})", h.view.record.id, h.view.record.title.chars().take(60).collect::<String>())).collect();
        Some(format!("project memory given to the agent: {}", ids.join(", ")))
    }

    /// Continues a finished conversation. On the model it ran on (or another
    /// model of the same provider) it resumes the provider session; on a
    /// different provider, or when there's no session to resume (an imported
    /// or failed task), the conversation so far is passed on as context.
    pub fn follow_up(self: &Arc<Self>, id: u64, prompt: &str, opts: FollowUp) -> Result<()> {
        self.continue_task(id, prompt, opts, None)
    }

    /// Resumes an interrupted (or failed) task in its provider session: a new
    /// attempt of the same task, whose events continue the same trace.
    pub fn resume(self: &Arc<Self>, id: u64) -> Result<()> {
        let t = self.get(id).ok_or_else(|| anyhow::anyhow!("no such task"))?;
        if !matches!(t.status, TaskStatus::Cancelled | TaskStatus::Failed) {
            bail!("only an interrupted or failed task can be resumed");
        }
        if t.session.is_none() || t.ran_on.is_none() {
            bail!("this task never reached a provider, so there's no session to resume; send it again instead");
        }
        let checkpoint = t.stamps().last().map_or(0, |s| s.seq);
        let note = format!("resumed as attempt {} after event #{checkpoint}, in the same provider session", t.attempt + 1);
        self.continue_task(id, RESUME_PROMPT, FollowUp::default(), Some(note))
    }

    fn continue_task(self: &Arc<Self>, id: u64, prompt: &str, opts: FollowUp, note: Option<String>) -> Result<()> {
        let engine = self.engine();
        let mut task = {
            let tasks = self.tasks.lock().unwrap();
            let Some(t) = tasks.iter().find(|t| t.id == id) else { bail!("no such task") };
            if t.status == TaskStatus::Running {
                bail!("this task is still running");
            }
            let task = continuation(&engine, t, prompt, &opts);
            // Unknown model, no account for it in the profile…: say so now.
            // Tasks on workers are routed by the worker's own accounts.
            if crate::remote::workers_for(&engine, &task).map_or(true, |w| w.is_empty()) && task.worker.is_none() {
                engine.plan(&task)?;
            }
            task
        };
        {
            let mut tasks = self.tasks.lock().unwrap();
            let Some(t) = tasks.iter_mut().find(|t| t.id == id) else { bail!("no such task") };
            if opts.model.is_some() {
                t.model = task.model.clone();
            }
            t.effort = task.effort.clone();
            t.permission = task.permission.clone();
            t.allowed = task.allowed.clone();
            t.via = Some(self.via.clone());
            t.reopen(t.attempt + 1, &engine.redactor.text(prompt), now_ms());
            task.attempt = t.attempt;
            if let Some(line) = &note {
                t.push_at(Event::Log { line: line.clone() }, now_ms());
            }
        }
        let summary = self.get(id).map(|t| t.summary());
        let user = Event::User { text: engine.redactor.text(prompt).into_owned() };
        self.publish(json!({ "kind": "task_event", "id": id, "event": user, "task": summary }));
        if let Some(line) = note {
            self.publish(json!({ "kind": "task_event", "id": id, "event": Event::Log { line }, "task": summary }));
        }
        self.launch(id, task);
        self.save(id);
        Ok(())
    }

    /// Provider session ids already in the history (run here or imported),
    /// mapped to their task.
    pub fn known_sessions(&self) -> HashMap<String, u64> {
        let mut out = HashMap::new();
        for t in self.tasks.lock().unwrap().iter() {
            if let Some((_, s)) = t.origin.as_deref().and_then(|o| o.split_once(':')) {
                out.insert(s.to_string(), t.id);
            }
            for s in t.session.iter().chain(&t.past_sessions) {
                out.insert(s.clone(), t.id);
            }
        }
        out
    }

    /// Adds a session from another tool to the history. Importing it again
    /// refreshes it, unless it has been continued here since (then the
    /// existing task is returned as is, like a session fermi ran itself).
    pub fn import(&self, found: &crate::sessions::Found) -> Result<u64> {
        let engine = self.engine();
        let origin = found.origin();
        let existing = {
            let tasks = self.tasks.lock().unwrap();
            if let Some(t) = tasks.iter().find(|t| t.origin.is_none() && t.session.as_deref() == Some(&found.session)) {
                return Ok(t.id);
            }
            tasks.iter().find(|t| t.origin.as_deref() == Some(&origin)).cloned()
        };
        let id = match &existing {
            Some(t) if t.status == TaskStatus::Running || t.events.iter().filter(|e| matches!(e, Event::Done { .. })).count() > 1 => {
                return Ok(t.id);
            }
            Some(t) => t.id,
            None => 0,
        };
        let mut rec = crate::sessions::to_rec(&engine, found, id)?;
        rec.redact(&engine.redactor);
        if id == 0 {
            // Keep the history in date order.
            rec.id = id_at(&self.dir, rec.started * 1000);
        }
        let id = rec.id;
        if let Some(mtime) = write_rec(&self.dir, &mut rec) {
            self.scanner.lock().unwrap().note(id, mtime);
        }
        let summary = rec.summary();
        {
            let mut tasks = self.tasks.lock().unwrap();
            match tasks.iter().position(|t| t.id == id) {
                Some(i) => tasks[i] = rec,
                None => {
                    let at = tasks.partition_point(|t| t.id < id);
                    tasks.insert(at, rec);
                }
            }
        }
        self.publish(json!({ "kind": "task_sync", "task": summary }));
        Ok(id)
    }

    fn launch(self: &Arc<Self>, id: u64, mut task: Task) {
        task.id = Some(id); // usage and budget entries name the task
        let (tx, rx) = mpsc::unbounded_channel::<Event>();
        let engine = self.engine();
        crate::trace::save_task(&engine, &task);
        let on_worker = task.worker.is_some() || crate::remote::workers_for(&engine, &task).is_ok_and(|w| !w.is_empty());
        let runner = tokio::spawn(async move {
            if on_worker {
                let _ = crate::remote::run(&engine, &task, &tx).await;
                return;
            }
            match task.verify.clone() {
                Some(v) => crate::verify::run_task(&engine, task, &v, &tx).await,
                None => {
                    let _ = engine.run(&task, &tx).await;
                }
            }
        });
        self.follow_events(id, runner.abort_handle(), rx);
    }

    /// Records a run's events into its task.
    fn follow_events(self: &Arc<Self>, id: u64, abort: AbortHandle, mut rx: mpsc::UnboundedReceiver<Event>) {
        self.handles.lock().unwrap().insert(id, abort);
        let hub = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                hub.record(id, ev);
            }
            hub.handles.lock().unwrap().remove(&id);
            hub.save(id); // final state, even if the last write was throttled
        });
    }

    /// Picks up following tasks that run on workers (a window reopened).
    fn reattach(self: &Arc<Self>) {
        let engine = self.engine();
        let mine: Vec<TaskRec> =
            self.tasks.lock().unwrap().iter().filter(|t| t.status == TaskStatus::Running && t.remote.is_some()).cloned().collect();
        for t in mine {
            let Some(r) = t.remote.clone() else { continue };
            let mut task = Task::new(&t.prompt, t.cwd.clone());
            task.id = Some(t.id);
            task.attempt = t.attempt;
            task.profile = t.profile.clone();
            let (tx, rx) = mpsc::unbounded_channel::<Event>();
            let engine = engine.clone();
            let runner = tokio::spawn(async move {
                if let Err(e) = crate::remote::resume(&engine, &task, &r.worker, &r.job, r.seq, &tx).await {
                    crate::events::emit(&tx, Event::Done { ok: false, summary: format!("{e:#}") });
                }
            });
            self.follow_events(t.id, runner.abort_handle(), rx);
        }
    }

    /// The window is closing: tasks on workers keep running there (the next
    /// window picks them up); everything else is cancelled.
    pub fn shutdown(&self) {
        let ids: Vec<u64> = self.handles.lock().unwrap().keys().copied().collect();
        for id in ids {
            if self.get(id).is_some_and(|t| t.remote.is_some()) {
                crate::remote::detach(id);
                if let Some(h) = self.handles.lock().unwrap().remove(&id) {
                    h.abort();
                }
                self.save(id);
            } else {
                self.cancel(id);
            }
        }
    }

    fn record(&self, id: u64, ev: Event) {
        // Secrets never reach the history, the tapes or the clients.
        let ev = self.engine().redactor.event(&ev);
        let finished = {
            let mut tasks = self.tasks.lock().unwrap();
            let Some(t) = tasks.iter_mut().find(|t| t.id == id) else { return };
            if t.status != TaskStatus::Running {
                return;
            }
            t.apply(&ev)
        };
        let due = self.last_save.lock().unwrap().get(&id).is_none_or(|t| t.elapsed() >= SAVE_EVERY);
        if finished || (due && !matches!(ev, Event::Delta { .. })) {
            self.save(id);
        }
        let summary = self.get(id).map(|t| t.summary());
        self.publish(json!({ "kind": "task_event", "id": id, "event": ev, "task": summary }));
    }

    /// Heartbeats our running tasks, then folds in other programs' changes.
    fn sync(&self) {
        let owned: Vec<u64> = self.handles.lock().unwrap().keys().copied().collect();
        for id in &owned {
            let due = self.last_save.lock().unwrap().get(id).is_none_or(|t| t.elapsed() >= HEARTBEAT);
            if due {
                self.save(*id);
            }
        }
        let (changed, removed) = self.scanner.lock().unwrap().scan(&self.dir);
        for rec in changed {
            if self.owns(rec.id) {
                continue;
            }
            let summary = rec.summary();
            {
                let mut tasks = self.tasks.lock().unwrap();
                match tasks.iter().position(|t| t.id == rec.id) {
                    Some(i) => tasks[i] = rec,
                    None => {
                        let at = tasks.partition_point(|t| t.id < rec.id);
                        tasks.insert(at, rec);
                    }
                }
            }
            self.publish(json!({ "kind": "task_sync", "task": summary }));
        }
        // Tasks other programs were running whose files went quiet: the
        // program closed or crashed, so nobody will ever finish them.
        let at = now();
        let quiet: Vec<u64> =
            self.tasks.lock().unwrap().iter().filter(|t| t.is_stale(at) && !owned.contains(&t.id)).map(|t| t.id).collect();
        for id in quiet {
            if let Some(rec) = settle_if_stale(&self.dir, id) {
                if let Ok(m) = std::fs::metadata(self.dir.join(format!("{id}.json"))).and_then(|m| m.modified()) {
                    self.scanner.lock().unwrap().note(id, m);
                }
                let summary = rec.summary();
                if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.id == id) {
                    *t = rec;
                }
                self.publish(json!({ "kind": "task_sync", "task": summary }));
            } else if let Some(fresh) = read_task(&self.dir, id) {
                // Not stale after all (our copy was old): take the file's version.
                if let Some(t) = self.tasks.lock().unwrap().iter_mut().find(|t| t.id == id) {
                    *t = fresh;
                }
            }
        }
        for id in removed {
            if self.owns(id) {
                continue;
            }
            let mut tasks = self.tasks.lock().unwrap();
            if let Some(i) = tasks.iter().position(|t| t.id == id) {
                tasks.remove(i);
                drop(tasks);
                self.publish(json!({ "kind": "task_deleted", "id": id }));
            }
        }
    }

    /// Stops a task this process runs. Tasks running in another fermi
    /// window can only be stopped there.
    pub fn cancel(&self, id: u64) -> bool {
        let Some(h) = self.handles.lock().unwrap().remove(&id) else { return false };
        h.abort(); // drops the run future and with it the child process
        let summary = {
            let mut tasks = self.tasks.lock().unwrap();
            let Some(t) = tasks.iter_mut().find(|t| t.id == id) else { return false };
            t.cancel();
            t.summary()
        };
        self.save(id);
        self.publish(json!({ "kind": "task_update", "task": summary }));
        true
    }

    pub fn cancel_all(&self) {
        let ids: Vec<u64> = self.handles.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.cancel(id);
        }
    }

    /// Removes a finished task from the list and from disk.
    pub fn delete(&self, id: u64) -> Result<()> {
        {
            let mut tasks = self.tasks.lock().unwrap();
            match tasks.iter().position(|t| t.id == id) {
                Some(i) if tasks[i].status == TaskStatus::Running => bail!("cancel the task before deleting it"),
                Some(i) => {
                    tasks.remove(i);
                }
                None => bail!("no such task"),
            }
        }
        let _ = std::fs::remove_file(self.dir.join(format!("{id}.json")));
        let _ = std::fs::remove_dir_all(crate::trace::tapes_dir(&self.engine().home, id));
        self.publish(json!({ "kind": "task_deleted", "id": id }));
        Ok(())
    }

    fn save(&self, id: u64) {
        let Some(mut t) = self.get(id) else { return };
        // Another window may have resumed this task since (a later attempt):
        // a late save from here must not put the older run back.
        if read_task(&self.dir, id).is_some_and(|disk| disk.attempt > t.attempt) {
            return;
        }
        if let Some(mtime) = write_rec(&self.dir, &mut t) {
            self.scanner.lock().unwrap().note(id, mtime);
        }
        if let Some(cur) = self.tasks.lock().unwrap().iter_mut().find(|x| x.id == id) {
            cur.updated = t.updated;
        }
        self.last_save.lock().unwrap().insert(id, Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fermi-hist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn ids_are_unique_and_ordered() {
        let d = tmp_dir("ids");
        let a = new_task_id(&d);
        std::thread::sleep(Duration::from_millis(2));
        let b = new_task_id(&d);
        assert!(b > a);
        assert!(b < (1u64 << 53), "ids must stay safe integers in JavaScript");
    }

    #[test]
    fn scanner_sees_other_writers_and_stale_tasks() {
        let home = tmp_dir("scan");
        let dir = history_dir(&home);
        std::fs::create_dir_all(&dir).unwrap();
        let mut scanner = Scanner::new(&dir);
        let engine = Engine::new(Default::default(), home.clone(), 0);

        // Another program starts and finishes a task.
        let mut task = Task::new("hello", &home);
        {
            let mut rec = Recorder::start(&engine, &mut task, "cli");
            rec.record(&Event::Text { text: "hi".into() });
            rec.record(&Event::Done { ok: true, summary: "ok".into() });
        }
        let (changed, removed) = scanner.scan(&dir);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].status, TaskStatus::Done);
        assert_eq!(changed[0].via.as_deref(), Some("cli"));
        assert!(removed.is_empty());
        assert!(scanner.scan(&dir).0.is_empty(), "unchanged files aren't reported again");

        // A "running" task nobody has touched for a while comes back interrupted.
        let mut abandoned = TaskRec::new(new_task_id(&dir), &task, "ui");
        abandoned.updated = now() - 60;
        std::fs::write(dir.join(format!("{}.json", abandoned.id)), serde_json::to_vec(&abandoned).unwrap()).unwrap();
        let (changed, _) = scanner.scan(&dir);
        assert_eq!(changed[0].status, TaskStatus::Cancelled);
        assert_eq!(read_task(&dir, abandoned.id).unwrap().status, TaskStatus::Cancelled, "saved back");

        // Deleting a file is reported.
        std::fs::remove_file(dir.join(format!("{}.json", abandoned.id))).unwrap();
        assert_eq!(scanner.scan(&dir).1, vec![abandoned.id]);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn late_save_keeps_a_resumed_attempt() {
        let home = tmp_dir("late");
        let engine = Engine::new(Default::default(), home.clone(), 0);
        let rec = Recorder::start(&engine, &mut Task::new("x", &home), "tui");
        let id = rec.id();
        // Another window resumes the task and finishes the second attempt…
        let dir = history_dir(&home);
        let mut newer = read_task(&dir, id).unwrap();
        newer.reopen(2, "continue", now_ms());
        newer.apply(&Event::Done { ok: true, summary: "done".into() });
        write_rec(&dir, &mut newer);
        // …then this window closes, cancelling and saving its stale copy.
        drop(rec);
        let disk = read_task(&dir, id).unwrap();
        assert_eq!((disk.attempt, disk.status), (2, TaskStatus::Done), "the resumed attempt survives");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn dropped_recorder_marks_cancelled() {
        let home = tmp_dir("drop");
        let id = {
            let engine = Engine::new(Default::default(), home.clone(), 0);
            let rec = Recorder::start(&engine, &mut Task::new("x", &home), "tui");
            rec.id()
        };
        assert_eq!(read_task(&history_dir(&home), id).unwrap().status, TaskStatus::Cancelled);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn switching_models() {
        let home = tmp_dir("switch");
        let cfg = crate::config::Config::parse(
            r#"
            [[accounts]]
            id = "c"
            provider = "claude"
            [[accounts]]
            id = "o"
            provider = "openai"
            [[profiles]]
            name = "p"
            accounts = ["c", "o"]
            "#,
        )
        .unwrap();
        let engine = Engine::new(cfg, home.clone(), 0);
        let mut t = TaskRec::new(1, &Task::new("fix the bug", &home), "ui");
        for ev in [
            Event::Routed {
                profile: "p".into(),
                model: "claude:opus".into(),
                label: "Opus".into(),
                effort: None,
                account: "c".into(),
                why: String::new(),
            },
            Event::Session { id: "s1".into() },
            Event::tool("Bash", "cargo test"),
            Event::Text { text: "Fixed it.".into() },
            Event::Done { ok: true, summary: String::new() },
        ] {
            t.apply(&ev);
        }
        let opts = |model: Option<&str>| FollowUp { model: model.map(String::from), ..Default::default() };

        // Same model, or another Claude model: the session carries on.
        let same = continuation(&engine, &t, "and the docs", &opts(None));
        assert_eq!((same.resume.as_deref(), same.model.as_deref(), same.account.as_deref()), (Some("s1"), Some("claude:opus"), Some("c")));
        let sonnet = continuation(&engine, &t, "and the docs", &opts(Some("claude:sonnet")));
        assert_eq!((sonnet.resume.as_deref(), sonnet.model.as_deref()), (Some("s1"), Some("claude:sonnet")));
        assert!(sonnet.context.is_none());

        // Another provider: a fresh session that gets the conversation.
        let gpt = continuation(&engine, &t, "and the docs", &opts(Some("openai:gpt")));
        assert!(gpt.resume.is_none() && gpt.account.is_none());
        assert_eq!((gpt.model.as_deref(), gpt.profile.as_deref()), (Some("openai:gpt"), Some("p")));
        let ctx = gpt.context.as_deref().unwrap();
        assert!(ctx.contains("User: fix the bug") && ctx.contains("Bash cargo test") && ctx.contains("Assistant: Fixed it."));
        assert!(gpt.input().ends_with("The user's new message:\n\nand the docs"));
        assert_eq!(gpt.prompt, "and the docs", "routing sees only the new message");
        engine.plan(&gpt).unwrap();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn long_transcripts_keep_the_start_and_the_end() {
        let home = tmp_dir("transcript");
        let cfg = crate::config::Config::parse("").unwrap();
        let engine = Engine::new(cfg, home.clone(), 0);
        let mut t = TaskRec::new(1, &Task::new("the original task", &home), "ui");
        for i in 0..400 {
            t.events.push(Event::Text { text: format!("answer {i} {}", "x".repeat(1000)) });
        }
        let s = transcript(&engine, &t);
        assert!(s.len() < TRANSCRIPT_MAX + 2_000);
        assert!(s.contains("User: the original task") && s.contains("answer 399") && !s.contains("answer 0 "));
        assert!(s.contains("earlier messages left out"));
        let _ = std::fs::remove_dir_all(&home);
    }
}
