//! Verification: fermi runs a project's checks (build, tests, lint,
//! dependency advisories) itself and records structured results, so whether
//! work is done comes from the checks, not from the agent's own account.
//!
//! Checks are read from `.fermi/checks.toml` at the repository root, or
//! detected from its manifests when there's no such file:
//!
//! ```toml
//! max_repairs = 3            # repair turns a verified task may take
//! protect = ["tests/**"]     # files the agent must leave as they are
//!
//! [[check]]
//! name = "build"
//! kind = "build"             # build | test | lint | audit | other
//! run = "cargo check --workspace --all-targets --message-format=json"
//! mandatory = true           # the gate needs it to pass (the default)
//! timeout_secs = 900
//!
//! [[check]]
//! name = "tests"
//! kind = "test"
//! run = "cargo test --workspace --no-fail-fast --message-format=json"
//! needs = ["build"]          # not run unless build passed
//!
//! [[check]]
//! name = "advisories"
//! kind = "audit"             # no `run`: fermi's own lockfile scanner
//! advisories = "advisories"  # OSV records: a folder of JSON files, or one file
//! ```
//!
//! Every check ends passed, failed, skipped or not_run. The gate passes only
//! when every mandatory check passed, so a check that couldn't run fails it.

use crate::engine::{Engine, Task};
use crate::events::{emit, Event, Tx};
use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;

pub const CONFIG_FILE: &str = ".fermi/checks.toml";
/// Repair turns a verified task gets when the project doesn't say.
pub const DEFAULT_REPAIRS: u32 = 3;
const DEFAULT_TIMEOUT: u64 = 900;
/// Characters of a command's output kept in its result.
const OUTPUT_KEEP: usize = 16_000;
const MAX_DIAGNOSTICS: usize = 200;

// ── configuration ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Build,
    Test,
    Lint,
    Audit,
    #[default]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckDef {
    pub name: String,
    #[serde(default)]
    pub kind: Kind,
    /// Shell command, run in the repository (or `dir`). An audit check
    /// without one uses fermi's own advisory scanner.
    #[serde(default)]
    pub run: Option<String>,
    #[serde(default = "yes")]
    pub mandatory: bool,
    /// Checks that must pass first; otherwise this one is not run.
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Subfolder to run in, relative to the repository.
    #[serde(default)]
    pub dir: Option<String>,
    /// Advisory records (OSV JSON) for the built-in scanner; default `~/.fermi/advisories`.
    #[serde(default)]
    pub advisories: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Checks {
    #[serde(default)]
    pub max_repairs: Option<u32>,
    /// Globs (relative to the repository) of files an agent must not change.
    #[serde(default)]
    pub protect: Vec<String>,
    #[serde(default, rename = "check")]
    pub checks: Vec<CheckDef>,
    /// "file" (`.fermi/checks.toml`) or "detected".
    #[serde(skip)]
    pub source: String,
}

/// The repository's checks: its `.fermi/checks.toml`, or detected ones.
pub fn load(root: &Path) -> Result<Checks> {
    let path = root.join(CONFIG_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(detect(root)) };
    let mut c: Checks = toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?;
    c.source = "file".into();
    for (i, ch) in c.checks.iter().enumerate() {
        if ch.name.trim().is_empty() {
            bail!("{CONFIG_FILE}: a check has no name");
        }
        if c.checks[..i].iter().any(|o| o.name == ch.name) {
            bail!("{CONFIG_FILE}: two checks are named `{}`", ch.name);
        }
        if let Some(n) = ch.needs.iter().find(|n| !c.checks[..i].iter().any(|o| &o.name == *n)) {
            bail!("{CONFIG_FILE}: check `{}` needs `{n}`, which isn't defined before it", ch.name);
        }
        if ch.run.is_none() && ch.kind != Kind::Audit {
            bail!("{CONFIG_FILE}: check `{}` has no `run` command", ch.name);
        }
    }
    Ok(c)
}

/// Checks for a repository without `.fermi/checks.toml`, from its manifests.
pub fn detect(root: &Path) -> Checks {
    let has = |f: &str| root.join(f).is_file();
    let mut checks = vec![];
    let mut add = |name: &str, kind: Kind, run: &str, mandatory: bool, needs: &[&str]| {
        checks.push(CheckDef {
            name: name.into(),
            kind,
            run: Some(run.into()),
            mandatory,
            needs: needs.iter().map(|s| s.to_string()).collect(),
            timeout_secs: None,
            dir: None,
            advisories: None,
        })
    };
    if has("Cargo.toml") {
        add("build", Kind::Build, "cargo check --workspace --all-targets --message-format=json", true, &[]);
        add("tests", Kind::Test, "cargo test --workspace --no-fail-fast --message-format=json", true, &["build"]);
        add("lint", Kind::Lint, "cargo clippy --workspace --all-targets --message-format=json -- -D warnings", false, &["build"]);
    } else if has("package.json") {
        let pkg: Value = std::fs::read(root.join("package.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let script = |k: &str| pkg["scripts"][k].as_str().map(str::to_string);
        let build = script("build").is_some();
        if build {
            add("build", Kind::Build, "npm run build", true, &[]);
        }
        if script("test").is_some_and(|t| !t.contains("no test specified")) {
            add("tests", Kind::Test, "npm test", true, if build { &["build"] } else { &[] });
        }
        if script("lint").is_some() {
            add("lint", Kind::Lint, "npm run lint", false, &[]);
        }
    } else if has("go.mod") {
        add("build", Kind::Build, "go build ./...", true, &[]);
        add("tests", Kind::Test, "go test ./...", true, &["build"]);
        add("lint", Kind::Lint, "go vet ./...", false, &["build"]);
    } else if (has("pyproject.toml") || has("setup.py") || has("requirements.txt"))
        && (root.join("tests").is_dir() || has("pytest.ini") || has("conftest.py"))
    {
        add("tests", Kind::Test, "python -m pytest -q", true, &[]);
    }
    if default_dataset().exists() && LOCKFILES.iter().any(|f| has(f)) {
        checks.push(CheckDef {
            name: "advisories".into(),
            kind: Kind::Audit,
            run: None,
            mandatory: false,
            needs: vec![],
            timeout_secs: None,
            dir: None,
            advisories: None,
        });
    }
    Checks { checks, source: "detected".into(), ..Default::default() }
}

// ── results ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Passed,
    Failed,
    /// Deliberately not applicable (e.g. no lockfile to scan).
    Skipped,
    /// Didn't run (a check it needs didn't pass).
    NotRun,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Skipped => "skipped",
            Status::NotRun => "not run",
        }
    }
}

/// A compiler, linter or type checker message at a place in the code.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub severity: String,
    #[serde(default)]
    pub code: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestFailure {
    pub name: String,
    /// What the test printed (assertion messages, panics), clipped.
    #[serde(default)]
    pub output: String,
}

/// A dependency with a known advisory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub package: String,
    pub version: String,
    pub ecosystem: String,
    pub lockfile: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub fixed: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub kind: Kind,
    pub mandatory: bool,
    pub status: Status,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    pub failed_tests: Vec<TestFailure>,
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// The tool and data a check used (the advisory scanner and its dataset).
    #[serde(default)]
    pub tool: Option<Value>,
    #[serde(default)]
    pub reason: Option<String>,
    /// The end of the command's output.
    #[serde(default)]
    pub output: String,
}

impl CheckResult {
    fn new(def: &CheckDef, status: Status, reason: impl Into<String>) -> Self {
        CheckResult {
            name: def.name.clone(),
            kind: def.kind,
            mandatory: def.mandatory,
            status,
            command: def.run.clone(),
            exit_code: None,
            duration_ms: 0,
            diagnostics: vec![],
            failed_tests: vec![],
            findings: vec![],
            tool: None,
            reason: Some(reason.into()),
            output: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub root: String,
    /// The commit checked out, and whether there were uncommitted changes.
    pub revision: Option<String>,
    pub dirty: bool,
    /// The exact files checked: the commit, plus a hash of uncommitted changes.
    pub fingerprint: String,
    /// "file" (`.fermi/checks.toml`) or "detected".
    pub source: String,
    /// Every mandatory check passed.
    pub passed: bool,
    pub checks: Vec<CheckResult>,
    pub started: u64,
    pub duration_ms: u64,
}

impl Report {
    /// Mandatory checks that didn't pass.
    pub fn blocking(&self) -> Vec<&CheckResult> {
        self.checks.iter().filter(|c| c.mandatory && c.status != Status::Passed).collect()
    }

    /// One line: what passed, or what failed and where.
    pub fn summary(&self) -> String {
        if self.passed {
            return match self.checks.len() {
                0 => "no checks to run".into(),
                n => format!("{n} check{} passed at {}", if n == 1 { "" } else { "s" }, self.fingerprint),
            };
        }
        let parts: Vec<String> = self
            .blocking()
            .iter()
            .map(|c| match &c.reason {
                Some(r) if c.status == Status::Failed => format!("{} ({})", c.name, r),
                _ => format!("{} ({})", c.name, c.status.as_str()),
            })
            .collect();
        format!("checks failed at {}: {}", self.fingerprint, parts.join(", "))
    }

    /// The report with output and long lists cut, to keep in task histories
    /// and team runs.
    pub fn trimmed(&self) -> Report {
        let mut r = self.clone();
        for c in &mut r.checks {
            c.output = clip_tail(&c.output, 1500);
            c.diagnostics.truncate(20);
            c.failed_tests.truncate(10);
            for t in &mut c.failed_tests {
                t.output = clip_tail(&t.output, 800);
            }
        }
        r
    }

    pub fn compact(&self) -> Value {
        serde_json::to_value(self.trimmed()).unwrap_or_default()
    }
}

// ── running checks ──────────────────────────────────────────────────────────

/// Files the agent must not change, as they were before it started.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub globs: Vec<String>,
    pub files: BTreeMap<String, String>,
}

#[derive(Default)]
pub struct RunOpts<'a> {
    /// Only these checks (by name); empty = all.
    pub only: &'a [String],
    /// Fail if protected files changed since this snapshot.
    pub protected: Option<&'a Snapshot>,
    /// Use these checks instead of loading them.
    pub checks: Option<Checks>,
}

/// Runs the checks of the repository containing `dir`.
pub async fn run(dir: &Path, opts: RunOpts<'_>) -> Result<Report> {
    let root = crate::code::repo_root(dir);
    let checks = match opts.checks {
        Some(c) => c,
        None => load(&root)?,
    };
    if let Some(n) = opts.only.iter().find(|n| !checks.checks.iter().any(|c| &&c.name == n)) {
        let names: Vec<&str> = checks.checks.iter().map(|c| c.name.as_str()).collect();
        bail!("no check named `{n}` (checks: {})", if names.is_empty() { "none".into() } else { names.join(", ") });
    }
    let started_at = Instant::now();
    let (revision, dirty, fingerprint) = fingerprint(&root);
    let mut results: Vec<CheckResult> = vec![];
    if let Some(before) = opts.protected.filter(|s| !s.globs.is_empty()) {
        results.push(protected_check(&root, before));
    }
    for def in checks.checks.iter().filter(|c| opts.only.is_empty() || opts.only.contains(&c.name)) {
        // Needs only count checks that are part of this run.
        let unmet: Vec<&String> =
            def.needs.iter().filter(|n| results.iter().any(|r| &&r.name == n && r.status != Status::Passed)).collect();
        if !unmet.is_empty() {
            let names: Vec<&str> = unmet.iter().map(|s| s.as_str()).collect();
            results.push(CheckResult::new(def, Status::NotRun, format!("needs {}, which didn't pass", names.join(", "))));
            continue;
        }
        let r = match (&def.run, def.kind) {
            (None, Kind::Audit) => audit(&root, def),
            (Some(_), _) => exec(&root, def).await,
            (None, _) => CheckResult::new(def, Status::Failed, "no command to run"),
        };
        results.push(r);
    }
    let passed = results.iter().all(|c| !c.mandatory || c.status == Status::Passed);
    Ok(Report {
        root: root.to_string_lossy().replace('\\', "/"),
        revision,
        dirty,
        fingerprint,
        source: checks.source.clone(),
        passed,
        checks: results,
        started: crate::util::now(),
        duration_ms: started_at.elapsed().as_millis() as u64,
    })
}

async fn exec(root: &Path, def: &CheckDef) -> CheckResult {
    let cmdline = def.run.clone().unwrap_or_default();
    let cwd = def.dir.as_ref().map(|d| root.join(d)).unwrap_or_else(|| root.to_path_buf());
    let mut cmd = shell(&cmdline);
    cmd.current_dir(&cwd)
        .env("CARGO_TERM_COLOR", "never")
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return CheckResult::new(def, Status::Failed, format!("couldn't start: {e}")),
    };
    let pid = child.id();
    let (mut out, mut err) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let limit = Duration::from_secs(def.timeout_secs.unwrap_or(DEFAULT_TIMEOUT));
    let waited = tokio::time::timeout(limit, async {
        let (mut a, mut b) = (vec![], vec![]);
        let (_, _, status) = tokio::join!(out.read_to_end(&mut a), err.read_to_end(&mut b), child.wait());
        (a, b, status)
    })
    .await;
    let mut r = CheckResult::new(def, Status::Failed, "");
    r.reason = None;
    r.duration_ms = started.elapsed().as_millis() as u64;
    let Ok((stdout, stderr, status)) = waited else {
        if let Some(pid) = pid {
            crate::providers::proc::kill_tree(pid);
        }
        r.reason = Some(format!("timed out after {}s", limit.as_secs()));
        return r;
    };
    let text = format!("{}{}", String::from_utf8_lossy(&stdout), String::from_utf8_lossy(&stderr));
    let code = status.ok().and_then(|s| s.code());
    r.exit_code = code;
    let (diags, tests) = parse_output(root, &cwd, &text);
    r.diagnostics = diags;
    r.failed_tests = tests;
    r.output = clip_tail(&strip_json_lines(&text), OUTPUT_KEEP);
    let errors = r.diagnostics.iter().filter(|d| d.severity == "error").count();
    let not_found = code == Some(127) || (code == Some(9009) && text.contains("is not recognized"));
    if code == Some(0) {
        r.status = Status::Passed;
    } else {
        r.reason = Some(if not_found {
            "command not found".into()
        } else if !r.failed_tests.is_empty() {
            format!("{} failing test{}", r.failed_tests.len(), if r.failed_tests.len() == 1 { "" } else { "s" })
        } else if errors > 0 {
            format!("{errors} error{}", if errors == 1 { "" } else { "s" })
        } else if !r.diagnostics.is_empty() {
            format!("{} problem{}", r.diagnostics.len(), if r.diagnostics.len() == 1 { "" } else { "s" })
        } else {
            match code {
                Some(c) => format!("exit code {c}"),
                None => "stopped by a signal".into(),
            }
        });
    }
    r
}

fn shell(cmdline: &str) -> tokio::process::Command {
    #[cfg(windows)]
    {
        let mut c = tokio::process::Command::new("cmd");
        c.raw_arg(format!("/D /S /C \"{cmdline}\""));
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(cmdline);
        c
    }
}

// ── output parsing ──────────────────────────────────────────────────────────

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("valid pattern"))
}

const FILE: &str = r"(?P<file>(?:[A-Za-z]:[\\/])?[^\s:()\x22']+\.[A-Za-z0-9]+)";

/// Diagnostics and failing tests found in a command's output.
pub fn parse_output(root: &Path, cwd: &Path, text: &str) -> (Vec<Diagnostic>, Vec<TestFailure>) {
    let mut diags = cargo_messages(root, cwd, text);
    if diags.is_empty() {
        diags = text_diagnostics(root, cwd, text);
    }
    let mut seen = std::collections::HashSet::new();
    diags.retain(|d| seen.insert((d.file.clone(), d.line, d.column, d.code.clone(), d.message.clone())));
    diags.truncate(MAX_DIAGNOSTICS);
    (diags, test_failures(text))
}

/// `--message-format=json` output of cargo (check, build, test, clippy).
fn cargo_messages(root: &Path, cwd: &Path, text: &str) -> Vec<Diagnostic> {
    let mut out = vec![];
    for line in text.lines().map(str::trim).filter(|l| l.starts_with('{')) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v["reason"] != "compiler-message" {
            continue;
        }
        let m = &v["message"];
        let level = m["level"].as_str().unwrap_or("");
        if !matches!(level, "error" | "warning") {
            continue;
        }
        let Some(span) = m["spans"].as_array().and_then(|s| s.iter().find(|s| s["is_primary"] == true)) else { continue };
        out.push(Diagnostic {
            file: rel_path(root, cwd, span["file_name"].as_str().unwrap_or("")),
            line: span["line_start"].as_u64().unwrap_or(0) as u32,
            column: span["column_start"].as_u64().unwrap_or(0) as u32,
            severity: level.into(),
            code: m["code"]["code"].as_str().map(str::to_string),
            message: m["message"].as_str().unwrap_or("").to_string(),
        });
    }
    out
}

/// `file:line:col: severity: message` and the other common text formats
/// (rustc, gcc/clang, go, tsc, eslint unix, ruff, Python tracebacks).
fn text_diagnostics(root: &Path, cwd: &Path, text: &str) -> Vec<Diagnostic> {
    static LOC: OnceLock<Regex> = OnceLock::new();
    static TSC: OnceLock<Regex> = OnceLock::new();
    static HEAD: OnceLock<Regex> = OnceLock::new();
    static ARROW: OnceLock<Regex> = OnceLock::new();
    static PY: OnceLock<Regex> = OnceLock::new();
    static PYERR: OnceLock<Regex> = OnceLock::new();
    static CODE: OnceLock<Regex> = OnceLock::new();
    let loc = re(
        &LOC,
        &format!(
            r"^{FILE}:(?P<line>\d+):(?P<col>\d+):?\s*(?:-\s*)?(?:(?P<sev>error|warning|note|info)(?:\[(?P<code>[^\]]+)\])?:?)?\s*(?P<msg>.*)$"
        ),
    );
    let tsc = re(&TSC, &format!(r"^{FILE}\((?P<line>\d+),(?P<col>\d+)\):\s*(?P<sev>error|warning)\s+(?P<code>[A-Z]+\d+):\s*(?P<msg>.*)$"));
    let head = re(&HEAD, r"^(?P<sev>error|warning)(?:\[(?P<code>[^\]]+)\])?: (?P<msg>.+)$");
    let arrow = re(&ARROW, &format!(r"^\s*--> {FILE}:(?P<line>\d+):(?P<col>\d+)"));
    let py = re(&PY, r#"^\s*File "(?P<file>[^"]+)", line (?P<line>\d+)"#);
    let pyerr = re(&PYERR, r"^(?P<code>[A-Za-z_]\w*(?:Error|Exception|Warning)):?\s*(?P<msg>.*)$");
    let code_at = re(&CODE, r"^(?P<code>[A-Z]{1,4}\d{2,5})\b");
    let lines: Vec<&str> = text.lines().collect();
    let mut out = vec![];
    let mut header: Option<(String, Option<String>, String)> = None;
    let num = |s: &str| s.parse::<u32>().unwrap_or(0);
    for (i, line) in lines.iter().enumerate() {
        if let Some(c) = head.captures(line) {
            header = Some((c["sev"].to_string(), c.name("code").map(|m| m.as_str().to_string()), c["msg"].to_string()));
            continue;
        }
        if let Some(c) = arrow.captures(line) {
            if let Some((sev, code, msg)) = header.take() {
                out.push(Diagnostic {
                    file: rel_path(root, cwd, &c["file"]),
                    line: num(&c["line"]),
                    column: num(&c["col"]),
                    severity: sev,
                    code,
                    message: msg,
                });
            }
            continue;
        }
        if let Some(c) = tsc.captures(line) {
            out.push(Diagnostic {
                file: rel_path(root, cwd, &c["file"]),
                line: num(&c["line"]),
                column: num(&c["col"]),
                severity: c["sev"].to_string(),
                code: Some(c["code"].to_string()),
                message: c["msg"].trim().to_string(),
            });
            continue;
        }
        if let Some(c) = loc.captures(line) {
            let msg = c["msg"].trim().to_string();
            let code = c.name("code").map(|m| m.as_str().to_string()).or_else(|| code_at.captures(&msg).map(|m| m["code"].to_string()));
            out.push(Diagnostic {
                file: rel_path(root, cwd, &c["file"]),
                line: num(&c["line"]),
                column: num(&c["col"]),
                severity: c.name("sev").map(|m| m.as_str()).unwrap_or("error").to_string(),
                code,
                message: msg,
            });
            continue;
        }
        if let Some(c) = py.captures(line) {
            let file = rel_path(root, cwd, &c["file"]);
            // Only frames in the project, with the error a few lines below.
            if file.starts_with("..") || file.starts_with('/') || !root.join(&file).is_file() {
                continue;
            }
            if let Some(e) = lines[i + 1..].iter().take(5).find_map(|l| pyerr.captures(l)) {
                out.push(Diagnostic {
                    file,
                    line: num(&c["line"]),
                    column: 0,
                    severity: "error".into(),
                    code: Some(e["code"].to_string()),
                    message: e["msg"].trim().to_string(),
                });
            }
        }
    }
    out
}

/// Failing tests from libtest (cargo), pytest, go test, TAP (node --test) and jest.
fn test_failures(text: &str) -> Vec<TestFailure> {
    static LIBTEST: OnceLock<Regex> = OnceLock::new();
    static SECTION: OnceLock<Regex> = OnceLock::new();
    static PYTEST: OnceLock<Regex> = OnceLock::new();
    static GO: OnceLock<Regex> = OnceLock::new();
    static TAP: OnceLock<Regex> = OnceLock::new();
    static JEST: OnceLock<Regex> = OnceLock::new();
    let libtest = re(&LIBTEST, r"^test (?P<name>\S+) \.\.\. FAILED\s*$");
    let section = re(&SECTION, r"^---- (?P<name>\S+) stdout ----\s*$");
    let pytest = re(&PYTEST, r"^FAILED (?P<name>\S+)(?: - (?P<msg>.*))?$");
    let go = re(&GO, r"^\s*--- FAIL: (?P<name>\S+)");
    let tap = re(&TAP, r"^not ok \d+ - (?P<name>.+)$");
    let jest = re(&JEST, r"^\s*● (?P<name>.+ › .+)$");
    let mut names: Vec<String> = vec![];
    let mut outputs: BTreeMap<String, String> = BTreeMap::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        if let Some(c) = section.captures(line) {
            let mut body = vec![];
            let mut j = i + 1;
            while j < lines.len()
                && !lines[j].starts_with("---- ")
                && lines[j].trim_end() != "failures:"
                && !lines[j].starts_with("test result:")
            {
                body.push(lines[j]);
                j += 1;
            }
            outputs.insert(c["name"].to_string(), clip(body.join("\n").trim(), 2000));
            i = j;
            continue;
        }
        let hit = libtest
            .captures(line)
            .or_else(|| go.captures(line))
            .or_else(|| tap.captures(line))
            .or_else(|| jest.captures(line))
            .map(|c| (c["name"].to_string(), None))
            .or_else(|| pytest.captures(line).map(|c| (c["name"].to_string(), c.name("msg").map(|m| m.as_str().to_string()))));
        if let Some((name, msg)) = hit {
            if !names.contains(&name) {
                names.push(name.clone());
            }
            if let Some(m) = msg {
                outputs.entry(name).or_insert(m);
            }
        }
        i += 1;
    }
    names.into_iter().map(|n| TestFailure { output: outputs.remove(&n).unwrap_or_default(), name: n }).collect()
}

/// A path from tool output, relative to the repository with `/`.
fn rel_path(root: &Path, cwd: &Path, p: &str) -> String {
    let p = p.trim().trim_start_matches("./");
    let path = Path::new(p);
    let abs = if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) };
    // Keep the case of the names; compare without it (Windows paths).
    let slash = |x: &Path| x.to_string_lossy().replace('\\', "/").trim_start_matches("//?/").trim_end_matches('/').to_string();
    let (a, r) = (slash(&abs), slash(root));
    if a.len() > r.len() && a[..r.len()].eq_ignore_ascii_case(&r) && a.as_bytes()[r.len()] == b'/' {
        return a[r.len() + 1..].to_string();
    }
    if !path.is_absolute() && cwd == root {
        return p.replace('\\', "/");
    }
    a
}

/// Output without cargo's JSON lines, which are only noise to a reader.
fn strip_json_lines(text: &str) -> String {
    let rendered: Vec<String> = text
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            match serde_json::from_str::<Value>(t) {
                // Keep cargo's own rendering of compiler messages.
                Ok(v) if t.starts_with('{') => v["message"]["rendered"].as_str().map(|s| s.trim_end().to_string()),
                _ => Some(l.to_string()),
            }
        })
        .collect();
    rendered.join("\n")
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// The last `max` characters (what failed is usually at the end).
fn clip_tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        format!("…{}", s.chars().skip(n - max).collect::<String>())
    }
}

// ── revision and protected files ────────────────────────────────────────────

fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(root).args(args).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let o = cmd.output().ok()?;
    o.status.success().then_some(o.stdout)
}

/// (commit, dirty, fingerprint). The fingerprint is the short commit, plus a
/// hash of the uncommitted changes when there are any.
pub fn fingerprint(root: &Path) -> (Option<String>, bool, String) {
    let head = git(root, &["rev-parse", "HEAD"]).map(|b| String::from_utf8_lossy(&b).trim().to_string());
    let status = git(root, &["status", "--porcelain", "--untracked-files=all"]).unwrap_or_default();
    let dirty = !status.is_empty();
    let short = head.as_deref().map(|h| h[..h.len().min(12)].to_string()).unwrap_or_else(|| "no-commit".into());
    if !dirty {
        return (head, false, short);
    }
    let mut h = Sha256::new();
    h.update(git(root, &["diff", "HEAD", "--binary"]).unwrap_or_default());
    for line in String::from_utf8_lossy(&status).lines().filter(|l| l.starts_with("?? ")) {
        let rel = line[3..].trim_matches('"');
        h.update(rel.as_bytes());
        h.update(std::fs::read(root.join(rel)).unwrap_or_default());
    }
    let hex = format!("{:x}", h.finalize());
    (head, true, format!("{short}+{}", &hex[..8]))
}

/// Hashes of the files matching `globs`, to tell later whether they changed.
pub fn snapshot(root: &Path, globs: &[String]) -> Result<Snapshot> {
    let mut snap = Snapshot { globs: globs.to_vec(), files: BTreeMap::new() };
    if globs.is_empty() {
        return Ok(snap);
    }
    let mut b = globset::GlobSetBuilder::new();
    for g in globs {
        b.add(globset::Glob::new(g).with_context(|| format!("protect pattern `{g}`"))?);
    }
    let set = b.build()?;
    let walker = ignore::WalkBuilder::new(root).hidden(false).git_ignore(true).parents(true).build();
    for e in walker.filter_map(|e| e.ok()) {
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = e.path().strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel.starts_with(".git/") || !set.is_match(&rel) {
            continue;
        }
        let bytes = std::fs::read(e.path()).unwrap_or_default();
        snap.files.insert(rel, format!("{:x}", Sha256::digest(&bytes)));
    }
    Ok(snap)
}

fn protected_check(root: &Path, before: &Snapshot) -> CheckResult {
    let def = CheckDef {
        name: "protected files".into(),
        kind: Kind::Other,
        run: None,
        mandatory: true,
        needs: vec![],
        timeout_secs: None,
        dir: None,
        advisories: None,
    };
    let after = match snapshot(root, &before.globs) {
        Ok(s) => s,
        Err(e) => return CheckResult::new(&def, Status::Failed, format!("{e:#}")),
    };
    let mut changed = vec![];
    for (f, h) in &before.files {
        match after.files.get(f) {
            None => changed.push(format!("deleted {f}")),
            Some(h2) if h2 != h => changed.push(format!("modified {f}")),
            _ => {}
        }
    }
    changed.extend(after.files.keys().filter(|f| !before.files.contains_key(*f)).map(|f| format!("added {f}")));
    let mut r = CheckResult::new(&def, if changed.is_empty() { Status::Passed } else { Status::Failed }, "");
    r.command = None;
    r.reason = Some(if changed.is_empty() {
        format!("{} protected file(s) unchanged", before.files.len())
    } else {
        format!("changed: {}", changed.join(", "))
    });
    r.tool = Some(json!({ "patterns": before.globs, "files": before.files.len() }));
    r
}

// ── dependency advisories ───────────────────────────────────────────────────

const LOCKFILES: [&str; 3] = ["Cargo.lock", "package-lock.json", "requirements.txt"];

pub fn default_dataset() -> PathBuf {
    crate::config::Config::home().join("advisories")
}

struct Pkg {
    ecosystem: &'static str,
    name: String,
    version: String,
    lockfile: String,
}

/// Packages pinned by the repository's lockfiles (read only; nothing is installed or run).
fn locked_packages(root: &Path) -> (Vec<Pkg>, Vec<String>) {
    let mut pkgs = vec![];
    let mut files = vec![];
    if let Ok(text) = std::fs::read_to_string(root.join("Cargo.lock")) {
        files.push("Cargo.lock".to_string());
        if let Ok(v) = toml::from_str::<toml::Value>(&text) {
            for p in v.get("package").and_then(|p| p.as_array()).into_iter().flatten() {
                // Workspace members have no source: they're the project, not dependencies.
                if p.get("source").is_none() {
                    continue;
                }
                if let (Some(n), Some(ver)) = (p.get("name").and_then(|x| x.as_str()), p.get("version").and_then(|x| x.as_str())) {
                    pkgs.push(Pkg { ecosystem: "crates.io", name: n.into(), version: ver.into(), lockfile: "Cargo.lock".into() });
                }
            }
        }
    }
    if let Some(v) = std::fs::read(root.join("package-lock.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
        files.push("package-lock.json".to_string());
        if let Some(map) = v["packages"].as_object() {
            for (k, p) in map {
                if k.is_empty() || p["link"] == true {
                    continue;
                }
                let name =
                    p["name"].as_str().map(str::to_string).unwrap_or_else(|| k.rsplit("node_modules/").next().unwrap_or(k).to_string());
                if let Some(ver) = p["version"].as_str() {
                    pkgs.push(Pkg { ecosystem: "npm", name, version: ver.into(), lockfile: "package-lock.json".into() });
                }
            }
        } else {
            fn walk(deps: &Value, out: &mut Vec<Pkg>) {
                for (name, d) in deps.as_object().into_iter().flatten() {
                    if let Some(ver) = d["version"].as_str() {
                        out.push(Pkg { ecosystem: "npm", name: name.clone(), version: ver.into(), lockfile: "package-lock.json".into() });
                    }
                    walk(&d["dependencies"], out);
                }
            }
            walk(&v["dependencies"], &mut pkgs);
        }
    }
    if let Ok(text) = std::fs::read_to_string(root.join("requirements.txt")) {
        files.push("requirements.txt".to_string());
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if let Some((name, ver)) = line.split_once("==") {
                let name = name.split('[').next().unwrap_or(name).trim();
                let ver = ver.split(';').next().unwrap_or(ver).trim();
                if !name.is_empty() && !ver.is_empty() {
                    pkgs.push(Pkg { ecosystem: "PyPI", name: name.into(), version: ver.into(), lockfile: "requirements.txt".into() });
                }
            }
        }
    }
    (pkgs, files)
}

/// OSV records from a folder of JSON files (or one file), with a description
/// of the dataset: where it is, how many records, its SHA-256 and newest record.
fn load_dataset(path: &Path) -> Result<(Vec<Value>, Value)> {
    let mut files: Vec<PathBuf> = if path.is_dir() {
        std::fs::read_dir(path)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("json")))
            .collect()
    } else if path.is_file() {
        vec![path.to_path_buf()]
    } else {
        bail!("advisory dataset not found at {}", path.display());
    };
    files.sort();
    if files.is_empty() {
        bail!("no advisory records in {}", path.display());
    }
    let mut h = Sha256::new();
    let mut records = vec![];
    for f in &files {
        let bytes = std::fs::read(f)?;
        h.update(f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default().as_bytes());
        h.update(&bytes);
        let v: Value = serde_json::from_slice(&bytes).with_context(|| format!("reading {}", f.display()))?;
        match v {
            Value::Array(a) => records.extend(a),
            Value::Object(ref o) if o.get("vulns").is_some_and(Value::is_array) => {
                records.extend(o["vulns"].as_array().cloned().unwrap_or_default())
            }
            other => records.push(other),
        }
    }
    let modified = records.iter().filter_map(|r| r["modified"].as_str()).max().map(str::to_string);
    let info = json!({
        "path": crate::util::norm_path(path), "files": files.len(), "records": records.len(),
        "sha256": format!("{:x}", h.finalize()), "modified": modified,
    });
    Ok((records, info))
}

fn audit(root: &Path, def: &CheckDef) -> CheckResult {
    let started = Instant::now();
    let dataset = match &def.advisories {
        Some(p) => {
            let p = crate::util::expand(p);
            if p.is_absolute() {
                p
            } else {
                root.join(p)
            }
        }
        None => default_dataset(),
    };
    let mut r = CheckResult::new(def, Status::Failed, "");
    r.command = Some(format!("fermi advisories {}", crate::util::norm_path(&dataset)));
    let (records, info) = match load_dataset(&dataset) {
        Ok(d) => d,
        Err(e) => {
            r.reason = Some(format!("{e:#}"));
            return r;
        }
    };
    let (pkgs, lockfiles) = locked_packages(root);
    r.tool = Some(json!({
        "scanner": "fermi advisories", "version": env!("CARGO_PKG_VERSION"),
        "dataset": info, "lockfiles": lockfiles, "packages": pkgs.len(),
    }));
    if lockfiles.is_empty() {
        r.status = Status::Skipped;
        r.reason = Some("no lockfile to scan (Cargo.lock, package-lock.json, requirements.txt)".into());
        return r;
    }
    for p in &pkgs {
        for rec in &records {
            if let Some(fixed) = affects(rec, p) {
                r.findings.push(Finding {
                    id: rec["id"].as_str().unwrap_or("?").to_string(),
                    aliases: rec["aliases"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_string)).collect(),
                    package: p.name.clone(),
                    version: p.version.clone(),
                    ecosystem: p.ecosystem.into(),
                    lockfile: p.lockfile.clone(),
                    summary: rec["summary"].as_str().or(rec["details"].as_str()).unwrap_or("").chars().take(300).collect(),
                    fixed,
                });
            }
        }
    }
    r.duration_ms = started.elapsed().as_millis() as u64;
    if r.findings.is_empty() {
        r.status = Status::Passed;
        r.reason = Some(format!("no known advisories for {} locked packages", pkgs.len()));
    } else {
        let ids: Vec<String> = r.findings.iter().map(|f| format!("{} ({} {})", f.id, f.package, f.version)).collect();
        r.reason = Some(format!("{} advisor{}: {}", ids.len(), if ids.len() == 1 { "y" } else { "ies" }, ids.join(", ")));
    }
    r
}

/// Whether an OSV record affects a package: Some(first fixed version, if any).
fn affects(rec: &Value, p: &Pkg) -> Option<Option<String>> {
    let norm = |eco: &str, n: &str| {
        if eco.eq_ignore_ascii_case("pypi") {
            n.to_lowercase().replace(['_', '.'], "-")
        } else {
            n.to_string()
        }
    };
    for a in rec["affected"].as_array().into_iter().flatten() {
        let eco = a["package"]["ecosystem"].as_str().unwrap_or("");
        if !eco.eq_ignore_ascii_case(p.ecosystem) || norm(eco, a["package"]["name"].as_str().unwrap_or("")) != norm(eco, &p.name) {
            continue;
        }
        let fixed_after = || {
            a["ranges"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|r| r["events"].as_array().cloned().unwrap_or_default())
                .filter_map(|e| e["fixed"].as_str().map(str::to_string))
                .filter(|f| cmp_version(f, &p.version).is_gt())
                .min_by(|x, y| cmp_version(x, y))
        };
        if a["versions"].as_array().into_iter().flatten().any(|v| v.as_str() == Some(p.version.as_str())) {
            return Some(fixed_after());
        }
        for range in a["ranges"].as_array().into_iter().flatten() {
            if !matches!(range["type"].as_str(), Some("SEMVER" | "ECOSYSTEM")) {
                continue;
            }
            let mut inside = false;
            for e in range["events"].as_array().into_iter().flatten() {
                if let Some(i) = e["introduced"].as_str() {
                    if i == "0" || cmp_version(&p.version, i).is_ge() {
                        inside = true;
                    }
                }
                if let Some(f) = e["fixed"].as_str() {
                    if cmp_version(&p.version, f).is_ge() {
                        inside = false;
                    }
                }
                if let Some(l) = e["last_affected"].as_str() {
                    if cmp_version(&p.version, l).is_gt() {
                        inside = false;
                    }
                }
            }
            if inside {
                return Some(fixed_after());
            }
        }
    }
    None
}

/// Compares versions like `1.2.10` > `1.2.9`; a pre-release sorts before its release.
pub fn cmp_version(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn parts(s: &str) -> (Vec<&str>, Option<&str>) {
        let s = s.trim().trim_start_matches(['v', '=']);
        let s = s.split('+').next().unwrap_or(s);
        match s.split_once('-') {
            Some((rel, pre)) => (rel.split('.').collect(), Some(pre)),
            None => (s.split('.').collect(), None),
        }
    }
    fn seg(x: &str, y: &str) -> Ordering {
        match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(a), Ok(b)) => a.cmp(&b),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            _ => x.cmp(y),
        }
    }
    let ((ra, pa), (rb, pb)) = (parts(a), parts(b));
    for i in 0..ra.len().max(rb.len()) {
        let o = seg(ra.get(i).copied().unwrap_or("0"), rb.get(i).copied().unwrap_or("0"));
        if o != Ordering::Equal {
            return o;
        }
    }
    match (pa, pb) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let (xs, ys): (Vec<&str>, Vec<&str>) = (x.split('.').collect(), y.split('.').collect());
            for i in 0..xs.len().max(ys.len()) {
                match (xs.get(i), ys.get(i)) {
                    (Some(p), Some(q)) => {
                        let o = seg(p, q);
                        if o != Ordering::Equal {
                            return o;
                        }
                    }
                    (Some(_), None) => return Ordering::Greater,
                    (None, Some(_)) => return Ordering::Less,
                    _ => {}
                }
            }
            Ordering::Equal
        }
    }
}

// ── verified tasks: run, check, repair ──────────────────────────────────────

/// Verification for a task: after the agent finishes, fermi runs the checks
/// and sends failures back to the same conversation, up to `max_repairs` times.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VerifyOpts {
    /// Repair turns; None = the project's `max_repairs`, else 3.
    #[serde(default)]
    pub max_repairs: Option<u32>,
    /// Only these checks; empty = all of them.
    #[serde(default)]
    pub checks: Vec<String>,
}

/// One agent turn, its `Done` held back. Returns (ok, summary, session, (profile, model, account)).
async fn agent_turn(engine: &Engine, task: &Task, tx: &Tx) -> (bool, String, Option<String>, Option<(String, String, String)>) {
    let (itx, mut irx) = tokio::sync::mpsc::unbounded_channel();
    let mut done = (false, String::new());
    let mut session = None;
    let mut ran_on = None;
    let run = async move {
        let _ = engine.run(task, &itx).await;
    };
    let forward = async {
        while let Some(ev) = irx.recv().await {
            match &ev {
                Event::Done { ok, summary } => {
                    done = (*ok, summary.clone());
                    continue;
                }
                Event::Session { id } => session = Some(id.clone()),
                Event::Routed { profile, model, account, .. } => ran_on = Some((profile.clone(), model.clone(), account.clone())),
                _ => {}
            }
            emit(tx, ev);
        }
    };
    tokio::join!(run, forward);
    (done.0, done.1, session, ran_on)
}

/// Runs a task, then its checks; failing checks go back to the agent until
/// they pass or the repair turns run out. Emits exactly one `Done`: ok only
/// when every mandatory check passed.
pub async fn run_task(engine: &Engine, task: Task, opts: &VerifyOpts, tx: &Tx) {
    let root = crate::code::repo_root(&task.cwd);
    let checks = match load(&root) {
        Ok(c) => c,
        Err(e) => return emit(tx, Event::Done { ok: false, summary: format!("can't verify: {e:#}") }),
    };
    let max = opts.max_repairs.or(checks.max_repairs).unwrap_or(DEFAULT_REPAIRS);
    let guard = match snapshot(&root, &checks.protect) {
        Ok(s) => s,
        Err(e) => return emit(tx, Event::Done { ok: false, summary: format!("can't verify: {e:#}") }),
    };
    let original = task.prompt.clone();
    let mut task = task;
    let mut attempt = 0;
    loop {
        let (ok, summary, session, ran_on) = agent_turn(engine, &task, tx).await;
        if !ok {
            return emit(tx, Event::Done { ok: false, summary });
        }
        emit(tx, Event::Log { line: "fermi is running the checks…".into() });
        let report = match run(&task.cwd, RunOpts { only: &opts.checks, protected: Some(&guard), checks: Some(checks.clone()) }).await {
            Ok(r) => r,
            Err(e) => return emit(tx, Event::Done { ok: false, summary: format!("the checks couldn't run: {e:#}") }),
        };
        emit(tx, Event::Verify { attempt, passed: report.passed, report: report.compact() });
        if report.passed {
            return emit(tx, Event::Done { ok: true, summary: format!("{summary} · {}", report.summary()) });
        }
        if attempt >= max {
            let tries = if max == 0 {
                "no repairs allowed".to_string()
            } else {
                format!("after {attempt} repair attempt{}", if attempt == 1 { "" } else { "s" })
            };
            return emit(tx, Event::Done { ok: false, summary: format!("{} ({tries})", report.summary()) });
        }
        attempt += 1;
        let prompt = repair_prompt(&report, attempt, max);
        emit(tx, Event::Repair { attempt, max, prompt: prompt.clone() });
        task = repair_task(&task, &original, prompt, session, ran_on);
    }
}

/// The next turn: the same conversation when there's a session to resume.
fn repair_task(prev: &Task, original: &str, prompt: String, session: Option<String>, ran_on: Option<(String, String, String)>) -> Task {
    let mut t = Task::new(prompt, prev.cwd.clone());
    t.effort = prev.effort.clone();
    t.permission = prev.permission.clone();
    t.allowed = prev.allowed.clone();
    t.team = prev.team.clone();
    match (session, ran_on) {
        (Some(s), Some((profile, model, account))) => {
            t.profile = Some(profile);
            t.model = Some(model);
            t.account = Some(account);
            t.resume = Some(s);
        }
        (_, on) => {
            t.profile = on.as_ref().map(|o| o.0.clone()).or_else(|| prev.profile.clone());
            t.model = on.map(|o| o.1).or_else(|| prev.model.clone());
            t.context = Some(format!("You were asked:\n{original}\n\nYou made changes for it, but the project's checks failed."));
        }
    }
    t
}

/// What the agent is told when checks fail.
pub fn repair_prompt(r: &Report, attempt: u32, max: u32) -> String {
    let mut s = format!(
        "fermi ran the project's checks on your changes and they failed (repair attempt {attempt} of {max}). \
         Fix the code so they pass, then stop: fermi runs the checks again. Don't weaken or delete tests or checks \
         to make them pass unless the task asked for that.\n"
    );
    s += &text(r);
    s
}

/// A report as text, failures first (for agents, the CLI and repair prompts).
pub fn text(r: &Report) -> String {
    let mut s = format!(
        "{} · {}{}\n",
        if r.passed { "PASSED" } else { "FAILED" },
        r.fingerprint,
        if r.source == "detected" { " · checks detected from the project's manifests" } else { "" }
    );
    for c in &r.checks {
        s += &format!(
            "\n## {} [{}{}]: {}{}\n",
            c.name,
            serde_json::to_value(c.kind).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(),
            if c.mandatory { "" } else { ", optional" },
            c.status.as_str(),
            c.reason.as_ref().map(|x| format!(" — {x}")).unwrap_or_default()
        );
        if c.status == Status::Passed {
            continue;
        }
        if let Some(cmd) = &c.command {
            s += &format!("command: {cmd}{}\n", c.exit_code.map(|e| format!(" (exit {e})")).unwrap_or_default());
        }
        for d in
            c.diagnostics.iter().filter(|d| d.severity == "error").chain(c.diagnostics.iter().filter(|d| d.severity != "error")).take(25)
        {
            s += &format!(
                "- {}:{}:{}: {}{}: {}\n",
                d.file,
                d.line,
                d.column,
                d.severity,
                d.code.as_ref().map(|c| format!("[{c}]")).unwrap_or_default(),
                d.message
            );
        }
        for t in c.failed_tests.iter().take(10) {
            s += &format!("- failing test {}\n", t.name);
            if !t.output.is_empty() {
                s += &format!("{}\n", clip(&t.output, 1500).lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n"));
            }
        }
        for f in &c.findings {
            s += &format!(
                "- {} in {} {} ({}){}: {}\n",
                f.id,
                f.package,
                f.version,
                f.lockfile,
                f.fixed.as_ref().map(|x| format!(", fixed in {x}")).unwrap_or_default(),
                f.summary
            );
        }
        if c.diagnostics.is_empty() && c.failed_tests.is_empty() && c.findings.is_empty() && !c.output.trim().is_empty() {
            s += &format!("output (end):\n{}\n", clip_tail(c.output.trim(), 3000));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        use std::cmp::Ordering::*;
        assert_eq!(cmp_version("1.2.10", "1.2.9"), Greater);
        assert_eq!(cmp_version("1.2", "1.2.0"), Equal);
        assert_eq!(cmp_version("1.0.0-alpha", "1.0.0"), Less);
        assert_eq!(cmp_version("v2.0.0", "1.9.9"), Greater);
    }

    #[test]
    fn diagnostics_from_text_and_cargo_json() {
        let root = Path::new("/repo");
        let json = r#"{"reason":"compiler-message","message":{"level":"error","message":"expected `;`","code":null,"spans":[{"file_name":"src/lib.rs","line_start":3,"column_start":14,"is_primary":true}]}}"#;
        let (d, _) = parse_output(root, root, json);
        assert_eq!(
            d,
            vec![Diagnostic {
                file: "src/lib.rs".into(),
                line: 3,
                column: 14,
                severity: "error".into(),
                code: None,
                message: "expected `;`".into()
            }]
        );
        let (d, _) = parse_output(root, root, "src/a.ts(4,7): error TS2322: Type 'x' is not assignable\nmain.go:9:2: undefined: foo\n");
        assert_eq!((d[0].file.as_str(), d[0].line, d[0].code.as_deref()), ("src/a.ts", 4, Some("TS2322")));
        assert_eq!((d[1].file.as_str(), d[1].line, d[1].column), ("main.go", 9, 2));
        let (d, _) = parse_output(root, root, "error[E0425]: cannot find value `x`\n  --> src/main.rs:2:5\n");
        assert_eq!((d[0].line, d[0].code.as_deref()), (2, Some("E0425")));
    }

    #[test]
    fn failing_tests_with_their_output() {
        let out = "running 2 tests\ntest a::ok ... ok\ntest a::bad ... FAILED\n\nfailures:\n\n---- a::bad stdout ----\nthread 'a::bad' panicked at src/a.rs:9:5:\nboom\n\nfailures:\n    a::bad\n\ntest result: FAILED. 1 passed; 1 failed\n";
        let t = test_failures(out);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].name, "a::bad");
        assert!(t[0].output.contains("boom"));
        let t = test_failures("FAILED tests/test_x.py::test_y - assert 1 == 2\n--- FAIL: TestZ (0.00s)\n");
        assert_eq!(t.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["tests/test_x.py::test_y", "TestZ"]);
    }

    #[test]
    fn osv_ranges_and_versions() {
        let rec = json!({ "id": "X-1", "affected": [{ "package": { "ecosystem": "crates.io", "name": "smallvec" },
            "ranges": [{ "type": "SEMVER", "events": [{ "introduced": "1.0.0" }, { "fixed": "1.6.1" }, { "introduced": "2.0.0" }, { "fixed": "2.1.0" }] }] }] });
        let p = |v: &str| Pkg { ecosystem: "crates.io", name: "smallvec".into(), version: v.into(), lockfile: "Cargo.lock".into() };
        assert_eq!(affects(&rec, &p("1.6.0")), Some(Some("1.6.1".into())));
        assert_eq!(affects(&rec, &p("1.6.1")), None);
        assert_eq!(affects(&rec, &p("1.9.0")), None);
        assert!(affects(&rec, &p("2.0.5")).is_some());
        let exact = json!({ "id": "X-2", "affected": [{ "package": { "ecosystem": "PyPI", "name": "Py_Yaml" }, "versions": ["5.3"] }] });
        let py = Pkg { ecosystem: "PyPI", name: "py-yaml".into(), version: "5.3".into(), lockfile: "requirements.txt".into() };
        assert!(affects(&exact, &py).is_some());
    }
}
