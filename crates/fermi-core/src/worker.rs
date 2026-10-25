//! A worker: an execution service that runs tasks for other fermi instances
//! (`fermi worker`), here, in a container or on another machine. Jobs keep
//! running whoever is watching; their state, numbered events and artifacts
//! live on the worker's disk, so a client can reconnect and pick up where it
//! left off, and a worker that restarts marks the jobs it lost as interrupted.
//!
//! The HTTP layer is in the `fermi` crate; the protocol is versioned
//! ([`PROTOCOL`]) and every request carries the client's version and the
//! worker's token.

use crate::engine::{Engine, Task};
use crate::events::Event;
use crate::util::{now, now_ms};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The worker protocol this fermi speaks, and the versions it accepts from the other side.
pub const PROTOCOL: u32 = 1;
pub const SUPPORTED: (u32, u32) = (1, 1);
pub const HEADER: &str = "x-fermi-protocol";

/// The version this process speaks (`FERMI_WORKER_PROTOCOL` overrides it, to test handshakes).
pub fn protocol() -> u32 {
    std::env::var("FERMI_WORKER_PROTOCOL").ok().and_then(|v| v.parse().ok()).unwrap_or(PROTOCOL)
}

pub fn compatible(v: u32) -> bool {
    (SUPPORTED.0..=SUPPORTED.1).contains(&v)
}

fn range() -> String {
    if SUPPORTED.0 == SUPPORTED.1 {
        SUPPORTED.0.to_string()
    } else {
        format!("{} to {}", SUPPORTED.0, SUPPORTED.1)
    }
}

/// What a client asks a worker to run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JobSpec {
    /// Chosen by the client and stable across resends: submitting it again
    /// returns the same job instead of starting another.
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub verify: Option<crate::verify::VerifyOpts>,
    /// A follow-up: the provider session to continue (it lives on this
    /// worker's account), the account it's on, and the conversation so far.
    #[serde(default)]
    pub resume: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub context: Option<String>,
    /// The client's task, for the record.
    #[serde(default)]
    pub task: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobState {
    pub id: String,
    /// running | done | failed | cancelled | interrupted
    pub status: String,
    /// The last event's number.
    pub seq: u64,
    pub created: u64,
    /// Rewritten every second while running.
    pub heartbeat: u64,
    pub ended: Option<u64>,
    pub summary: String,
    pub worker: String,
    #[serde(default)]
    pub cancel: Option<Value>,
}

impl JobState {
    pub fn terminal(&self) -> bool {
        self.status != "running"
    }
}

struct Running {
    abort: tokio::task::AbortHandle,
}

/// The jobs of one worker.
pub struct Service {
    engine: Arc<Engine>,
    dir: PathBuf,
    pub name: String,
    token: String,
    running: Mutex<HashMap<String, Running>>,
    lock: Mutex<()>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

impl Service {
    /// Opens the job store; jobs it finds still "running" were lost with an
    /// earlier process and are marked interrupted.
    pub fn open(engine: Arc<Engine>, dir: &Path, name: &str, token: &str) -> Result<Arc<Service>> {
        std::fs::create_dir_all(dir.join("jobs"))?;
        let s = Arc::new(Service {
            engine,
            dir: dir.to_path_buf(),
            name: name.into(),
            token: token.into(),
            running: Mutex::new(HashMap::new()),
            lock: Mutex::new(()),
        });
        for e in std::fs::read_dir(dir.join("jobs"))?.flatten() {
            let id = e.file_name().to_string_lossy().to_string();
            if let Ok(mut st) = s.state(&id) {
                if st.status == "running" {
                    st.status = "interrupted".into();
                    st.ended = Some(now());
                    st.summary = "the worker stopped while this job ran".into();
                    s.save(&st)?;
                    s.append(&id, &Event::Log { line: "interrupted: the worker stopped while this job ran".into() })?;
                }
            }
        }
        Ok(s)
    }

    fn job_dir(&self, id: &str) -> PathBuf {
        self.dir.join("jobs").join(id)
    }

    /// Who this worker is and what it speaks.
    pub fn hello(&self) -> Value {
        json!({ "worker": self.name, "protocol": protocol(), "accepts": [SUPPORTED.0, SUPPORTED.1],
            "runtime": { "fermi": env!("CARGO_PKG_VERSION"), "os": std::env::consts::OS, "arch": std::env::consts::ARCH } })
    }

    /// Checks a request's token and protocol version: Err((status, body)) refuses it.
    pub fn admit(&self, token: Option<&str>, version: Option<&str>) -> std::result::Result<(), (u16, Value)> {
        let given = token.unwrap_or("").trim_start_matches("Bearer ").trim();
        if self.token.is_empty()
            || given.len() != self.token.len()
            || !given.bytes().zip(self.token.bytes()).fold(true, |ok, (a, b)| ok & (a == b))
        {
            return Err((401, json!({ "error": "this worker needs its token" })));
        }
        let v: Option<u32> = version.and_then(|v| v.trim().parse().ok());
        match v {
            Some(v) if compatible(v) => Ok(()),
            _ => Err((
                426,
                json!({ "error": format!("this worker speaks protocol {} and accepts clients of protocol {}; the request said {}",
                    protocol(), range(), version.unwrap_or("nothing")),
                    "worker_protocol": protocol(), "accepts": [SUPPORTED.0, SUPPORTED.1] }),
            )),
        }
    }

    pub fn state(&self, id: &str) -> Result<JobState> {
        if !valid_id(id) {
            bail!("bad job id");
        }
        serde_json::from_slice(&std::fs::read(self.job_dir(id).join("job.json")).map_err(|_| anyhow!("no job {id}"))?).context("job state")
    }

    fn save(&self, st: &JobState) -> Result<()> {
        let d = self.job_dir(&st.id);
        let tmp = d.join(format!("job.{}.tmp", std::process::id()));
        std::fs::write(&tmp, serde_json::to_vec_pretty(st)?)?;
        std::fs::rename(&tmp, d.join("job.json"))?;
        Ok(())
    }

    /// Appends an event with the next number.
    fn append(&self, id: &str, ev: &Event) -> Result<u64> {
        let _g = self.lock.lock().unwrap();
        let mut st = self.state(id)?;
        st.seq += 1;
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(self.job_dir(id).join("events.jsonl"))?;
        writeln!(f, "{}", json!({ "seq": st.seq, "at": now_ms(), "event": ev }))?;
        st.heartbeat = now();
        self.save(&st)?;
        Ok(st.seq)
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut JobState)) -> Result<JobState> {
        let _g = self.lock.lock().unwrap();
        let mut st = self.state(id)?;
        f(&mut st);
        self.save(&st)?;
        Ok(st)
    }

    /// Events after `after`, with the job's state.
    pub fn events(&self, id: &str, after: u64) -> Result<(Vec<Value>, JobState)> {
        let st = self.state(id)?;
        let text = std::fs::read_to_string(self.job_dir(id).join("events.jsonl")).unwrap_or_default();
        let evs =
            text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["seq"].as_u64().unwrap_or(0) > after).collect();
        Ok((evs, st))
    }

    /// Starts a job (or returns it, if this id was submitted before).
    pub fn submit(self: &Arc<Self>, spec: JobSpec, bundle: &[u8]) -> Result<(JobState, bool)> {
        if !valid_id(&spec.id) {
            bail!("a job id is letters, digits, - and _ (at most 80)");
        }
        {
            let _g = self.lock.lock().unwrap();
            if let Ok(st) = self.state(&spec.id) {
                return Ok((st, false));
            }
            let d = self.job_dir(&spec.id);
            std::fs::create_dir_all(d.join("artifacts"))?;
            std::fs::write(d.join("spec.json"), serde_json::to_vec_pretty(&spec)?)?;
            let st = JobState {
                id: spec.id.clone(),
                status: "running".into(),
                seq: 0,
                created: now(),
                heartbeat: now(),
                ended: None,
                summary: String::new(),
                worker: self.name.clone(),
                cancel: None,
            };
            self.save(&st)?;
        }
        let work = self.job_dir(&spec.id).join("work");
        unbundle(bundle, &work)?;
        crate::eval::git_baseline(&work)?;
        let me = self.clone();
        let id = spec.id.clone();
        let handle = tokio::spawn(async move {
            let r = me.execute(&spec, &work).await;
            if let Err(e) = r {
                let _ = me.append(&spec.id, &Event::Done { ok: false, summary: format!("{e:#}") });
                let _ = me.finish(&spec.id, false, &format!("{e:#}"));
            }
            me.running.lock().unwrap().remove(&spec.id);
        });
        self.running.lock().unwrap().insert(id.clone(), Running { abort: handle.abort_handle() });
        Ok((self.state(&id)?, true))
    }

    async fn execute(self: &Arc<Self>, spec: &JobSpec, work: &Path) -> Result<()> {
        let mut task = Task::new(&spec.prompt, work);
        task.profile = spec.profile.clone();
        task.model = spec.model.clone();
        task.effort = spec.effort.clone();
        task.permission = spec.permission.clone();
        task.verify = spec.verify.clone();
        task.resume = spec.resume.clone();
        task.account = spec.account.clone();
        task.context = spec.context.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let engine = self.engine.clone();
        let t2 = task.clone();
        let runner = tokio::spawn(async move {
            match t2.verify.clone() {
                Some(v) => crate::verify::run_task(&engine, t2, &v, &tx).await,
                None => {
                    let _ = engine.run(&t2, &tx).await;
                }
            }
        });
        // Cancelling this job drops this future: the run must stop with it,
        // and with the run its CLI and everything that CLI started.
        struct StopRun(tokio::task::AbortHandle);
        impl Drop for StopRun {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _stop = StopRun(runner.abort_handle());
        // Events as they come, with a heartbeat while nothing happens.
        let mut beat = tokio::time::interval(std::time::Duration::from_secs(1));
        let (mut ok, mut summary) = (false, String::new());
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Some(ev) => {
                        if matches!(ev, Event::Delta { .. } | Event::Session { .. }) {
                            continue;
                        }
                        if let Event::Done { ok: o, summary: s } = &ev {
                            (ok, summary) = (*o, s.clone());
                        }
                        let ev = self.engine.redactor.event(&ev);
                        self.append(&spec.id, &ev)?;
                    }
                    None => break,
                },
                _ = beat.tick() => {
                    let _ = self.update(&spec.id, |s| s.heartbeat = now());
                }
            }
        }
        let _ = runner.await;
        self.collect(&spec.id, work)?;
        self.finish(&spec.id, ok, &summary)?;
        Ok(())
    }

    /// The patch and the files the task produced, with a manifest.
    fn collect(&self, id: &str, work: &Path) -> Result<()> {
        let art = self.job_dir(id).join("artifacts");
        std::fs::create_dir_all(&art)?;
        let patch = crate::eval::git_patch(work)?;
        std::fs::write(art.join("patch.diff"), &patch)?;
        let (evs, _) = self.events(id, 0)?;
        for v in evs {
            if let Ok(Event::Artifact { path }) = serde_json::from_value::<Event>(v["event"].clone()) {
                let p = PathBuf::from(&path);
                let src = if p.is_absolute() { p } else { work.join(p) };
                if let (Some(name), true) = (src.file_name(), src.is_file()) {
                    let _ = std::fs::copy(&src, art.join(name));
                }
            }
        }
        let mut files = vec![];
        for e in std::fs::read_dir(&art)?.flatten().filter(|e| e.path().is_file()) {
            let name = e.file_name().to_string_lossy().to_string();
            if name == "manifest.json" {
                continue;
            }
            let bytes = std::fs::read(e.path())?;
            files.push(json!({ "path": name, "bytes": bytes.len(), "sha256": crate::trace::sha256(&bytes) }));
        }
        files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
        std::fs::write(art.join("manifest.json"), serde_json::to_vec_pretty(&json!({ "job": id, "worker": self.name, "files": files }))?)?;
        Ok(())
    }

    fn finish(&self, id: &str, ok: bool, summary: &str) -> Result<JobState> {
        self.update(id, |s| {
            if s.status == "running" {
                s.status = if ok { "done" } else { "failed" }.into();
                s.ended = Some(now());
                s.summary = summary.into();
            }
        })
    }

    /// Stops a job and everything it started, and frees its working folder.
    /// Cancelling again (a lost acknowledgement) returns the same receipt.
    pub fn cancel(&self, id: &str) -> Result<JobState> {
        let st = self.state(id)?;
        if st.status != "running" {
            return Ok(st);
        }
        let handle = self.running.lock().unwrap().remove(id);
        let stopped = handle.is_some();
        if let Some(h) = handle {
            h.abort.abort(); // drops the run: its CLI and every process it started are killed
        }
        let _ = self.append(id, &Event::Log { line: "cancelled by the client".into() });
        let st = self.update(id, |s| {
            s.status = "cancelled".into();
            s.ended = Some(now());
            s.summary = "cancelled".into();
            s.cancel = Some(json!({ "at": now_ms(), "stopped_running_job": stopped }));
        })?;
        // Release the working folder once the processes are gone.
        let work = self.job_dir(id).join("work");
        tokio::spawn(async move {
            for _ in 0..50 {
                if std::fs::remove_dir_all(&work).is_ok() || !work.exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        });
        Ok(st)
    }

    pub fn artifacts(&self, id: &str) -> Result<Value> {
        self.state(id)?;
        let p = self.job_dir(id).join("artifacts").join("manifest.json");
        Ok(serde_json::from_slice(&std::fs::read(p).map_err(|_| anyhow!("job {id} has no artifacts yet"))?)?)
    }

    pub fn artifact(&self, id: &str, name: &str) -> Result<Vec<u8>> {
        self.state(id)?;
        if name.contains(['/', '\\']) || name.starts_with('.') {
            bail!("bad artifact name");
        }
        std::fs::read(self.job_dir(id).join("artifacts").join(name)).map_err(|_| anyhow!("no artifact {name}"))
    }

    /// Whether this worker has a job (for tests and the HTTP layer).
    pub fn job_count(&self) -> usize {
        std::fs::read_dir(self.dir.join("jobs")).map(|d| d.count()).unwrap_or(0)
    }
}

// ── bundles ─────────────────────────────────────────────────────────────────

/// The workspace as a zip (without `.git`, `target` or `node_modules`).
pub fn bundle(dir: &Path) -> Result<Vec<u8>> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut walker = ignore::WalkBuilder::new(dir);
        walker
            .hidden(false)
            .git_ignore(false)
            .filter_entry(|e| !matches!(e.file_name().to_str(), Some(".git") | Some("target") | Some("node_modules")));
        for e in walker.build().flatten().filter(|e| e.file_type().is_some_and(|t| t.is_file())) {
            let rel = e.path().strip_prefix(dir)?.to_string_lossy().replace('\\', "/");
            z.start_file(rel, opts)?;
            z.write_all(&std::fs::read(e.path())?)?;
        }
        z.finish()?;
    }
    Ok(buf.into_inner())
}

pub fn unbundle(bytes: &[u8], to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("the job's bundle")?;
    for i in 0..z.len() {
        let mut f = z.by_index(i)?;
        let Some(rel) = f.enclosed_name() else { bail!("the bundle has an unsafe path") };
        let out = to.join(rel);
        if let Some(d) = out.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut w = std::fs::File::create(&out)?;
        std::io::copy(&mut f, &mut w)?;
    }
    Ok(())
}
