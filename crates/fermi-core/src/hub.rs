//! Task registry for long-lived front ends (the dashboard server). Runs tasks
//! on the engine, keeps every task's events, and broadcasts updates so any
//! number of clients can follow along.

use crate::engine::{Engine, Task};
use crate::events::Event;
use crate::util::now;
use anyhow::Result;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::{broadcast, mpsc};
use tokio::task::AbortHandle;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskRec {
    pub id: u64,
    pub prompt: String,
    pub profile: Option<String>,
    pub model: Option<String>,
    pub account: Option<String>,
    pub cwd: PathBuf,
    pub status: TaskStatus,
    /// `model @ account` once routed.
    pub route: Option<String>,
    pub started: u64,
    pub ended: Option<u64>,
    pub events: Vec<Event>,
}

impl TaskRec {
    /// Everything but the event log.
    pub fn summary(&self) -> Value {
        json!({
            "id": self.id, "prompt": self.prompt, "profile": self.profile, "model": self.model,
            "account": self.account, "cwd": self.cwd, "status": self.status, "route": self.route,
            "started": self.started, "ended": self.ended,
        })
    }
}

pub struct Hub {
    engine: RwLock<Arc<Engine>>,
    tasks: Mutex<Vec<TaskRec>>,
    handles: Mutex<HashMap<u64, AbortHandle>>,
    bus: broadcast::Sender<Value>,
    next: AtomicU64,
}

impl Hub {
    pub fn new(engine: Arc<Engine>) -> Arc<Self> {
        Arc::new(Hub {
            engine: RwLock::new(engine),
            tasks: Mutex::new(vec![]),
            handles: Mutex::new(HashMap::new()),
            bus: broadcast::channel(4096).0,
            next: AtomicU64::new(1),
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
            cwd: task.cwd.clone(),
            status: TaskStatus::Running,
            route: None,
            started: now(),
            ended: None,
            events: vec![],
        };
        self.publish(json!({ "kind": "task_new", "task": rec.summary() }));
        self.tasks.lock().unwrap().push(rec);

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
        id
    }

    fn record(&self, id: u64, ev: Event) {
        {
            let mut tasks = self.tasks.lock().unwrap();
            let Some(t) = tasks.iter_mut().find(|t| t.id == id) else { return };
            if t.status != TaskStatus::Running {
                return; // cancelled: drop late events
            }
            match &ev {
                Event::Routed { model, account, .. } => t.route = Some(format!("{model} @ {account}")),
                Event::Done { ok, .. } => {
                    t.status = if *ok { TaskStatus::Done } else { TaskStatus::Failed };
                    t.ended = Some(now());
                }
                _ => {}
            }
            t.events.push(ev.clone());
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
        self.publish(json!({ "kind": "task_update", "task": summary }));
        true
    }

    pub fn cancel_all(&self) {
        let ids: Vec<u64> = self.handles.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.cancel(id);
        }
    }
}
