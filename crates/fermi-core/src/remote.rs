//! Running tasks on workers (`[[workers]]`, see `worker`): fermi bundles the
//! task's folder, dispatches it, follows the job's numbered events and, when
//! it ends, downloads its artifacts (checksummed) and applies its patch to
//! the folder, once.
//!
//! - **Outages**: a worker that doesn't answer is waited for up to
//!   `[remote] heartbeat_secs`; then the attempt counts as lost, and the task
//!   is retried on the next worker, at most `[remote] retries` times. Only a
//!   finished job's result is ever applied.
//! - **Reconnecting**: the client asks for events after the last one it saw,
//!   so nothing is missed or repeated, and resending a job is harmless (the id
//!   is the client's).
//! - **Closing the window** doesn't stop a job: the task keeps its worker and
//!   job, and the next fermi window to open picks up following it.
//! - **Cancelling** asks the worker to stop the job, and keeps asking until
//!   the worker confirms, even if an answer gets lost on the way.

use crate::config::WorkerCfg;
use crate::engine::{Engine, Outcome, Task};
use crate::events::{emit, Event, Tx};
use crate::worker::{JobSpec, JobState, HEADER};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(250);

/// A worker speaking a protocol this fermi doesn't: refused, never retried.
#[derive(Debug)]
pub struct Incompatible(pub String);

impl std::fmt::Display for Incompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Incompatible {}

/// A worker, as seen from here.
pub struct Client {
    pub name: String,
    url: String,
    token: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(w: &WorkerCfg) -> Client {
        Client {
            name: w.name.clone(),
            url: w.url.trim_end_matches('/').to_string(),
            token: w.token(),
            http: reqwest::Client::builder().timeout(Duration::from_secs(30)).build().unwrap_or_default(),
        }
    }

    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http.request(m, format!("{}{path}", self.url)).header(HEADER, crate::worker::protocol().to_string()).bearer_auth(&self.token)
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<Value> {
        let r = rb.send().await.with_context(|| format!("worker {} unreachable", self.name))?;
        let status = r.status();
        let v: Value = r.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            bail!("worker {}: {}", self.name, v["error"].as_str().unwrap_or(status.as_str()));
        }
        Ok(v)
    }

    /// The handshake: refuses a worker whose protocol this fermi doesn't speak.
    pub async fn hello(&self) -> Result<Value> {
        let v = self.send(self.req(reqwest::Method::GET, "/v1/hello")).await?;
        let theirs = v["protocol"].as_u64().unwrap_or(0) as u32;
        if !crate::worker::compatible(theirs) {
            return Err(Incompatible(format!(
                "worker {} speaks protocol {theirs}, but this fermi supports protocol {}–{}: update one of them",
                self.name,
                crate::worker::SUPPORTED.0,
                crate::worker::SUPPORTED.1
            ))
            .into());
        }
        Ok(v)
    }

    pub async fn submit(&self, spec: &JobSpec, bundle: &[u8]) -> Result<JobState> {
        use base64::Engine as _;
        let body = json!({ "spec": spec, "bundle": base64::engine::general_purpose::STANDARD.encode(bundle) });
        let v = self.send(self.req(reqwest::Method::POST, "/v1/jobs").json(&body)).await?;
        Ok(serde_json::from_value(v["job"].clone())?)
    }

    pub async fn events(&self, job: &str, after: u64) -> Result<(Vec<Value>, JobState)> {
        let v = self.send(self.req(reqwest::Method::GET, &format!("/v1/jobs/{job}/events?after={after}"))).await?;
        Ok((v["events"].as_array().cloned().unwrap_or_default(), serde_json::from_value(v["job"].clone())?))
    }

    pub async fn state(&self, job: &str) -> Result<JobState> {
        let v = self.send(self.req(reqwest::Method::GET, &format!("/v1/jobs/{job}"))).await?;
        Ok(serde_json::from_value(v)?)
    }

    pub async fn cancel(&self, job: &str) -> Result<JobState> {
        let v = self.send(self.req(reqwest::Method::POST, &format!("/v1/jobs/{job}/cancel"))).await?;
        Ok(serde_json::from_value(v)?)
    }

    pub async fn manifest(&self, job: &str) -> Result<Value> {
        self.send(self.req(reqwest::Method::GET, &format!("/v1/jobs/{job}/artifacts"))).await
    }

    pub async fn download(&self, job: &str, name: &str) -> Result<Vec<u8>> {
        let r = self.req(reqwest::Method::GET, &format!("/v1/jobs/{job}/artifacts/{name}")).send().await?;
        if !r.status().is_success() {
            bail!("worker {}: no artifact {name}", self.name);
        }
        Ok(r.bytes().await?.to_vec())
    }
}

/// The workers a task runs on, in order (none: it runs here).
pub fn workers_for(engine: &Engine, task: &Task) -> Result<Vec<WorkerCfg>> {
    let names: Vec<String> = match &task.worker {
        Some(w) => vec![w.clone()],
        None => match engine.cfg.resolve_profile(task.profile.as_deref(), &task.cwd) {
            Ok(p) => p.workers,
            Err(_) => vec![],
        },
    };
    names
        .iter()
        .map(|n| engine.cfg.workers.iter().find(|w| &w.name == n).cloned().ok_or_else(|| anyhow!("no worker `{n}` in [[workers]]")))
        .collect()
}

pub fn dir(home: &Path, task: u64) -> PathBuf {
    home.join("remote").join(task.to_string())
}

/// What happened to a task's jobs: dispatched, lost, finished, applied, cancelled.
fn ledger(engine: &Engine, task: u64, mut v: Value) {
    v["at"] = json!(crate::util::now_ms());
    let d = dir(&engine.home, task);
    let _ = std::fs::create_dir_all(&d);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(d.join("ledger.jsonl")) {
        let _ = std::io::Write::write_all(&mut f, format!("{v}\n").as_bytes());
    }
}

// ── detaching vs cancelling ─────────────────────────────────────────────────

/// Tasks whose window is closing: their jobs keep running.
static DETACHED: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

pub fn detach(task: u64) {
    DETACHED.lock().unwrap().get_or_insert_with(HashSet::new).insert(task);
}

fn detached(task: u64) -> bool {
    DETACHED.lock().unwrap().as_mut().is_some_and(|s| s.remove(&task))
}

/// Cancels the job when the follower is dropped (the task was cancelled),
/// unless the window is only closing.
struct CancelOnDrop {
    worker: WorkerCfg,
    job: String,
    task: u64,
    home: PathBuf,
    armed: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.armed || detached(self.task) {
            return;
        }
        let (w, job, task, home) = (self.worker.clone(), self.job.clone(), self.task, self.home.clone());
        let Ok(rt) = tokio::runtime::Handle::try_current() else { return };
        rt.spawn(async move {
            let c = Client::new(&w);
            // Ask until the worker confirms; an answer lost on the way is
            // reconciled by asking for the job's state.
            let deadline = Instant::now() + Duration::from_secs(30);
            while Instant::now() < deadline {
                let confirmed = match c.cancel(&job).await {
                    Ok(st) => st.terminal(),
                    Err(_) => c.state(&job).await.is_ok_and(|st| st.terminal()),
                };
                if confirmed {
                    let d = dir(&home, task);
                    let _ = std::fs::create_dir_all(&d);
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(d.join("ledger.jsonl")) {
                        let _ = std::io::Write::write_all(
                            &mut f,
                            format!("{}\n", json!({ "event": "cancelled", "job": job, "worker": w.name, "at": crate::util::now_ms() }))
                                .as_bytes(),
                        );
                    }
                    return;
                }
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        });
    }
}

// ── running and following ───────────────────────────────────────────────────

enum Followed {
    Ended(JobState),
    Lost(String),
}

/// Forwards the job's events after `after` until it ends or the worker is lost.
async fn follow(engine: &Engine, c: &Client, job: &str, mut after: u64, tx: &Tx) -> Followed {
    let limit = Duration::from_secs(engine.cfg.remote.heartbeat_secs.unwrap_or(30));
    let mut last_ok = Instant::now();
    let mut warned = false;
    loop {
        match c.events(job, after).await {
            Ok((evs, st)) => {
                if warned {
                    emit(tx, Event::Log { line: format!("worker {} reachable again; caught up from event {after}", c.name) });
                    warned = false;
                }
                last_ok = Instant::now();
                let before = after;
                for e in evs {
                    let seq = e["seq"].as_u64().unwrap_or(0);
                    if seq <= after {
                        continue;
                    }
                    after = seq;
                    if let Ok(ev) = serde_json::from_value::<Event>(e["event"].clone()) {
                        // The task's own end is decided here, after the result is applied.
                        if !matches!(ev, Event::Done { .. }) {
                            emit(tx, ev);
                        }
                    }
                }
                if after > before {
                    emit(tx, Event::Remote { worker: c.name.clone(), job: job.into(), seq: after });
                }
                if st.status == "interrupted" {
                    return Followed::Lost(format!("worker {} restarted and lost the job", c.name));
                }
                if st.terminal() && st.seq <= after {
                    return Followed::Ended(st);
                }
            }
            Err(e) => {
                if !warned {
                    emit(tx, Event::Log { line: format!("{e:#}; waiting for it (up to {} s)", limit.as_secs()) });
                    warned = true;
                }
                if last_ok.elapsed() > limit {
                    return Followed::Lost(format!("worker {} didn't answer for {} s", c.name, limit.as_secs()));
                }
            }
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Downloads a finished job's artifacts, checks them against its manifest
/// and applies its patch to the task's folder.
async fn take_result(engine: &Engine, c: &Client, task: &Task, job: &str, tx: &Tx) -> Result<Vec<PathBuf>> {
    let id = task.id.unwrap_or_default();
    let m = c.manifest(job).await?;
    let to = dir(&engine.home, id).join(job);
    std::fs::create_dir_all(&to)?;
    let mut files = vec![];
    for f in m["files"].as_array().into_iter().flatten() {
        let name = f["path"].as_str().unwrap_or_default();
        let bytes = c.download(job, name).await?;
        let sum = crate::trace::sha256(&bytes);
        if Some(sum.as_str()) != f["sha256"].as_str() {
            bail!("artifact {name} from worker {} doesn't match its checksum", c.name);
        }
        std::fs::write(to.join(name), &bytes)?;
        files.push(to.join(name));
    }
    std::fs::write(to.join("manifest.json"), serde_json::to_vec_pretty(&m)?)?;
    let patch = to.join("patch.diff");
    if std::fs::metadata(&patch).is_ok_and(|m| m.len() > 0) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&task.cwd)
            .args(["apply", "--whitespace=nowarn"])
            .arg(&patch)
            .output()
            .context("running git apply")?;
        if !out.status.success() {
            bail!("the worker's changes don't apply here: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        ledger(
            engine,
            id,
            json!({ "event": "applied", "job": job, "worker": c.name, "patch_sha256": crate::trace::sha256(&std::fs::read(&patch)?) }),
        );
        emit(tx, Event::Log { line: format!("applied the changes made on worker {}", c.name) });
    }
    for f in files.iter().filter(|f| f.file_name().is_some_and(|n| n != "patch.diff")) {
        emit(tx, Event::Artifact { path: f.display().to_string() });
    }
    Ok(files)
}

/// What the worker's events said about who answered.
fn outcome(engine: &Engine, task: &Task, st: &JobState, worker: &str) -> Outcome {
    let rec = crate::hub::read_task(&crate::hub::history_dir(&engine.home), task.id.unwrap_or_default());
    let routed = rec.and_then(|r| r.ran_on);
    Outcome {
        text: st.summary.clone(),
        model: routed.as_ref().map(|r| r.model.clone()).unwrap_or_default(),
        account: format!("{}@{worker}", routed.map(|r| r.account).unwrap_or_default()),
        usage: Default::default(),
        artifacts: vec![],
    }
}

/// Follows a dispatched job to its end, then takes its result (shared by
/// `run` and by a window picking up a task after a restart).
async fn finish_job(engine: &Engine, w: &WorkerCfg, task: &Task, job: &str, after: u64, tx: &Tx) -> Result<Option<JobState>> {
    let c = Client::new(w);
    let mut guard =
        CancelOnDrop { worker: w.clone(), job: job.into(), task: task.id.unwrap_or_default(), home: engine.home.clone(), armed: true };
    let id = task.id.unwrap_or_default();
    match follow(engine, &c, job, after, tx).await {
        Followed::Ended(st) => {
            guard.armed = false;
            ledger(engine, id, json!({ "event": "finished", "job": job, "worker": w.name, "status": st.status }));
            if st.status == "done" {
                take_result(engine, &c, task, job, tx).await?;
            }
            Ok(Some(st))
        }
        Followed::Lost(why) => {
            guard.armed = false;
            ledger(engine, id, json!({ "event": "lost", "job": job, "worker": w.name, "reason": why }));
            emit(tx, Event::Log { line: format!("{why}: this attempt is interrupted") });
            Ok(None)
        }
    }
}

fn done(tx: &Tx, st: &JobState, worker: &str) -> Result<()> {
    if st.status == "done" {
        emit(tx, Event::Done { ok: true, summary: format!("{} (on worker {worker})", st.summary) });
        Ok(())
    } else {
        let msg = format!("{} on worker {worker}: {}", st.status, st.summary);
        emit(tx, Event::Done { ok: false, summary: msg.clone() });
        Err(anyhow!(msg))
    }
}

/// Runs `task` on its workers, retrying on the next one when a worker is lost.
pub async fn run(engine: &Engine, task: &Task, tx: &Tx) -> Result<Outcome> {
    let r = run_inner(engine, task, tx).await;
    if let Err(e) = &r {
        if !e.to_string().contains("on worker") {
            emit(tx, Event::Done { ok: false, summary: format!("{e:#}") });
        }
    }
    r
}

async fn run_inner(engine: &Engine, task: &Task, tx: &Tx) -> Result<Outcome> {
    let workers = workers_for(engine, task)?;
    if workers.is_empty() {
        bail!("no worker to run this task on");
    }
    let id = task.id.unwrap_or_default();
    let retries = engine.cfg.remote.retries.unwrap_or(1) as usize;
    let bundle = crate::worker::bundle(&task.cwd)?;
    for i in 0..=retries {
        let w = &workers[i % workers.len()];
        let c = Client::new(w);
        // Refused before anything runs when the versions don't match; a
        // worker that doesn't answer at all counts as a lost attempt.
        let hello = match c.hello().await {
            Ok(h) => h,
            Err(e) if e.is::<Incompatible>() => return Err(e),
            Err(e) => {
                ledger(engine, id, json!({ "event": "lost", "job": null, "worker": w.name, "reason": format!("{e:#}") }));
                emit(tx, Event::Log { line: format!("{e:#}: trying the next worker") });
                continue;
            }
        };
        let job = format!("t{id}-a{}-r{i}", task.attempt);
        let spec = JobSpec {
            id: job.clone(),
            prompt: task.prompt.clone(),
            profile: task.profile.clone(),
            model: task.model.clone(),
            effort: task.effort.clone(),
            permission: task.permission.clone(),
            verify: task.verify.clone(),
            resume: task.resume.clone(),
            account: task.account.clone(),
            context: task.context.clone(),
            task: task.id,
        };
        // Resending is harmless: the worker knows the job by its id.
        let limit = Duration::from_secs(engine.cfg.remote.heartbeat_secs.unwrap_or(30));
        let started = Instant::now();
        let st = loop {
            match c.submit(&spec, &bundle).await {
                Ok(st) => break Some(st),
                Err(_) if started.elapsed() < limit => tokio::time::sleep(POLL).await,
                Err(e) => {
                    emit(tx, Event::Log { line: format!("{e:#}") });
                    break None;
                }
            }
        };
        let Some(_) = st else { continue };
        ledger(
            engine,
            id,
            json!({ "event": "dispatched", "job": job, "worker": w.name, "worker_protocol": hello["protocol"], "runtime": hello["runtime"] }),
        );
        emit(tx, Event::Remote { worker: w.name.clone(), job: job.clone(), seq: 0 });
        if let Some(st) = finish_job(engine, w, task, &job, 0, tx).await? {
            done(tx, &st, &w.name)?;
            return Ok(outcome(engine, task, &st, &w.name));
        }
    }
    bail!("every attempt lost its worker ({} tried)", retries + 1)
}

/// Picks up following a task's job (a window reopened while it ran).
pub async fn resume(engine: &Engine, task: &Task, worker: &str, job: &str, after: u64, tx: &Tx) -> Result<()> {
    let w = engine.cfg.workers.iter().find(|w| w.name == worker).cloned().ok_or_else(|| anyhow!("no worker `{worker}` in [[workers]]"))?;
    emit(tx, Event::Log { line: format!("following job {job} on worker {worker} again, from event {after}") });
    match finish_job(engine, &w, task, job, after, tx).await? {
        Some(st) => done(tx, &st, worker),
        None => {
            emit(tx, Event::Done { ok: false, summary: format!("lost worker {worker} while following job {job}") });
            Ok(())
        }
    }
}
