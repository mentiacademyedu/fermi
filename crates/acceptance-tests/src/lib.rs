//! Acceptance-test harness for fermi (the "APEX ADE" of the acceptance plan).
//!
//! - [`Case`]: one acceptance case attempt: its scratch folder, assertions and
//!   evidence (`result.json` plus a SHA-256 manifest of every artifact).
//! - [`Fermi`]: an isolated fermi home whose accounts run the deterministic
//!   `fake-cli` (see `src/bin/fake-cli.rs`) through the real engine and hub.
//! - Fixtures: pinned folders under `tests/fixtures` copied into a case and
//!   committed with fixed dates, so every run starts from the same commit.

use anyhow::{bail, Context, Result};
use fermi_core::config::Config;
use fermi_core::engine::{Engine, Task};
use fermi_core::hub::{Hub, TaskRec, TaskStatus};
use fermi_core::state::Store;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// The repository root (two levels above this crate).
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().to_path_buf()
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// UTC timestamp, RFC 3339.
pub fn utc_now() -> String {
    let secs = (now_ms() / 1000) as i64;
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // Civil from days (Howard Hinnant).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

pub fn sha256(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// One hash over every file under `dir` (relative paths and contents), skipping `.git`.
pub fn tree_hash(dir: &Path) -> String {
    let mut files = vec![];
    collect(dir, dir, &mut files);
    files.sort();
    let mut h = Sha256::new();
    for rel in files {
        h.update(rel.as_bytes());
        h.update([0]);
        h.update(std::fs::read(dir.join(&rel)).unwrap_or_default());
        h.update([0]);
    }
    format!("{:x}", h.finalize())
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<String>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok()) {
        let p = e.path();
        if e.file_name() == ".git" {
            continue;
        }
        if p.is_dir() {
            collect(root, &p, out);
        } else {
            out.push(p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"));
        }
    }
}

// ── a case ──────────────────────────────────────────────────────────────────

/// One attempt of one acceptance case: assertions plus evidence.
pub struct Case {
    pub id: String,
    pub run_id: String,
    pub attempt: String,
    /// Where evidence goes: tests/reports/<run>/<id>/<attempt>.
    pub dir: PathBuf,
    /// Scratch space for fixtures and homes (outside the reports).
    pub work: PathBuf,
    checks: Vec<(String, bool, String)>,
    artifacts: Vec<String>,
    real: Vec<String>,
    mocked: Vec<String>,
    tier: String,
    started: String,
    clock: Instant,
}

impl Case {
    /// Starts case `id`. The runner passes the run and attempt ids in the
    /// environment; a plain `cargo test` gets local ones.
    pub fn start(id: &str, tier: &str) -> Case {
        let run_id = std::env::var("ACCEPTANCE_RUN_ID").unwrap_or_else(|_| format!("local-{}", now_ms()));
        let attempt = std::env::var("ACCEPTANCE_ATTEMPT").unwrap_or_else(|_| "a1".into());
        let reports =
            std::env::var_os("ACCEPTANCE_REPORTS").map(PathBuf::from).unwrap_or_else(|| repo_root().join("tests").join("reports"));
        let dir = reports.join(&run_id).join(id).join(&attempt);
        let work = repo_root().join("target").join("acceptance").join(&run_id).join(id).join(&attempt);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&dir).expect("report dir");
        std::fs::create_dir_all(&work).expect("work dir");
        Case {
            id: id.into(),
            run_id,
            attempt,
            dir,
            work,
            checks: vec![],
            artifacts: vec![],
            real: vec![],
            mocked: vec![],
            tier: tier.into(),
            started: utc_now(),
            clock: Instant::now(),
        }
    }

    /// Records an assertion; the case passes only if every one holds.
    pub fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Display) -> bool {
        let detail = detail.to_string();
        eprintln!("[{}] {} {name}: {detail}", self.id, if ok { "ok  " } else { "FAIL" });
        self.checks.push((name.into(), ok, detail));
        ok
    }

    pub fn real(&mut self, components: &[&str]) {
        self.real.extend(components.iter().map(|s| s.to_string()));
    }

    pub fn mocked(&mut self, components: &[&str]) {
        self.mocked.extend(components.iter().map(|s| s.to_string()));
    }

    pub fn json(&mut self, name: &str, v: &Value) {
        self.text(name, &serde_json::to_string_pretty(v).unwrap_or_default());
    }

    pub fn text(&mut self, name: &str, s: &str) {
        let p = self.dir.join(name);
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        std::fs::write(&p, s).expect("artifact");
        if !self.artifacts.contains(&name.to_string()) {
            self.artifacts.push(name.into());
        }
    }

    /// Writes result.json and the artifact manifest. Panics (failing the
    /// test) unless at least one assertion was made and all of them held.
    pub fn finish(mut self) {
        let failed: Vec<&(String, bool, String)> = self.checks.iter().filter(|c| !c.1).collect();
        let status = if self.checks.is_empty() || !failed.is_empty() { "failed" } else { "passed" };
        let mut manifest = vec![];
        for a in &self.artifacts {
            let bytes = std::fs::read(self.dir.join(a)).unwrap_or_default();
            manifest.push(json!({ "path": a, "bytes": bytes.len(), "sha256": sha256(&bytes) }));
        }
        let manifest = json!({ "files": manifest });
        std::fs::write(self.dir.join("manifest.json"), serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
        let commit = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root())
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
        let dirty = std::process::Command::new("git")
            .arg("-C")
            .arg(repo_root())
            .args(["status", "--porcelain"])
            .output()
            .ok()
            .is_some_and(|o| !o.stdout.is_empty());
        let result = json!({
            "schemaVersion": 1,
            "testId": self.id,
            "runId": self.run_id,
            "attemptId": self.attempt,
            "executionState": "completed",
            "status": status,
            "tier": self.tier,
            "commitSha": commit,
            "dirtyTree": dirty,
            "startedAt": self.started,
            "finishedAt": utc_now(),
            "durationMs": self.clock.elapsed().as_millis() as u64,
            "realComponents": self.real,
            "mockedComponents": self.mocked,
            "assertions": self.checks.iter().map(|(n, ok, d)| json!({ "name": n, "passed": ok, "detail": d })).collect::<Vec<_>>(),
            "artifacts": self.artifacts,
            "reason": if failed.is_empty() { Value::Null } else { json!(failed.iter().map(|c| format!("{}: {}", c.0, c.2)).collect::<Vec<_>>().join("; ")) },
        });
        std::fs::write(self.dir.join("result.json"), serde_json::to_string_pretty(&result).unwrap()).unwrap();
        self.artifacts.clear();
        assert!(!self.checks.is_empty(), "{}: no assertions were made", self.id);
        assert!(
            failed.is_empty(),
            "{} failed: {}",
            self.id,
            failed.iter().map(|c| format!("{} ({})", c.0, c.2)).collect::<Vec<_>>().join("; ")
        );
    }
}

// ── fixtures ────────────────────────────────────────────────────────────────

pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .output()
        .context("running git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for e in std::fs::read_dir(src)? {
        let e = e?;
        let to = dst.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&e.path(), &to)?;
        } else {
            std::fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}

/// A pinned fixture copied to `dest` and committed with fixed identity and
/// dates (so the commit id is the same on every run). Returns the commit.
pub struct Fixture {
    pub path: PathBuf,
    pub commit: String,
    /// Hash of the pinned source, to prove the oracle wasn't touched.
    pub source_hash: String,
}

pub fn fixture(name: &str, dest: &Path) -> Result<Fixture> {
    let src = repo_root().join("tests").join("fixtures").join(name).join("repo");
    copy_dir(&src, dest).with_context(|| format!("copying fixture {name}"))?;
    git(dest, &["init", "-q", "-b", "main"])?;
    // Byte-identical working trees on every machine (Git for Windows
    // otherwise converts line endings on checkout and merge).
    git(dest, &["config", "core.autocrlf", "false"])?;
    git(dest, &["add", "-A"])?;
    git(dest, &["commit", "-q", "-m", &format!("fixture {name}")])?;
    let commit = git(dest, &["rev-parse", "HEAD"])?;
    Ok(Fixture { path: dest.to_path_buf(), commit, source_hash: tree_hash(&src) })
}

/// The fixture's read-only ground truth (`tests/fixtures/<name>/<file>`).
pub fn ground_truth(name: &str, file: &str) -> Value {
    let p = repo_root().join("tests").join("fixtures").join(name).join(file);
    serde_json::from_slice(&std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).expect("ground truth JSON")
}

// ── fermi with fake providers ───────────────────────────────────────────────

/// A configured account whose CLI is the fake.
pub struct Account {
    pub id: &'static str,
    /// claude | openai | gemini
    pub provider: &'static str,
    pub steps: Vec<Value>,
    /// In the test profile (false: configured but not allowed for its tasks).
    pub in_profile: bool,
}

pub fn account(id: &'static str, provider: &'static str, steps: Vec<Value>) -> Account {
    Account { id, provider, steps, in_profile: true }
}

impl Account {
    /// Configured, but not one of the profile's accounts.
    pub fn outside(mut self) -> Self {
        self.in_profile = false;
        self
    }
}

/// An isolated fermi home: config, accounts with scenarios, and the engine.
pub struct Fermi {
    pub home: PathBuf,
    pub engine: Arc<Engine>,
}

/// The fake CLI Cargo builds next to the test binaries (target/<profile>/).
pub fn fake_cli() -> PathBuf {
    let exe = std::env::current_exe().expect("test binary path");
    let name = format!("fake-cli{}", std::env::consts::EXE_SUFFIX);
    exe.ancestors()
        .skip(1)
        .take(3)
        .map(|d| d.join(&name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} not built next to {}", exe.display()))
}

impl Fermi {
    /// `profile` lists the accounts (in order) of the single profile `p`.
    /// `extra` is appended to config.toml (models, profile settings…).
    pub fn new(case: &Case, accounts: &[Account], extra_profile: &str) -> Result<Fermi> {
        Self::at(case, "fermi-home", accounts, extra_profile)
    }

    /// Like [`Fermi::new`], in its own folder (for several scenarios in one case).
    pub fn at(case: &Case, name: &str, accounts: &[Account], extra_profile: &str) -> Result<Fermi> {
        let home = case.work.join(name);
        std::fs::create_dir_all(&home)?;
        let bin = fake_cli().display().to_string().replace('\\', "/");
        let mut cfg = format!("default_profile = \"p\"\n\n[bins]\nclaude = \"{bin}\"\ncodex = \"{bin}\"\ngemini = \"{bin}\"\n\n");
        for a in accounts {
            let dir = home.join("accounts").join(a.id);
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("scenario.json"), serde_json::to_vec_pretty(&json!({ "steps": a.steps }))?)?;
            // What fermi checks to see an account as logged in.
            match a.provider {
                "claude" => std::fs::write(dir.join(".credentials.json"), "{}")?,
                "openai" => std::fs::write(dir.join("auth.json"), "{}")?,
                _ => {
                    std::fs::create_dir_all(dir.join(".gemini"))?;
                    std::fs::write(dir.join(".gemini").join("oauth_creds.json"), "{}")?
                }
            }
            cfg += &format!(
                "[[accounts]]\nid = \"{}\"\nprovider = \"{}\"\nauth = \"cli\"\nhome = \"{}\"\n\n",
                a.id,
                a.provider,
                dir.display().to_string().replace('\\', "/")
            );
        }
        let ids: Vec<String> = accounts.iter().filter(|a| a.in_profile).map(|a| format!("\"{}\"", a.id)).collect();
        cfg += &format!("[[profiles]]\nname = \"p\"\naccounts = [{}]\nmode = \"auto\"\n{extra_profile}\n", ids.join(", "));
        std::fs::write(home.join("config.toml"), &cfg)?;
        let cfg = Config::parse(&cfg)?;
        let engine = Arc::new(Engine { catalog: cfg.catalog(), store: Store::open(&home), cfg, home: home.clone(), depth: 0 });
        Ok(Fermi { home, engine })
    }

    pub fn account_dir(&self, id: &str) -> PathBuf {
        self.home.join("accounts").join(id)
    }

    /// Every call the fake CLI received for an account, in order.
    pub fn calls(&self, id: &str) -> Vec<Value> {
        std::fs::read_to_string(self.account_dir(id).join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Creates a barrier file in an account folder (the fake waits for it).
    pub fn release(&self, account: &str, name: &str) {
        std::fs::write(self.account_dir(account).join(name), "go").unwrap();
    }

    pub fn hub(&self) -> Arc<Hub> {
        Hub::new(self.engine.clone(), "acceptance")
    }

    pub fn task(&self, prompt: &str, cwd: &Path) -> Task {
        let mut t = Task::new(prompt, cwd);
        t.profile = Some("p".into());
        t
    }
}

/// Waits until a hub task leaves Running (or the deadline passes).
pub async fn wait_task(hub: &Hub, id: u64, timeout: Duration) -> Result<TaskRec> {
    let start = Instant::now();
    loop {
        if let Some(t) = hub.get(id) {
            if t.status != TaskStatus::Running {
                return Ok(t);
            }
        }
        if start.elapsed() > timeout {
            bail!("task {id} still running after {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Waits until `file` exists.
pub async fn wait_file(file: &Path, timeout: Duration) -> Result<()> {
    let start = Instant::now();
    while !file.exists() {
        if start.elapsed() > timeout {
            bail!("{} didn't appear within {timeout:?}", file.display());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// Whether a process id is still running.
pub fn alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"]).output();
        out.map(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\""))).unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        // A zombie (exited, not yet reaped) isn't running.
        std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .is_ok_and(|st| st.rsplit(')').next().is_some_and(|rest| !rest.trim_start().starts_with('Z')))
    }
}

/// Kills a process by id.
pub fn kill(pid: u32) {
    #[cfg(windows)]
    let _ = std::process::Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).output();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).output();
}
