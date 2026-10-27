//! Evaluation: which models and configurations do best on the same tasks.
//!
//! A **dataset** is a folder with a `dataset.toml`: a name, a version and
//! tasks, each a prompt, a fixture repository to work in, the paths in it the
//! candidate may not change (`protected`), files only the evaluator sees
//! (`hidden`, such as the tests that decide the score) and weighted checks
//! (commands). Datasets are registered by content hash, and a changed dataset
//! needs a new version: results stay bound to the version and hash they ran on.
//!
//! A **run** takes one configuration (model, effort, permission mode,
//! verification) through every task, each in a fresh copy of its fixture with
//! the protected files made read-only. The candidate runs through the normal
//! engine. Then the evaluator, not the candidate, scores the work in a
//! separate folder: the pristine fixture, the candidate's changes outside the
//! protected paths, and the hidden files. A failed run, no change at all, or
//! a protected file that changed anyway earns no credit (the last invalidates
//! the task). Reports record the dataset and fixture hashes, the evaluator,
//! the configuration's fingerprint, scores with their evidence, latency,
//! tokens and cost (unknown when no price applies, never a made-up zero).
//!
//! **Comparisons** only pair runs of the same dataset version and evaluator,
//! show the sample size and claim no statistical significance.
//!
//! **Proposals**: an agent proposes a configuration change from earlier
//! failures. It is recorded with what it cites and how it will be judged, and
//! can't become the active configuration until it was evaluated without
//! regressions and a person approved it.

use crate::engine::{Engine, Task};
use crate::events::Event;
use crate::util::now;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The evaluator's identity: changing how scores are computed means a new one.
pub const EVALUATOR: &str = "fermi-eval-1";

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    /// The program and its arguments (no shell).
    pub run: Vec<String>,
    #[serde(default = "one")]
    pub weight: u32,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalTask {
    pub id: String,
    pub prompt: String,
    /// The repository the candidate works in (relative to the dataset).
    pub fixture: String,
    /// Paths in the fixture the candidate may read but not change.
    #[serde(default)]
    pub protected: Vec<String>,
    /// A folder whose files are laid over the work for scoring only.
    #[serde(default)]
    pub hidden: Option<String>,
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dataset {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Longest a candidate may work on one task (default 600 s).
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    pub tasks: Vec<EvalTask>,
    #[serde(skip)]
    pub dir: PathBuf,
}

fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    sha(bytes)
}

/// Files under `dir` (relative, `/`-separated, sorted), without `.git` and `target`.
fn files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok()) {
            let p = e.path();
            let name = e.file_name();
            if name == ".git" || name == "target" {
                continue;
            }
            if p.is_dir() {
                walk(root, &p, out);
            } else if let Ok(rel) = p.strip_prefix(root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = vec![];
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// One hash over a folder's files: paths and contents.
pub fn tree_hash(dir: &Path) -> String {
    let mut h = Sha256::new();
    for rel in files(dir) {
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(std::fs::read(dir.join(&rel)).unwrap_or_default());
        h.update([0]);
    }
    format!("{:x}", h.finalize())
}

fn valid_rel(p: &str) -> bool {
    !p.is_empty() && !Path::new(p).is_absolute() && !p.split(['/', '\\']).any(|c| c == "..")
}

impl Dataset {
    pub fn load(dir: &Path) -> Result<Dataset> {
        let p = dir.join("dataset.toml");
        let text = std::fs::read_to_string(&p).with_context(|| format!("no dataset.toml in {}", dir.display()))?;
        let mut d: Dataset = toml::from_str(&text).with_context(|| format!("in {}", p.display()))?;
        d.dir = dir.to_path_buf();
        if d.name.trim().is_empty() || d.version.trim().is_empty() {
            bail!("a dataset needs a name and a version");
        }
        let mut ids = std::collections::HashSet::new();
        for t in &d.tasks {
            if !ids.insert(t.id.as_str()) {
                bail!("task `{}` appears twice", t.id);
            }
            for rel in std::iter::once(&t.fixture).chain(&t.hidden).chain(&t.protected) {
                if !valid_rel(rel) {
                    bail!("task `{}`: `{rel}` must be a path inside the dataset", t.id);
                }
            }
            if !dir.join(&t.fixture).is_dir() {
                bail!("task `{}`: no fixture folder {}", t.id, t.fixture);
            }
            for p in &t.protected {
                if !dir.join(&t.fixture).join(p).exists() {
                    bail!("task `{}`: protected path {p} isn't in the fixture", t.id);
                }
            }
            if t.checks.is_empty() || t.checks.iter().any(|c| c.run.is_empty() || c.weight == 0) {
                bail!("task `{}` needs checks, each with a command and a weight", t.id);
            }
        }
        if d.tasks.is_empty() {
            bail!("dataset {} has no tasks", d.name);
        }
        Ok(d)
    }

    pub fn hash(&self) -> String {
        tree_hash(&self.dir)
    }

    /// The hash of what one task depends on: its definition, fixture and hidden files.
    pub fn task_hash(&self, t: &EvalTask) -> String {
        let mut h = Sha256::new();
        h.update(serde_json::to_vec(t).unwrap_or_default());
        h.update(tree_hash(&self.dir.join(&t.fixture)).as_bytes());
        if let Some(hd) = &t.hidden {
            h.update(tree_hash(&self.dir.join(hd)).as_bytes());
        }
        format!("{:x}", h.finalize())
    }
}

// ── configurations ──────────────────────────────────────────────────────────

/// What's being evaluated: how fermi runs a task.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EvalConfig {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    /// Run the project's checks after the agent and send failures back.
    #[serde(default)]
    pub verify: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_repairs: Option<u32>,
}

impl EvalConfig {
    /// Identifies what the configuration does (its name aside).
    pub fn fingerprint(&self) -> String {
        let v = json!({ "profile": self.profile, "model": self.model, "effort": self.effort, "permission": self.permission,
            "verify": self.verify, "max_repairs": self.max_repairs, "fermi": env!("CARGO_PKG_VERSION") });
        sha(v.to_string().as_bytes())[..16].to_string()
    }
}

pub fn dir(home: &Path) -> PathBuf {
    home.join("evals")
}

fn log(home: &Path, mut ev: Value) {
    ev["ts"] = json!(now());
    let _ = std::fs::create_dir_all(dir(home));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir(home).join("events.jsonl")) {
        let _ = std::io::Write::write_all(&mut f, format!("{ev}\n").as_bytes());
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(p: &Path) -> Option<T> {
    serde_json::from_slice(&std::fs::read(p).ok()?).ok()
}

fn write_json(p: &Path, v: &impl Serialize) -> Result<()> {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
    std::fs::rename(&tmp, p)?;
    Ok(())
}

// ── the dataset registry ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DatasetRecord {
    pub name: String,
    pub version: String,
    pub hash: String,
    pub tasks: Vec<String>,
    pub registered_at: u64,
}

pub fn datasets(home: &Path) -> Vec<DatasetRecord> {
    read_json(&dir(home).join("datasets.json")).unwrap_or_default()
}

/// Records a dataset version. The same name and version with different
/// contents is refused: results must stay bound to what they ran on.
pub fn register(home: &Path, ds: &Dataset) -> Result<DatasetRecord> {
    let hash = ds.hash();
    let mut all = datasets(home);
    if let Some(r) = all.iter().find(|r| r.name == ds.name && r.version == ds.version) {
        if r.hash != hash {
            bail!(
                "dataset {} version {} was registered with hash {}, but its files now hash to {}: give the changed dataset a new version",
                ds.name,
                ds.version,
                &r.hash[..12],
                &hash[..12]
            );
        }
        return Ok(r.clone());
    }
    let rec = DatasetRecord {
        name: ds.name.clone(),
        version: ds.version.clone(),
        hash,
        tasks: ds.tasks.iter().map(|t| t.id.clone()).collect(),
        registered_at: now(),
    };
    all.push(rec.clone());
    write_json(&dir(home).join("datasets.json"), &all)?;
    log(home, json!({ "event": "dataset_registered", "name": rec.name, "version": rec.version, "hash": rec.hash }));
    Ok(rec)
}

// ── reports ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub weight: u32,
    pub passed: bool,
    pub exit: Option<i32>,
    pub ms: u64,
    /// Evidence: the full output, relative to the run's folder.
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Protection {
    pub before: String,
    pub after: String,
    pub intact: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub id: String,
    pub fixture_hash: String,
    /// passed | failed | error (the candidate's run failed) | no_output | invalidated (protected files changed)
    pub status: String,
    pub score: u32,
    pub max_score: u32,
    pub checks: Vec<CheckResult>,
    pub latency_ms: u64,
    pub model: Option<String>,
    pub account: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    /// None when no price applied to some call.
    pub cost_micros: Option<i64>,
    pub patch: String,
    pub patch_sha256: String,
    pub protection: Protection,
    pub note: String,
    /// The candidate's last message (shortened).
    #[serde(default)]
    pub reply: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub tasks: usize,
    pub passed: usize,
    pub score: u32,
    pub max_score: u32,
    pub latency_ms: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_micros: Option<i64>,
    /// Measurements some tasks lack (e.g. `cost: add, reverse`).
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    pub id: String,
    pub started: u64,
    pub finished: u64,
    pub fermi: String,
    pub evaluator: String,
    pub dataset: DatasetRecord,
    pub config: EvalConfig,
    pub fingerprint: String,
    pub limits: Value,
    pub os: String,
    pub tasks: Vec<TaskResult>,
    pub summary: Summary,
}

pub fn runs_dir(home: &Path) -> PathBuf {
    dir(home).join("runs")
}

pub fn load_report(home: &Path, id: &str) -> Result<Report> {
    read_json(&runs_dir(home).join(id).join("report.json")).ok_or_else(|| anyhow!("no evaluation run {id}"))
}

/// Every run, oldest first.
pub fn reports(home: &Path) -> Vec<Report> {
    let mut out: Vec<Report> =
        std::fs::read_dir(runs_dir(home)).into_iter().flatten().filter_map(|e| read_json(&e.ok()?.path().join("report.json"))).collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

pub fn summarize(tasks: &[TaskResult]) -> Summary {
    let unknown: Vec<&str> = tasks.iter().filter(|t| t.cost_micros.is_none()).map(|t| t.id.as_str()).collect();
    let no_tokens: Vec<&str> = tasks.iter().filter(|t| t.input_tokens.is_none()).map(|t| t.id.as_str()).collect();
    let mut missing = vec![];
    if !unknown.is_empty() {
        missing.push(format!("cost: {}", unknown.join(", ")));
    }
    if !no_tokens.is_empty() {
        missing.push(format!("tokens: {}", no_tokens.join(", ")));
    }
    Summary {
        tasks: tasks.len(),
        passed: tasks.iter().filter(|t| t.status == "passed").count(),
        score: tasks.iter().map(|t| t.score).sum(),
        max_score: tasks.iter().map(|t| t.max_score).sum(),
        latency_ms: tasks.iter().map(|t| t.latency_ms).sum(),
        input_tokens: tasks.iter().filter_map(|t| t.input_tokens).sum(),
        output_tokens: tasks.iter().filter_map(|t| t.output_tokens).sum(),
        cost_micros: if unknown.is_empty() { Some(tasks.iter().filter_map(|t| t.cost_micros).sum()) } else { None },
        missing,
    }
}

// ── running ─────────────────────────────────────────────────────────────────

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "fermi-eval")
        .env("GIT_AUTHOR_EMAIL", "eval@fermi.invalid")
        .env("GIT_COMMITTER_NAME", "fermi-eval")
        .env("GIT_COMMITTER_EMAIL", "eval@fermi.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Commits a folder as it is (fixed identity and date), to diff against later.
pub fn git_baseline(work: &Path) -> Result<()> {
    git(work, &["init", "-q", "-b", "main"])?;
    git(work, &["config", "core.autocrlf", "false"])?;
    // Build output the candidate's own builds and checks leave behind isn't
    // part of its change (scoring skips `target` too).
    let exclude = work.join(".git").join("info").join("exclude");
    std::fs::create_dir_all(exclude.parent().unwrap())?;
    let mut ignore = vec!["target/"];
    ignore.extend(crate::team::JUNK);
    std::fs::write(&exclude, ignore.join("\n") + "\n")?;
    git(work, &["add", "-A"])?;
    git(work, &["commit", "-q", "--allow-empty", "-m", "baseline"])?;
    Ok(())
}

/// Everything changed since [`git_baseline`], as a patch.
pub fn git_patch(work: &Path) -> Result<String> {
    git(work, &["add", "-A"])?;
    git(work, &["diff", "--cached", "--binary", "HEAD"])
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    for rel in files(src) {
        let to = dst.join(&rel);
        if let Some(d) = to.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::copy(src.join(&rel), &to)?;
    }
    Ok(())
}

/// Makes everything under `p` read-only (or writable again): the files, and
/// the folders too (read-only on Unix, a deny ACL on Windows), so files can't
/// be added, deleted or replaced either.
fn protect(p: &Path, on: bool) {
    let set = |q: &Path| {
        if let Ok(meta) = std::fs::metadata(q) {
            let mut perm = meta.permissions();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = if meta.is_dir() {
                    if on {
                        0o555
                    } else {
                        0o755
                    }
                } else if on {
                    0o444
                } else {
                    0o644
                };
                perm.set_mode(mode);
            }
            #[cfg(not(unix))]
            {
                if meta.is_dir() {
                    // A folder's read-only flag doesn't stop changes inside it on
                    // Windows: deny everyone adding, deleting or renaming its entries.
                    let sid = "*S-1-1-0";
                    let mut cmd = std::process::Command::new("icacls");
                    cmd.arg(q);
                    if on {
                        cmd.args(["/deny", &format!("{sid}:(WD,AD,DC,DE)")]);
                    } else {
                        cmd.args(["/remove:d", sid]);
                    }
                    {
                        use std::os::windows::process::CommandExt;
                        cmd.creation_flags(0x0800_0000);
                    }
                    let _ = cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
                    return;
                }
                perm.set_readonly(on);
            }
            let _ = std::fs::set_permissions(q, perm);
        }
    };
    if p.is_dir() {
        // Files first when locking, folders first when unlocking.
        if !on {
            set(p);
        }
        for e in std::fs::read_dir(p).into_iter().flatten().filter_map(|e| e.ok()) {
            protect(&e.path(), on);
        }
        if on {
            set(p);
        }
    } else {
        set(p);
    }
}

/// While the candidate runs on Windows, the evaluator holds every protected
/// file open without sharing write or delete access: the OS then refuses to
/// write, delete or replace them (the read-only flag alone doesn't stop a
/// delete there). Elsewhere, read-only files and folders do it.
struct Lock(#[allow(dead_code)] Vec<std::fs::File>);

fn lock(work: &Path, paths: &[String]) -> Lock {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        let mut held = vec![];
        for p in paths {
            let full = work.join(p);
            let list = if full.is_dir() { files(&full).into_iter().map(|r| full.join(r)).collect() } else { vec![full] };
            for f in list {
                if let Ok(h) = std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&f) {
                    held.push(h);
                }
            }
        }
        Lock(held)
    }
    #[cfg(not(windows))]
    {
        let _ = (work, paths);
        Lock(vec![])
    }
}

fn protected_hash(work: &Path, paths: &[String]) -> String {
    let mut h = Sha256::new();
    for p in paths {
        let full = work.join(p);
        h.update(p.as_bytes());
        h.update([0]);
        if full.is_dir() {
            h.update(tree_hash(&full).as_bytes());
        } else {
            h.update(std::fs::read(&full).map(|b| sha(&b)).unwrap_or_else(|_| "missing".into()).as_bytes());
        }
        h.update([0]);
    }
    format!("{:x}", h.finalize())
}

fn under(rel: &str, paths: &[String]) -> bool {
    paths.iter().any(|p| {
        let p = p.trim_end_matches('/');
        rel == p || rel.starts_with(&format!("{p}/"))
    })
}

async fn run_check(c: &Check, cwd: &Path, target: &Path) -> (bool, Option<i32>, u64, String) {
    let started = Instant::now();
    let prog = crate::util::resolve_bin(&c.run[0], None).unwrap_or_else(|_| PathBuf::from(&c.run[0]));
    let mut cmd = tokio::process::Command::new(prog);
    cmd.args(&c.run[1..]).current_dir(cwd).env("CARGO_TARGET_DIR", target).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000);
    let limit = Duration::from_secs(c.timeout_secs.unwrap_or(300));
    let ms = || started.elapsed().as_millis() as u64;
    match tokio::time::timeout(limit, cmd.output()).await {
        Ok(Ok(o)) => {
            let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            (o.status.success(), o.status.code(), ms(), text)
        }
        Ok(Err(e)) => (false, None, ms(), format!("couldn't run {}: {e}", c.run[0])),
        Err(_) => (false, None, ms(), format!("timed out after {} s", limit.as_secs())),
    }
}

/// Runs the candidate on one task and scores its work.
async fn run_task(engine: &Engine, ds: &Dataset, t: &EvalTask, cfg: &EvalConfig, rdir: &Path) -> Result<TaskResult> {
    let tdir = rdir.join("tasks").join(&t.id);
    let work = tdir.join("work");
    std::fs::create_dir_all(&work)?;
    copy_dir(&ds.dir.join(&t.fixture), &work)?;
    git_baseline(&work)?;
    let before = protected_hash(&work, &t.protected);
    for p in &t.protected {
        protect(&work.join(p), true);
    }
    let held = lock(&work, &t.protected);

    // The candidate, through the normal engine.
    let mut task = Task::new(&t.prompt, &work);
    task.profile = cfg.profile.clone();
    task.model = cfg.model.clone();
    task.effort = cfg.effort.clone();
    task.permission = cfg.permission.clone();
    task.id = Some(crate::hub::new_task_id(&crate::hub::history_dir(&engine.home)));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let started = Instant::now();
    let limit = Duration::from_secs(ds.timeout_secs.unwrap_or(600));
    let job = async {
        if cfg.verify {
            let opts = crate::verify::VerifyOpts { max_repairs: cfg.max_repairs, checks: vec![] };
            crate::verify::run_task(engine, task.clone(), &opts, &tx).await;
        } else {
            let _ = engine.run(&task, &tx).await;
        }
    };
    let timed_out = tokio::time::timeout(limit, job).await.is_err();
    let latency_ms = started.elapsed().as_millis() as u64;
    drop(tx);
    let (mut ok, mut model, mut account, mut summary, mut reply) = (false, None, None, String::new(), String::new());
    while let Some(ev) = rx.recv().await {
        match ev {
            Event::Routed { model: m, account: a, .. } => (model, account) = (Some(m), Some(a)),
            Event::Done { ok: o, summary: s } => (ok, summary) = (o, s),
            Event::Text { text } => reply = text.chars().take(2000).collect(),
            _ => {}
        }
    }

    drop(held);
    for p in &t.protected {
        protect(&work.join(p), false);
    }
    let after = protected_hash(&work, &t.protected);
    let intact = before == after;

    // Usage and cost of every call the candidate made for this task.
    let id = task.id.unwrap_or_default();
    let usage: Vec<Value> = std::fs::read_to_string(engine.home.join("usage.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|r| r["task"] == id)
        .collect();
    let sum = |k: &str| -> Option<u64> {
        if usage.is_empty() || usage.iter().any(|r| r[k].is_null()) {
            None
        } else {
            Some(usage.iter().filter_map(|r| r[k].as_u64()).sum())
        }
    };
    let cost_micros = if usage.is_empty() || usage.iter().any(|r| r["cost"].is_null()) {
        None
    } else {
        Some(usage.iter().filter_map(|r| r["cost"]["micros"].as_i64()).sum())
    };

    // The candidate's change, as evidence.
    let patch = git_patch(&work)?;
    std::fs::write(tdir.join("patch.diff"), &patch)?;

    // Scoring, by the evaluator, in its own folder: pristine fixture, the
    // candidate's changes outside the protected paths, then the hidden files.
    let score_dir = tdir.join("score");
    copy_dir(&ds.dir.join(&t.fixture), &score_dir)?;
    let changed = files(&work);
    for rel in files(&ds.dir.join(&t.fixture)).iter().filter(|r| !under(r, &t.protected) && !changed.contains(r)) {
        let _ = std::fs::remove_file(score_dir.join(rel));
    }
    for rel in changed.iter().filter(|r| !under(r, &t.protected)) {
        let to = score_dir.join(rel);
        if let Some(d) = to.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::copy(work.join(rel), &to)?;
    }
    if let Some(h) = &t.hidden {
        copy_dir(&ds.dir.join(h), &score_dir)?;
    }
    let target = rdir.join("target");
    let mut checks = vec![];
    for c in &t.checks {
        let (passed, exit, ms, out) = run_check(c, &score_dir, &target).await;
        let rel = format!("tasks/{}/checks/{}.txt", t.id, c.name.replace(|ch: char| !ch.is_ascii_alphanumeric(), "-"));
        let p = rdir.join(&rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, &out)?;
        checks.push(CheckResult { name: c.name.clone(), weight: c.weight, passed, exit, ms, output: rel });
    }
    let max_score: u32 = t.checks.iter().map(|c| c.weight).sum();
    let earned: u32 = checks.iter().filter(|c| c.passed).map(|c| c.weight).sum();
    let (status, note) = if !intact {
        ("invalidated", "protected files changed during the run: no credit, and the scoring used the pristine copies".to_string())
    } else if timed_out {
        ("error", format!("the candidate ran out of time ({} s)", limit.as_secs()))
    } else if !ok {
        ("error", format!("the candidate's run failed: {summary}"))
    } else if patch.trim().is_empty() {
        ("no_output", "the candidate changed nothing".to_string())
    } else if earned == max_score {
        ("passed", String::new())
    } else {
        ("failed", format!("{} of {} checks passed", checks.iter().filter(|c| c.passed).count(), checks.len()))
    };
    let score = if matches!(status, "passed" | "failed") { earned } else { 0 };
    Ok(TaskResult {
        id: t.id.clone(),
        fixture_hash: ds.task_hash(t),
        status: status.into(),
        score,
        max_score,
        checks,
        latency_ms,
        model,
        account,
        input_tokens: sum("input_tokens"),
        output_tokens: sum("output_tokens"),
        cost_micros,
        patch: format!("tasks/{}/patch.diff", t.id),
        patch_sha256: sha(patch.as_bytes()),
        protection: Protection { before, after, intact },
        note,
        reply,
    })
}

/// Runs `cfg` through every task of `ds`, in a fresh folder of its own.
pub async fn run(engine: &Engine, ds: &Dataset, cfg: &EvalConfig) -> Result<Report> {
    let home = &engine.home;
    let rec = register(home, ds)?;
    let id = format!("{}-{}", crate::util::now_ms(), &crate::util::uuid_v4()[..6]);
    let rdir = runs_dir(home).join(&id);
    std::fs::create_dir_all(&rdir)?;
    let started = now();
    log(
        home,
        json!({ "event": "run_started", "run": id, "dataset": rec.name, "version": rec.version, "config": cfg.name, "fingerprint": cfg.fingerprint() }),
    );
    let mut tasks = vec![];
    for t in &ds.tasks {
        tasks.push(run_task(engine, ds, t, cfg, &rdir).await.with_context(|| format!("task {}", t.id))?);
    }
    let report = Report {
        schema: 1,
        id: id.clone(),
        started,
        finished: now(),
        fermi: env!("CARGO_PKG_VERSION").into(),
        evaluator: EVALUATOR.into(),
        summary: summarize(&tasks),
        dataset: rec,
        fingerprint: cfg.fingerprint(),
        config: cfg.clone(),
        limits: json!({ "timeout_secs": ds.timeout_secs.unwrap_or(600), "max_repairs": cfg.max_repairs, "check_timeout_secs": 300 }),
        os: std::env::consts::OS.into(),
        tasks,
    };
    write_json(&rdir.join("report.json"), &report)?;
    log(home, json!({ "event": "run_finished", "run": id, "score": report.summary.score, "max": report.summary.max_score }));
    Ok(report)
}

// ── comparing ───────────────────────────────────────────────────────────────

/// Two runs side by side. Only runs of the same dataset version and
/// evaluator are compared; the sample size is part of the result.
pub fn compare(a: &Report, b: &Report) -> Result<Value> {
    if a.dataset.hash != b.dataset.hash {
        bail!(
            "these runs used different datasets ({} {} and {} {}); results aren't compared across dataset versions",
            a.dataset.name,
            a.dataset.version,
            b.dataset.name,
            b.dataset.version
        );
    }
    if a.evaluator != b.evaluator {
        bail!("these runs were scored by different evaluators ({} and {})", a.evaluator, b.evaluator);
    }
    let rows: Vec<Value> = a
        .tasks
        .iter()
        .map(|x| {
            let y = b.tasks.iter().find(|y| y.id == x.id);
            json!({
                "task": x.id,
                "a": { "status": x.status, "score": x.score, "max": x.max_score, "latency_ms": x.latency_ms, "cost_micros": x.cost_micros, "tokens": x.input_tokens.zip(x.output_tokens).map(|(i, o)| i + o) },
                "b": y.map(|y| json!({ "status": y.status, "score": y.score, "max": y.max_score, "latency_ms": y.latency_ms, "cost_micros": y.cost_micros, "tokens": y.input_tokens.zip(y.output_tokens).map(|(i, o)| i + o) })),
                "score_delta": y.map(|y| y.score as i64 - x.score as i64),
            })
        })
        .collect();
    let (sa, sb) = (summarize(&a.tasks), summarize(&b.tasks));
    let delta = |x: Option<i64>, y: Option<i64>| x.zip(y).map(|(x, y)| y - x);
    let mut missing: Vec<String> = sa.missing.iter().map(|m| format!("A {m}")).collect();
    missing.extend(sb.missing.iter().map(|m| format!("B {m}")));
    Ok(json!({
        "dataset": a.dataset, "evaluator": a.evaluator,
        "a": { "run": a.id, "config": a.config, "fingerprint": a.fingerprint, "summary": sa },
        "b": { "run": b.id, "config": b.config, "fingerprint": b.fingerprint, "summary": sb },
        "differences": {
            "score": sb.score as i64 - sa.score as i64,
            "passed": sb.passed as i64 - sa.passed as i64,
            "latency_ms": sb.latency_ms as i64 - sa.latency_ms as i64,
            "cost_micros": delta(sa.cost_micros, sb.cost_micros),
        },
        "tasks": rows,
        "sample_size": a.tasks.len(),
        "missing": missing,
        "note": format!("{} tasks, one run each: the differences describe these runs only; no statistical significance is claimed.", a.tasks.len()),
    }))
}

/// Whether two runs had the same setup and agree on their deterministic scores.
pub fn reproducibility(a: &Report, b: &Report) -> Value {
    let mut setup = vec![];
    if a.dataset.hash != b.dataset.hash {
        setup.push("dataset hash".to_string());
    }
    if a.evaluator != b.evaluator {
        setup.push("evaluator".into());
    }
    if a.fingerprint != b.fingerprint {
        setup.push("configuration".into());
    }
    if a.limits != b.limits {
        setup.push("limits".into());
    }
    for x in &a.tasks {
        match b.tasks.iter().find(|y| y.id == x.id) {
            Some(y) if y.fixture_hash != x.fixture_hash => setup.push(format!("fixture {}", x.id)),
            None => setup.push(format!("task {} missing", x.id)),
            _ => {}
        }
    }
    let variation: Vec<Value> = a
        .tasks
        .iter()
        .filter_map(|x| {
            let y = b.tasks.iter().find(|y| y.id == x.id)?;
            (x.score != y.score || x.status != y.status || x.patch_sha256 != y.patch_sha256).then(|| {
                json!({ "task": x.id, "a": { "status": x.status, "score": x.score, "patch": x.patch_sha256 }, "b": { "status": y.status, "score": y.score, "patch": y.patch_sha256 } })
            })
        })
        .collect();
    json!({ "same_setup": setup.is_empty(), "setup_differences": setup, "same_scores": variation.is_empty(), "variation": variation })
}

/// Every version of a dataset, each with its own runs.
pub fn history(home: &Path, name: &str) -> Value {
    let runs = reports(home);
    let versions: Vec<Value> = datasets(home)
        .into_iter()
        .filter(|d| d.name == name)
        .map(|d| {
            let mine: Vec<Value> = runs
                .iter()
                .filter(|r| r.dataset.hash == d.hash)
                .map(|r| json!({ "run": r.id, "config": r.config.name, "fingerprint": r.fingerprint, "score": r.summary.score, "max": r.summary.max_score }))
                .collect();
            json!({ "version": d.version, "hash": d.hash, "tasks": d.tasks, "registered_at": d.registered_at, "runs": mine })
        })
        .collect();
    json!({ "dataset": name, "versions": versions })
}

// ── proposals and the active configuration ─────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Active {
    pub config: EvalConfig,
    pub fingerprint: String,
    pub since: u64,
    /// The proposal that made it active, if any.
    pub proposal: Option<String>,
    pub approved_by: String,
}

pub fn active(home: &Path) -> Option<Active> {
    read_json(&dir(home).join("active.json"))
}

/// Sets the first active configuration. Later changes go through proposals.
pub fn init_active(home: &Path, cfg: &EvalConfig, by: &str) -> Result<Active> {
    if let Some(a) = active(home) {
        bail!("a configuration is already active ({}); change it through an evaluated proposal", a.config.name);
    }
    let a = Active { config: cfg.clone(), fingerprint: cfg.fingerprint(), since: now(), proposal: None, approved_by: by.into() };
    write_json(&dir(home).join("active.json"), &a)?;
    log(home, json!({ "event": "activated", "config": cfg.name, "fingerprint": a.fingerprint, "by": by }));
    Ok(a)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: String,
    pub created: u64,
    /// Who proposed it (the model and account that answered).
    pub author: String,
    /// The failures it's based on: run, task, status and failing checks.
    pub cites: Vec<Value>,
    pub rationale: String,
    pub proposed: EvalConfig,
    /// How it's judged: the dataset version, the baseline, and the rule.
    pub plan: Value,
    /// proposed | evaluated | rejected | adopted
    pub status: String,
    #[serde(default)]
    pub evaluation: Option<Value>,
    #[serde(default)]
    pub review: Option<Value>,
}

fn proposal_path(home: &Path, id: &str) -> PathBuf {
    dir(home).join("proposals").join(format!("{id}.json"))
}

pub fn proposals(home: &Path) -> Vec<Proposal> {
    let mut out: Vec<Proposal> =
        std::fs::read_dir(dir(home).join("proposals")).into_iter().flatten().filter_map(|e| read_json(&e.ok()?.path())).collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

pub fn proposal(home: &Path, id: &str) -> Result<Proposal> {
    read_json(&proposal_path(home, id)).ok_or_else(|| anyhow!("no proposal {id}"))
}

/// Asks an agent (the engine's default routing in `profile`) for one
/// configuration change that addresses the failures in `runs`, and records it.
/// Nothing changes until it's evaluated and approved.
pub async fn propose(engine: &Engine, runs: &[String], profile: Option<&str>) -> Result<Proposal> {
    let home = &engine.home;
    let active = active(home).ok_or_else(|| anyhow!("no active configuration to improve on (set one first)"))?;
    let mut cites = vec![];
    let mut dataset = None;
    for id in runs {
        let r = load_report(home, id)?;
        dataset.get_or_insert(r.dataset.clone());
        for t in r.tasks.iter().filter(|t| t.status != "passed") {
            let failing: Vec<&str> = t.checks.iter().filter(|c| !c.passed).map(|c| c.name.as_str()).collect();
            cites.push(json!({ "run": r.id, "task": t.id, "status": t.status, "failing_checks": failing, "note": t.note, "config": r.config.name }));
        }
    }
    let dataset = dataset.ok_or_else(|| anyhow!("name at least one evaluation run"))?;
    if cites.is_empty() {
        bail!("those runs have no failures to learn from");
    }
    let prompt = format!(
        "You are improving how fermi runs coding tasks. The active configuration is:\n{}\n\nThese evaluation tasks failed:\n{}\n\n\
         Propose ONE change to the configuration that could fix these failures. Fields: model (provider:name), effort, permission, \
         verify (true runs the project's checks and sends failures back), max_repairs. Answer with a JSON object: \
         {{\"name\": \"…\", \"model\": …, \"effort\": …, \"verify\": …, \"max_repairs\": …, \"rationale\": \"why, citing the failures\"}}.",
        serde_json::to_string_pretty(&active.config)?,
        serde_json::to_string_pretty(&cites)?
    );
    let cwd = dir(home).join("proposals");
    std::fs::create_dir_all(&cwd)?;
    let mut task = Task::new(prompt, &cwd);
    task.profile = profile.map(str::to_string);
    task.permission = Some("plan".into());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let out = engine.run(&task, &tx).await?;
    let v = crate::team::json_in_text(&out.text).ok_or_else(|| anyhow!("the proposal wasn't JSON: {}", out.text))?;
    let rationale = v["rationale"].as_str().unwrap_or_default().to_string();
    let mut proposed: EvalConfig = serde_json::from_value(v.clone()).context("the proposed configuration")?;
    if proposed.name.is_empty() {
        proposed.name = format!("proposal-{}", &proposed.fingerprint()[..6]);
    }
    if proposed.profile.is_none() {
        proposed.profile = active.config.profile.clone();
    }
    let id = format!("p-{}-{}", crate::util::now_ms(), &crate::util::uuid_v4()[..4]);
    let p = Proposal {
        id: id.clone(),
        created: now(),
        author: format!("agent ({} @ {})", out.model, out.account),
        cites,
        rationale,
        plan: json!({
            "dataset": dataset, "baseline": { "config": active.config.name, "fingerprint": active.fingerprint },
            "candidate": { "config": proposed.name, "fingerprint": proposed.fingerprint() },
            "rule": "on the same dataset version, the candidate's total score is at least the baseline's and no task the baseline passes fails; then a person approves",
        }),
        proposed,
        status: "proposed".into(),
        evaluation: None,
        review: None,
    };
    write_json(&proposal_path(home, &id), &p)?;
    log(home, json!({ "event": "proposed", "proposal": id, "author": p.author, "cites": p.cites.len() }));
    Ok(p)
}

/// Runs the plan: the candidate (and the baseline, if it hasn't run on that
/// dataset version) on the dataset in `ds`, then applies the rule.
pub async fn evaluate(engine: &Engine, id: &str, ds: &Dataset) -> Result<Proposal> {
    let home = &engine.home;
    let mut p = proposal(home, id)?;
    if p.plan["dataset"]["hash"].as_str() != Some(ds.hash().as_str()) {
        bail!(
            "proposal {id} is to be evaluated on {} {} (hash {}); this dataset differs",
            p.plan["dataset"]["name"],
            p.plan["dataset"]["version"],
            p.plan["dataset"]["hash"]
        );
    }
    let active = active(home).ok_or_else(|| anyhow!("no active configuration"))?;
    let base = match reports(home)
        .into_iter()
        .rev()
        .find(|r| r.fingerprint == active.fingerprint && r.dataset.hash == p.plan["dataset"]["hash"])
    {
        Some(r) => r,
        None => run(engine, ds, &active.config).await?,
    };
    let cand = run(engine, ds, &p.proposed).await?;
    let regressions: Vec<&str> = base
        .tasks
        .iter()
        .filter(|b| b.status == "passed" && cand.tasks.iter().any(|c| c.id == b.id && c.status != "passed"))
        .map(|b| b.id.as_str())
        .collect();
    let pass = cand.summary.score >= base.summary.score && regressions.is_empty();
    p.evaluation = Some(json!({
        "baseline_run": base.id, "candidate_run": cand.id,
        "baseline_score": base.summary.score, "candidate_score": cand.summary.score, "max": cand.summary.max_score,
        "regressions": regressions, "verdict": if pass { "pass" } else { "fail" }, "at": now(),
    }));
    p.status = if pass { "evaluated".into() } else { "rejected".into() };
    write_json(&proposal_path(home, id), &p)?;
    log(home, json!({ "event": "evaluated", "proposal": id, "verdict": if pass { "pass" } else { "fail" } }));
    Ok(p)
}

/// Makes an evaluated proposal the active configuration, with a person's
/// approval. Anything short of that is refused and nothing changes.
pub fn adopt(home: &Path, id: &str, reviewer: &str) -> Result<Active> {
    let mut p = proposal(home, id)?;
    let refuse = |why: String| {
        log(home, json!({ "event": "adoption_refused", "proposal": id, "reviewer": reviewer, "reason": why }));
        anyhow!("proposal {id} can't be adopted: {why}")
    };
    if p.status == "adopted" {
        return Err(refuse("it already was".into()));
    }
    match p.evaluation.as_ref().and_then(|e| e["verdict"].as_str()) {
        None => return Err(refuse("it hasn't been evaluated yet (fermi eval evaluate)".into())),
        Some("pass") => {}
        Some(_) => return Err(refuse("its evaluation found a lower score or a regression".into())),
    }
    let r = reviewer.trim();
    if r.is_empty() || r.to_lowercase().starts_with("agent") {
        return Err(refuse("a person has to approve it, by name".into()));
    }
    let a = Active {
        config: p.proposed.clone(),
        fingerprint: p.proposed.fingerprint(),
        since: now(),
        proposal: Some(id.into()),
        approved_by: r.into(),
    };
    write_json(&dir(home).join("active.json"), &a)?;
    p.status = "adopted".into();
    p.review = Some(json!({ "approved_by": r, "at": now() }));
    write_json(&proposal_path(home, id), &p)?;
    log(home, json!({ "event": "adopted", "proposal": id, "fingerprint": a.fingerprint, "approved_by": r }));
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_and_summaries() {
        let a = EvalConfig { name: "a".into(), model: Some("claude:sonnet".into()), ..Default::default() };
        let mut b = a.clone();
        b.name = "renamed".into();
        assert_eq!(a.fingerprint(), b.fingerprint(), "the name doesn't change what a configuration does");
        b.verify = true;
        assert_ne!(a.fingerprint(), b.fingerprint());
        assert!(under("tests/visible.rs", &["tests".into()]) && !under("testsuite.rs", &["tests".into()]));
        let t = |id: &str, cost: Option<i64>| TaskResult {
            id: id.into(),
            fixture_hash: String::new(),
            status: "passed".into(),
            score: 4,
            max_score: 4,
            checks: vec![],
            latency_ms: 10,
            model: None,
            account: None,
            input_tokens: Some(5),
            output_tokens: Some(1),
            cost_micros: cost,
            patch: String::new(),
            patch_sha256: String::new(),
            protection: Protection { before: String::new(), after: String::new(), intact: true },
            note: String::new(),
            reply: String::new(),
        };
        let s = summarize(&[t("x", Some(3)), t("y", None)]);
        assert_eq!((s.score, s.cost_micros), (8, None));
        assert_eq!(s.missing, ["cost: y"]);
    }
}
