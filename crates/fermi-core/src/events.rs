use serde::{Deserialize, Serialize};

/// Everything a front end needs to render a task. Serializable so the
/// dashboard can stream it as JSON unchanged and history can be saved.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A user turn (the first prompt or a follow-up).
    User {
        text: String,
    },
    Routed {
        profile: String,
        model: String,
        label: String,
        effort: Option<String>,
        account: String,
        why: String,
    },
    /// The provider session this run belongs to; follow-ups resume it.
    Session {
        id: String,
    },
    /// A chunk of assistant text as it's written. The finished message
    /// follows as `Text`, which replaces the streamed chunks.
    Delta {
        text: String,
    },
    Text {
        text: String,
    },
    /// A tool invocation starts. `id` is the provider's (or fermi's) id for
    /// it, `call` the model call that requested it, `input` its arguments.
    Tool {
        name: String,
        detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<serde_json::Value>,
    },
    /// How a tool invocation ended. `status`: ok | error | declined |
    /// no_result (the run ended before the tool reported back; never read as success).
    ToolResult {
        id: String,
        status: String,
        /// The output, redacted and shortened.
        #[serde(default)]
        output: String,
        /// The error code, when the tool or the CLI gives one (`exit 2`, `FILE_NOT_FOUND`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// Milliseconds from the call to its result, measured by fermi (monotonic clock).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
    },
    /// One model call finished, with the usage the provider reported for it,
    /// field for field (fields it didn't report are absent, not zero).
    Call {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default)]
        usage: serde_json::Value,
    },
    Limit {
        account: String,
        utilization: Option<f64>,
        resets_at: Option<u64>,
        blocked: bool,
    },
    Failover {
        account: String,
        reason: String,
    },
    /// A tool call the agent wasn't allowed to make without approval (the
    /// CLI runs headless, so nobody could approve it). `rules` would allow it
    /// on a follow-up, e.g. `Bash(node:*)`.
    Blocked {
        tool: String,
        detail: String,
        rules: Vec<String>,
    },
    Artifact {
        path: String,
    },
    Log {
        line: String,
    },
    /// What a call is expected to use and cost, before it's sent (`billing::Estimate`).
    Estimate {
        estimate: serde_json::Value,
    },
    /// A budget after a reservation, a settlement or a refusal (`billing::BudgetState`).
    Budget {
        /// reserved | settled | denied
        action: String,
        state: serde_json::Value,
        #[serde(default)]
        note: String,
    },
    /// How the model was chosen under an economy or quality policy (`router::Decision`).
    Decision {
        decision: serde_json::Value,
    },
    /// fermi ran the project's checks after the agent's turn (`verify::Report::compact`).
    Verify {
        attempt: u32,
        passed: bool,
        report: serde_json::Value,
    },
    /// The checks failed: fermi sends the failures back to the agent.
    Repair {
        attempt: u32,
        max: u32,
        prompt: String,
    },
    Done {
        ok: bool,
        summary: String,
    },
    /// The task runs on a worker as `job`; `seq` is the last of the job's
    /// events received here (0 when it was dispatched).
    Remote {
        worker: String,
        job: String,
        seq: u64,
    },
}

impl Event {
    /// A tool line without invocation detail (imported sessions, simple adapters).
    pub fn tool(name: impl Into<String>, detail: impl Into<String>) -> Event {
        Event::Tool { name: name.into(), detail: detail.into(), id: None, call: None, input: None }
    }
}

pub type Tx = tokio::sync::mpsc::UnboundedSender<Event>;

pub fn emit(tx: &Tx, ev: Event) {
    let _ = tx.send(ev);
}
