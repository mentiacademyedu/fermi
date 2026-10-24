//! Approvals for actions the policy holds (`[security] confirm_commands`).
//!
//! Every request, decision and use is a line in `~/.fermi/approvals/log.jsonl`
//! with who decided, when, for which task, tool and exact resource (the
//! command). An approval covers that exact command once in that task: the
//! first use consumes it, so a replay or a broader command is held again.
//! A denial stands until the user approves the same command.

use crate::util::now;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    /// An agent asked; the policy held it.
    Request { id: String, ts: u64, task: Option<u64>, tool: String, resource: String, reason: String },
    /// A person decided.
    Decision { id: String, ts: u64, task: Option<u64>, tool: String, resource: String, allow: bool, actor: String },
    /// An approval was used (and is spent).
    Use { id: String, ts: u64, task: Option<u64>, tool: String, resource: String, approval: String },
}

/// What the ledger says about a held action.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// An unspent approval for exactly this; using it spends it.
    Approved(String),
    /// The user denied exactly this.
    Denied { actor: String, ts: u64 },
    /// Nobody decided yet (or the approval was spent).
    Pending,
}

pub struct Approvals {
    dir: PathBuf,
}

impl Approvals {
    pub fn open(home: &Path) -> Approvals {
        Approvals { dir: home.join("approvals") }
    }

    fn log(&self) -> PathBuf {
        self.dir.join("log.jsonl")
    }

    pub fn entries(&self) -> Vec<Entry> {
        std::fs::read_to_string(self.log()).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// Runs `f` holding the ledger across fermi processes.
    fn locked<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        std::fs::create_dir_all(&self.dir)?;
        let lock = self.dir.join("log.lock");
        let start = Instant::now();
        while std::fs::OpenOptions::new().write(true).create_new(true).open(&lock).is_err() {
            if start.elapsed() > Duration::from_secs(5) {
                let _ = std::fs::remove_file(&lock);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let out = f(self);
        let _ = std::fs::remove_file(&lock);
        out
    }

    fn append(&self, e: &Entry) -> Result<()> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.log())?;
        f.write_all(format!("{}\n", serde_json::to_string(e)?).as_bytes())?;
        Ok(())
    }

    fn verdict_in(entries: &[Entry], task: Option<u64>, tool: &str, resource: &str) -> Verdict {
        let mut state = Verdict::Pending;
        for e in entries {
            match e {
                Entry::Decision { id, ts, task: t, tool: tl, resource: r, allow, actor } if *t == task && tl == tool && r == resource => {
                    state = if *allow { Verdict::Approved(id.clone()) } else { Verdict::Denied { actor: actor.clone(), ts: *ts } };
                }
                Entry::Use { approval, .. } if state == Verdict::Approved(approval.clone()) => state = Verdict::Pending,
                _ => {}
            }
        }
        state
    }

    /// Decides a held action: uses (spends) an approval, honors a denial, or
    /// records a new request. Atomic across processes.
    pub fn check(&self, task: Option<u64>, tool: &str, resource: &str, reason: &str) -> Result<Verdict> {
        self.locked(|me| {
            let v = Self::verdict_in(&me.entries(), task, tool, resource);
            match &v {
                Verdict::Approved(approval) => me.append(&Entry::Use {
                    id: new_id("use"),
                    ts: now(),
                    task,
                    tool: tool.into(),
                    resource: resource.into(),
                    approval: approval.clone(),
                })?,
                Verdict::Pending => me.append(&Entry::Request {
                    id: new_id("req"),
                    ts: now(),
                    task,
                    tool: tool.into(),
                    resource: resource.into(),
                    reason: reason.into(),
                })?,
                Verdict::Denied { .. } => {}
            }
            Ok(v)
        })
    }

    /// Records a person's decision on exactly this action in this task.
    pub fn decide(&self, task: Option<u64>, tool: &str, resource: &str, allow: bool, actor: &str) -> Result<String> {
        let id = new_id(if allow { "ok" } else { "no" });
        self.locked(|me| {
            me.append(&Entry::Decision {
                id: id.clone(),
                ts: now(),
                task,
                tool: tool.into(),
                resource: resource.into(),
                allow,
                actor: actor.into(),
            })
        })?;
        Ok(id)
    }

    /// Requests nobody has decided on yet, for a task (or all tasks).
    pub fn pending(&self, task: Option<u64>) -> Vec<Entry> {
        let entries = self.entries();
        let mut out: Vec<Entry> = vec![];
        for e in &entries {
            if let Entry::Request { task: t, tool, resource, .. } = e {
                if (task.is_none() || *t == task)
                    && !entries.iter().any(|d| matches!(d, Entry::Decision { task: dt, tool: dtl, resource: dr, .. } if dt == t && dtl == tool && dr == resource))
                    && !out.iter().any(|o| matches!(o, Entry::Request { task: ot, tool: otl, resource: or, .. } if ot == t && otl == tool && or == resource))
                {
                    out.push(e.clone());
                }
            }
        }
        out
    }
}

fn new_id(prefix: &str) -> String {
    format!("{prefix}-{}", &crate::util::uuid_v4().replace('-', "")[..12])
}

/// Who is deciding: the signed-in user of this computer.
pub fn actor() -> String {
    std::env::var("USERNAME").or_else(|_| std::env::var("USER")).map(|u| format!("user:{u}")).unwrap_or_else(|_| "user".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approvals_are_exact_and_single_use() {
        let dir = std::env::temp_dir().join(format!("fermi-approvals-{}", crate::util::uuid_v4()));
        let a = Approvals::open(&dir);
        let (t, cmd) = (Some(7), "rm scratch/a.txt");
        assert_eq!(a.check(t, "Bash", cmd, "destructive").unwrap(), Verdict::Pending);
        a.decide(t, "Bash", cmd, false, "user:x").unwrap();
        assert!(matches!(a.check(t, "Bash", cmd, "destructive").unwrap(), Verdict::Denied { .. }));
        a.decide(t, "Bash", cmd, true, "user:x").unwrap();
        assert!(matches!(a.check(t, "Bash", cmd, "destructive").unwrap(), Verdict::Approved(_)));
        assert_eq!(a.check(t, "Bash", cmd, "destructive").unwrap(), Verdict::Pending, "spent");
        assert_eq!(a.check(t, "Bash", "rm scratch/*", "destructive").unwrap(), Verdict::Pending, "broader");
        assert_eq!(a.check(Some(8), "Bash", cmd, "destructive").unwrap(), Verdict::Pending, "another task");
        let _ = std::fs::remove_dir_all(dir);
    }
}
