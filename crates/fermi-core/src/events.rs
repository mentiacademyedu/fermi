use serde::{Deserialize, Serialize};

/// Everything a front end needs to render a task. Serializable so the
/// dashboard can stream it as JSON unchanged and history can be saved.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A user turn (the first prompt or a follow-up).
    User { text: String },
    Routed { profile: String, model: String, label: String, effort: Option<String>, account: String, why: String },
    /// The provider session this run belongs to; follow-ups resume it.
    Session { id: String },
    Text { text: String },
    Tool { name: String, detail: String },
    Limit { account: String, utilization: Option<f64>, resets_at: Option<u64>, blocked: bool },
    Failover { account: String, reason: String },
    Artifact { path: String },
    Log { line: String },
    Done { ok: bool, summary: String },
}

pub type Tx = tokio::sync::mpsc::UnboundedSender<Event>;

pub fn emit(tx: &Tx, ev: Event) {
    let _ = tx.send(ev);
}
