//! Per-account usage state, shared between processes through a small JSON
//! file (the MCP bridge runs as a separate process), plus an append-only
//! usage log for history/dashboards.

use crate::util::now;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcctState {
    /// Share of the current rate-limit window used (0..1), when the CLI reports it.
    #[serde(default)]
    pub utilization: Option<f64>,
    #[serde(default)]
    pub resets_at: Option<u64>,
    /// Don't route here before this time (hit a limit).
    #[serde(default)]
    pub limited_until: Option<u64>,
    #[serde(default)]
    pub last_used: u64,
    #[serde(default)]
    pub tasks: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

impl AcctState {
    pub fn limited(&self, now: u64) -> bool {
        self.limited_until.is_some_and(|t| t > now)
    }
}

/// One call, as appended to `usage.jsonl`.
#[derive(Debug, Serialize)]
pub struct UsageRecord<'a> {
    pub ts: u64,
    pub profile: &'a str,
    pub account: &'a str,
    pub model: &'a str,
    /// ok | limited | failed (failed attempts are retried on another account or model).
    pub status: &'a str,
    /// Every input token, including cached ones. A count the provider didn't
    /// report is null, not zero.
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// Cost the provider reported.
    pub cost_usd: Option<f64>,
    /// Cost from the price table in effect, with its version and breakdown.
    pub cost: Option<&'a crate::billing::Cost>,
    pub estimate_micros: Option<i64>,
    /// Which try within the task's run (1 = first account and model).
    pub attempt: u32,
    #[serde(flatten)]
    pub who: &'a crate::billing::Attribution,
    pub secs: f64,
}

pub struct Store {
    path: PathBuf,
    log: PathBuf,
    lock: Mutex<()>,
    /// In-process running task count per account (used by rotate).
    running: Mutex<HashMap<String, u32>>,
}

/// Default cooldown when a provider reports a limit without a reset time.
const DEFAULT_COOLDOWN: u64 = 15 * 60;

impl Store {
    pub fn open(home: &Path) -> Self {
        let _ = std::fs::create_dir_all(home);
        Store { path: home.join("state.json"), log: home.join("usage.jsonl"), lock: Mutex::new(()), running: Mutex::new(HashMap::new()) }
    }

    pub fn snapshot(&self) -> HashMap<String, AcctState> {
        std::fs::read(&self.path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn get(&self, id: &str) -> AcctState {
        self.snapshot().remove(id).unwrap_or_default()
    }

    /// Read-modify-write, re-reading first so other processes' writes survive.
    pub fn update(&self, id: &str, f: impl FnOnce(&mut AcctState)) {
        let _g = self.lock.lock().unwrap();
        let mut all = self.snapshot();
        f(all.entry(id.to_string()).or_default());
        if let Ok(bytes) = serde_json::to_vec_pretty(&all) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, &self.path);
            }
        }
    }

    pub fn observe_window(&self, id: &str, utilization: Option<f64>, resets_at: Option<u64>) {
        self.update(id, |s| {
            if utilization.is_some() {
                s.utilization = utilization;
            }
            if resets_at.is_some() {
                s.resets_at = resets_at;
            }
        });
    }

    pub fn mark_limited(&self, id: &str, until: Option<u64>) {
        let until = until.filter(|t| *t > now()).unwrap_or(now() + DEFAULT_COOLDOWN);
        self.update(id, |s| s.limited_until = Some(until));
    }

    pub fn running(&self, id: &str) -> u32 {
        self.running.lock().unwrap().get(id).copied().unwrap_or(0)
    }

    pub fn begin(&self, id: &str) {
        *self.running.lock().unwrap().entry(id.to_string()).or_default() += 1;
    }

    pub fn end(&self, id: &str) {
        if let Some(n) = self.running.lock().unwrap().get_mut(id) {
            *n = n.saturating_sub(1);
        }
    }

    pub fn record(&self, rec: &UsageRecord) {
        self.update(rec.account, |s| {
            s.last_used = rec.ts;
            s.tasks += 1;
            s.input_tokens += rec.input_tokens.unwrap_or(0);
            s.output_tokens += rec.output_tokens.unwrap_or(0);
            if rec.status == "ok" {
                s.limited_until = None;
            }
        });
        if let Ok(line) = serde_json::to_string(rec) {
            let _g = self.lock.lock().unwrap();
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.log) {
                let _ = writeln!(f, "{line}");
            }
        }
    }
}
