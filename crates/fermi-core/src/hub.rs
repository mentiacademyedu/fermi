//! Task registry for long-lived front ends (the dashboard and desktop app).
//! Runs tasks on the engine, keeps every task's events, broadcasts updates
//! to any number of clients, continues conversations, and keeps a history on
//! disk (`~/.fermi/history/<id>.json`) so tasks survive restarts.

use crate::engine::{Engine, Task};
use crate::events::Event;
use crate::util::now;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{broadcast, mpsc};
use tokio::task::AbortHandle;

/// How many past tasks are loaded at startup (older ones stay on disk).
const HISTORY_LIMIT: usize = 500;

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
    pub events: Vec<Event>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RanOn {
    pub profile: String,
    pub model: String,
    pub account: String,
}

impl TaskRec {
    /// Everything but the event log.
    pub fn summary(&self) -> Value {
        json!({
            "id": self.id, "prompt": self.prompt, "profile": self.profile, "model": self.model,
            "account": self.account, "effort": self.effort, "permission": self.permission, "cwd": self.cwd, "status": self.status,
            "route": self.route, "started": self.started, "ended": self.ended,
            "resumable": self.session.is_some() && self.ran_on.is_some(),
            "turns": self.events.iter().filter(|e| matches!(e, Event::User { .. })).count(),
        })
    }
}

pub struct Hub {
    engine: RwLock<Arc<Engine>>,
    tasks: Mutex<Vec<TaskRec>>,
    handles: Mutex<HashMap<u64, AbortHandle>>,
    bus: broadcast::Sender<Value>,
    next: AtomicU64,
    dir: PathBuf,
}

impl Hub {
    pub fn new(engine: Arc<Engine>) -> Arc<Self> {
        let dir = engine.home.join("history");
        let _ = std::fs::create_dir_all(&dir);
        let tasks = load_history(&dir);
        let next = tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        Arc::new(Hub {
            engine: RwLock::new(engine),
            tasks: Mutex::new(tasks),
            handles: Mutex::new(HashMap::new()),
            bus: broadcast::channel(4096).0,
            next: AtomicU64::new(next),
            dir,
        })
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

    /// Folders tasks have run in, most recent first.
    pub fn cwds(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = vec![];
        for t in self.tasks.lock().unwrap().iter().rev() {
            if !out.contains(&t.cwd) {
                out.push(t.cwd.clone());
            }
        }
        out
    }

    pub fn running(&self) -> usize {
        self.tasks.lock().unwrap().iter().filter(|t| t.status == TaskStatus::Running).count()
    }

    pub fn submit(self: &Arc<Self>, task: Task) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let rec = TaskRec {
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
            events: vec![Event::User { text: task.prompt.clone() }],
        };
        self.publish(json!({ "kind": "task_new", "task": rec.summary() }));
        self.tasks.lock().unwrap().push(rec);
        self.save(id);
        self.launch(id, task);
        id
    }

    /// Continues a finished conversation in the same provider session, on the
    /// same account and model it ran on.
    pub fn follow_up(self: &Arc<Self>, id: u64, prompt: &str) -> Result<()> {
        let task = {
            let mut tasks = self.tasks.lock().unwrap();
            let Some(t) = tasks.iter_mut().find(|t| t.id == id) else { bail!("no such task") };
            if t.status == TaskStatus::Running {
                bail!("this task is still running");
            }
            let (Some(session), Some(on)) = (t.session.clone(), t.ran_on.clone()) else {
                bail!("this task can't be continued (it has no provider session)");
            };
            let mut task = Task::new(prompt, t.cwd.clone());
            task.profile = Some(on.profile);
            task.model = Some(on.model);
            task.account = Some(on.account);
            task.effort = t.effort.clone();
            task.permission = t.permission.clone();
            task.resume = Some(session);
            t.status = TaskStatus::Running;
            t.ended = None;
            t.events.push(Event::User { text: prompt.to_string() });
            task
        };
        let summary = self.get(id).map(|t| t.summary());
        self.publish(json!({ "kind": "task_event", "id": id, "event": Event::User { text: prompt.to_string() }, "task": summary }));
        self.save(id);
        self.launch(id, task);
        Ok(())
    }

    fn launch(self: &Arc<Self>, id: u64, task: Task) {
        let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
        let engine = self.engine();
        let runner = tokio::spawn(async move {
            let _ = engine.run(&task, &tx).await;
        });
        self.handles.lock().unwrap().insert(id, runner.abort_handle());

        let hub = self.clone();
        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                hub.record(id, ev);
            }
            hub.handles.lock().unwrap().remove(&id);
        });
    }

    fn record(&self, id: u64, ev: Event) {
        let finished = {
            let mut tasks = self.tasks.lock().unwrap();
            let Some(t) = tasks.iter_mut().find(|t| t.id == id) else { return };
            if t.status != TaskStatus::Running {
                return; // cancelled: drop late events
            }
            let mut finished = false;
            match &ev {
                Event::Routed { profile, model, label, effort, account, .. } => {
                    let effort = effort.as_deref().map(|e| format!(" · {e}")).unwrap_or_default();
                    t.route = Some(format!("{label}{effort} @ {account}"));
                    t.ran_on = Some(RanOn { profile: profile.clone(), model: model.clone(), account: account.clone() });
                }
                Event::Session { id } => t.session = Some(id.clone()),
                Event::Done { ok, .. } => {
                    t.status = if *ok { TaskStatus::Done } else { TaskStatus::Failed };
                    t.ended = Some(now());
                    finished = true;
                }
                _ => {}
            }
            // Session ids are bookkeeping, and streamed chunks are superseded
            // by the finished `Text`: neither belongs in the saved history.
            if !matches!(ev, Event::Session { .. } | Event::Delta { .. }) {
                t.events.push(ev.clone());
            }
            finished
        };
        if finished {
            self.save(id);
        }
        let summary = self.get(id).map(|t| t.summary());
        self.publish(json!({ "kind": "task_event", "id": id, "event": ev, "task": summary }));
    }

    pub fn cancel(&self, id: u64) -> bool {
        let Some(h) = self.handles.lock().unwrap().remove(&id) else { return false };
        h.abort(); // drops the run future and with it the child process
        let summary = {
            let mut tasks = self.tasks.lock().unwrap();
            let Some(t) = tasks.iter_mut().find(|t| t.id == id) else { return false };
            t.status = TaskStatus::Cancelled;
            t.ended = Some(now());
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
        self.publish(json!({ "kind": "task_deleted", "id": id }));
        Ok(())
    }

    fn save(&self, id: u64) {
        let Some(t) = self.get(id) else { return };
        let Ok(bytes) = serde_json::to_vec(&t) else { return };
        let path = self.dir.join(format!("{id}.json"));
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// The newest tasks on disk. Ones that were running when the app closed
/// come back as cancelled.
fn load_history(dir: &std::path::Path) -> Vec<TaskRec> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut tasks: Vec<TaskRec> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice::<TaskRec>(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    tasks.sort_by_key(|t| t.id);
    if tasks.len() > HISTORY_LIMIT {
        tasks.drain(..tasks.len() - HISTORY_LIMIT);
    }
    for t in tasks.iter_mut().filter(|t| t.status == TaskStatus::Running) {
        t.status = TaskStatus::Cancelled;
        t.ended.get_or_insert(t.started);
        t.events.push(Event::Log { line: "interrupted: fermi was closed while this ran".into() });
    }
    tasks
}
