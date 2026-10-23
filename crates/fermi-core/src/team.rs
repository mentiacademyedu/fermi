//! Team runs: several agents with roles work on one goal in a git repository.
//!
//! 1. The architect studies the repo and submits a plan: tasks with a role,
//!    dependencies, file scope and acceptance criteria. The user reviews it.
//! 2. Each task runs as its own agent conversation in its own git worktree
//!    and branch, started from the base commit plus the branches it depends
//!    on, and ends with a structured handoff that its dependents receive.
//! 3. Finished branches are merged in dependency order into an integration
//!    branch (a debugger agent resolves conflicts), then a tester and a
//!    security reviewer check the result there.
//! 4. Nothing touches the user's branch until they approve the merge.
//!
//! Agents are ordinary hub tasks (live output, approvals, model choice), so
//! the orchestrator only reacts to their `Done` events.

use crate::engine::{Task, TeamRef};
use crate::events::Event;
use crate::hub::{Hub, TaskStatus as HubStatus};
use crate::util::now;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// The most tasks a plan may have.
const MAX_TASKS: usize = 12;
/// Marker the MCP bridge looks for in an agent's folder (see `mcp.rs`).
pub const MARKER: &str = ".fermi-team.json";
/// Where agents without the bridge write their handoff.
pub const HANDOFF_FILE: &str = ".fermi-handoff.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Architect,
    Developer,
    Tester,
    Security,
    Debugger,
    Researcher,
}

pub const ROLES: [Role; 6] = [Role::Architect, Role::Developer, Role::Tester, Role::Security, Role::Debugger, Role::Researcher];

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Architect => "architect",
            Role::Developer => "developer",
            Role::Tester => "tester",
            Role::Security => "security",
            Role::Debugger => "debugger",
            Role::Researcher => "researcher",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Role::Architect => "Architect",
            Role::Developer => "Developer",
            Role::Tester => "Tester",
            Role::Security => "Security reviewer",
            Role::Debugger => "Debugger",
            Role::Researcher => "Researcher",
        }
    }
    pub fn parse(s: &str) -> Option<Role> {
        let s = s.trim().to_lowercase();
        Some(match s.as_str() {
            "architect" | "designer" | "planner" => Role::Architect,
            "developer" | "dev" | "engineer" | "coder" | "implementer" => Role::Developer,
            "tester" | "qa" | "test" | "testing" => Role::Tester,
            "security" | "security reviewer" | "security_reviewer" | "reviewer" | "auditor" => Role::Security,
            "debugger" | "fixer" => Role::Debugger,
            "researcher" | "research" | "analyst" => Role::Researcher,
            _ => return None,
        })
    }
    /// What this role is expected to do, for its brief.
    fn duty(self) -> &'static str {
        match self {
            Role::Architect => "You design: write the design docs, interfaces, types or scaffolding the task asks for, so the developers can build on it. Keep it concrete and small.",
            Role::Developer => "You implement the task: write the code, keep to the existing style, and build or run it if you can to check it works.",
            Role::Tester => "You test: write or extend automated tests for the work you depend on, run the test suite, and fix the tests (or report real bugs in your handoff). Put the commands you ran and their results in the handoff's `tests`.",
            Role::Security => "You review for security: read the changes (git diff against the base), look for injection, auth and secret handling, unsafe input, dependency and permission problems. Fix clear problems; list the rest as risks.",
            Role::Debugger => "You debug: reproduce the problem, find the root cause, fix it, and confirm the fix.",
            Role::Researcher => "You research: investigate the question (the codebase, docs, APIs, options), and write your findings into the handoff, with a recommendation. Change code only if the task says so; you may add a short notes file if it helps the others.",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    /// The architect is writing the plan.
    Planning,
    /// The plan waits for the user's approval.
    Review,
    Running,
    /// Merging the branches.
    Integrating,
    /// Final tests and security review on the integrated branch.
    Checking,
    /// Waiting for the user to merge.
    Ready,
    Merged,
    Discarded,
    Cancelled,
    /// Something needs the user (a failed task, a merge problem).
    Attention,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Running,
    /// The agent is blocked on an approval in its conversation.
    Waiting,
    Done,
    Failed,
    Skipped,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// A task from the plan, in its own worktree.
    #[default]
    Plan,
    /// Resolving a merge conflict in the integration worktree.
    Merge,
    /// A final check in the integration worktree.
    Check,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Handoff {
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub files_changed: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    #[serde(default)]
    pub risks: Vec<String>,
    #[serde(default)]
    pub next_steps: Vec<String>,
    #[serde(default)]
    pub tests: Option<String>,
}

fn pending() -> TaskState {
    TaskState::Pending
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamTask {
    pub key: String,
    pub role: Role,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub done_when: String,
    #[serde(default)]
    pub stage: Stage,
    #[serde(default = "pending")]
    pub state: TaskState,
    #[serde(default)]
    pub task_id: Option<u64>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub worktree: Option<PathBuf>,
    /// The commit its branch started from (its changes are start..branch).
    #[serde(default)]
    pub start: Option<String>,
    #[serde(default)]
    pub handoff: Option<Handoff>,
    #[serde(default)]
    pub changed: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Checks {
    #[serde(default)]
    pub tests: bool,
    #[serde(default)]
    pub security: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Integration {
    pub branch: String,
    pub worktree: PathBuf,
    #[serde(default)]
    pub merged: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamRun {
    pub id: u64,
    pub goal: String,
    /// The repository's top folder.
    pub repo: PathBuf,
    /// Base commit every branch starts from.
    pub base: String,
    /// The branch checked out when the run started; the merge goes there.
    pub base_branch: Option<String>,
    pub profile: Option<String>,
    /// Model per role (`claude:opus`…); missing = routed.
    #[serde(default)]
    pub models: BTreeMap<String, String>,
    #[serde(default)]
    pub permission: Option<String>,
    pub parallel: usize,
    #[serde(default)]
    pub checks: Checks,
    pub status: RunStatus,
    #[serde(default)]
    pub plan_summary: String,
    #[serde(default)]
    pub tasks: Vec<TeamTask>,
    /// The architect's planning conversation.
    #[serde(default)]
    pub planner: Option<u64>,
    #[serde(default)]
    pub integration: Option<Integration>,
    #[serde(default)]
    pub log: Vec<(u64, String)>,
    #[serde(default)]
    pub error: Option<String>,
    pub created: u64,
    pub updated: u64,
}

impl TeamRun {
    fn short(&self) -> String {
        let s = self.id.to_string();
        s[s.len().saturating_sub(6)..].to_string()
    }
    fn note(&mut self, line: impl Into<String>) {
        self.log.push((now(), line.into()));
        if self.log.len() > 300 {
            self.log.drain(..self.log.len() - 300);
        }
    }
    fn task(&self, key: &str) -> Option<&TeamTask> {
        self.tasks.iter().find(|t| t.key == key)
    }
    fn task_mut(&mut self, key: &str) -> Option<&mut TeamTask> {
        self.tasks.iter_mut().find(|t| t.key == key)
    }
    fn dir(&self, home: &Path) -> PathBuf {
        home.join("teams").join(self.id.to_string())
    }
}

// ── git ─────────────────────────────────────────────────────────────────────

fn git_out(dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.output().context("running git (is it installed?)")
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = git_out(dir, args)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = if err.trim().is_empty() { String::from_utf8_lossy(&out.stdout).into_owned() } else { err.into_owned() };
        bail!("git {}: {}", args.first().copied().unwrap_or(""), msg.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Git with an identity for commits and merges (the user's, else fermi's).
fn git_commit_args(dir: &Path, args: &[&str]) -> Result<String> {
    let has_identity = git(dir, &["config", "user.email"]).is_ok_and(|e| !e.is_empty());
    let mut all: Vec<&str> = vec![];
    if !has_identity {
        all.extend(["-c", "user.name=fermi", "-c", "user.email=fermi@localhost"]);
    }
    all.extend(args);
    git(dir, &all)
}

fn unmerged(dir: &Path) -> Vec<String> {
    git(dir, &["diff", "--name-only", "--diff-filter=U"]).map(|s| s.lines().map(String::from).collect()).unwrap_or_default()
}

/// Build output agents leave behind when they run code, which `git add -A`
/// would otherwise commit (only untracked files are affected).
const JUNK: [&str; 9] = ["__pycache__/", "*.pyc", "*.pyo", ".pytest_cache/", ".mypy_cache/", ".ruff_cache/", "node_modules/", ".DS_Store", "Thumbs.db"];

/// Keeps fermi's marker files and build junk out of every worktree's
/// `git status` (the repo's local `.git/info/exclude`, never committed).
fn exclude_markers(repo: &Path) -> Result<()> {
    let common = git(repo, &["rev-parse", "--git-common-dir"])?;
    let common = if Path::new(&common).is_absolute() { PathBuf::from(common) } else { repo.join(common) };
    let file = common.join("info").join("exclude");
    let current = std::fs::read_to_string(&file).unwrap_or_default();
    let mut add = String::new();
    if !current.contains("# fermi team runs") {
        add.push_str("# fermi team runs\n");
    }
    for m in [MARKER, HANDOFF_FILE].into_iter().chain(JUNK) {
        if !current.lines().any(|l| l.trim() == m) {
            add.push_str(m);
            add.push('\n');
        }
    }
    if !add.is_empty() {
        std::fs::create_dir_all(file.parent().unwrap())?;
        let sep = if current.is_empty() || current.ends_with('\n') { "" } else { "\n" };
        std::fs::write(&file, format!("{current}{sep}{add}"))?;
    }
    Ok(())
}

fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 28 {
            break;
        }
    }
    out.trim_end_matches('-').to_string()
}

// ── plans ───────────────────────────────────────────────────────────────────

/// Plan tasks in an order where every task comes after what it depends on.
pub fn topo(tasks: &[TeamTask]) -> Result<Vec<String>> {
    let keys: HashSet<&str> = tasks.iter().map(|t| t.key.as_str()).collect();
    let mut done: Vec<String> = vec![];
    let mut left: Vec<&TeamTask> = tasks.iter().collect();
    while !left.is_empty() {
        let before = left.len();
        left.retain(|t| {
            let ready = t.depends_on.iter().all(|d| done.contains(d) || !keys.contains(d.as_str()));
            if ready {
                done.push(t.key.clone());
            }
            !ready
        });
        if left.len() == before {
            bail!("the plan's dependencies go in a circle ({})", left.iter().map(|t| t.key.as_str()).collect::<Vec<_>>().join(", "));
        }
    }
    Ok(done)
}

/// Reads a plan from the architect's JSON (`{summary, tasks: [...]}`).
pub fn parse_plan(v: &Value) -> Result<(String, Vec<TeamTask>)> {
    let summary = v["summary"].as_str().unwrap_or_default().to_string();
    let Some(items) = v["tasks"].as_array().filter(|a| !a.is_empty()) else { bail!("the plan has no tasks") };
    if items.len() > MAX_TASKS {
        bail!("the plan has {} tasks; {MAX_TASKS} at most", items.len());
    }
    let mut tasks = vec![];
    for (n, it) in items.iter().enumerate() {
        let key = it["id"].as_str().or(it["key"].as_str()).map(slug).filter(|k| !k.is_empty()).unwrap_or_else(|| format!("t{}", n + 1));
        let role_s = it["role"].as_str().unwrap_or("developer");
        let role = Role::parse(role_s).with_context(|| format!("task {key}: unknown role `{role_s}`"))?;
        let description = it["description"].as_str().unwrap_or_default().to_string();
        let title = it["title"].as_str().map(String::from).unwrap_or_else(|| description.chars().take(60).collect());
        let list = |k: &str| -> Vec<String> { it[k].as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect() };
        tasks.push(TeamTask {
            key,
            role,
            title,
            description,
            depends_on: list("depends_on").into_iter().map(|d| slug(&d)).collect(),
            files: list("files"),
            done_when: it["done_when"].as_str().unwrap_or_default().to_string(),
            stage: Stage::Plan,
            state: TaskState::Pending,
            task_id: None,
            branch: None,
            worktree: None,
            start: None,
            handoff: None,
            changed: vec![],
            note: None,
        });
    }
    let mut seen = HashSet::new();
    for t in &tasks {
        if !seen.insert(t.key.clone()) {
            bail!("two tasks are called {}", t.key);
        }
    }
    for t in &tasks {
        if let Some(d) = t.depends_on.iter().find(|d| !seen.contains(*d)) {
            bail!("task {} depends on an unknown task {d}", t.key);
        }
    }
    topo(&tasks)?;
    Ok((summary, tasks))
}

/// The last JSON object in a reply (a ```json block, or the last {...}).
fn json_in_text(text: &str) -> Option<Value> {
    if let Some(start) = text.rfind("```json") {
        let rest = &text[start + 7..];
        if let Some(end) = rest.find("```") {
            if let Ok(v) = serde_json::from_str(rest[..end].trim()) {
                return Some(v);
            }
        }
    }
    let first = text.find('{')?;
    let last = text.rfind('}')?;
    serde_json::from_str(&text[first..=last]).ok()
}

fn last_text(hub: &Hub, id: u64) -> String {
    hub.get(id)
        .and_then(|t| t.events.iter().rev().find_map(|e| match e {
            Event::Text { text } => Some(text.clone()),
            _ => None,
        }))
        .unwrap_or_default()
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

// ── briefs ──────────────────────────────────────────────────────────────────

const HANDOFF_SHAPE: &str = r#"{"summary": "what you did, in a few sentences", "files_changed": ["path", ...], "decisions": ["choices you made and why"], "risks": ["what could be wrong or is left open"], "next_steps": ["what the next agent should know or do"], "tests": "commands you ran and their results, if any"}"#;

fn planner_brief(run: &TeamRun) -> String {
    format!(
        "[You are the architect of a team of AI agents working on this repository. Don't change any files: study the code \
         and plan the work.\n\n\
         Each task you plan runs as a separate agent in its own git worktree and branch. A task starts from the base commit \
         plus the branches of the tasks it depends on; independent tasks run in parallel; fermi merges all branches at the end. \
         Roles you can assign:\n\
         - architect: design docs, interfaces, scaffolding others build on\n\
         - developer: implements code\n\
         - tester: writes and runs tests for other tasks' work (depend on them)\n\
         - security: reviews other tasks' changes for security problems (depend on them)\n\
         - debugger: fixes a known bug or failure\n\
         - researcher: investigates options, docs or APIs and reports findings\n\n\
         Make small tasks with clear file scopes so parallel tasks don't edit the same files; add dependencies where a task \
         needs another's output; write acceptance criteria in done_when. Usually 2 to 7 tasks.{checks}\n\n\
         Submit the plan with the fermi submit_plan tool. If you don't have that tool, end your reply with the plan as a \
         ```json code block of this shape:\n\
         {{\"summary\": \"the approach in a few sentences\", \"tasks\": [{{\"id\": \"t1\", \"role\": \"developer\", \"title\": \"short title\", \
         \"description\": \"what to do, with enough context to do it alone\", \"depends_on\": [], \"files\": [\"paths it may change\"], \
         \"done_when\": \"acceptance criteria\"}}]}}]\n\nThe goal:",
        checks = match (run.checks.tests, run.checks.security) {
            (true, true) => " After the merge, fermi itself runs a final test pass and a security review, so plan those only when a task needs them earlier.",
            (true, false) => " After the merge, fermi itself runs a final test pass.",
            (false, true) => " After the merge, fermi itself runs a final security review.",
            _ => "",
        }
    )
}

fn task_brief(run: &TeamRun, t: &TeamTask, conflicts: &[String]) -> String {
    let mut b = format!(
        "[You are the {role} on a team of AI agents. {duty}\n\n\
         You work in your own git worktree on branch {branch}; other agents work in parallel on other branches and fermi \
         merges them at the end. Stay within your task{scope}. Don't commit, push or switch branches: fermi commits your work.\n\n\
         When you're done, call the fermi handoff tool. If you don't have it, write this JSON to {HANDOFF_FILE} in the working folder:\n{HANDOFF_SHAPE}\n\n\
         The team's goal: {goal}",
        role = t.role.title().to_lowercase(),
        duty = t.role.duty(),
        branch = t.branch.as_deref().unwrap_or("?"),
        scope = if t.files.is_empty() { String::new() } else { format!(" and its files ({})", t.files.join(", ")) },
        goal = run.goal,
    );
    if !run.plan_summary.is_empty() {
        b += &format!("\nThe plan: {}", run.plan_summary);
    }
    if !t.done_when.is_empty() {
        b += &format!("\nYour task is done when: {}", t.done_when);
    }
    let deps: Vec<&TeamTask> = t.depends_on.iter().filter_map(|d| run.task(d)).collect();
    if !deps.is_empty() {
        b += "\n\nWork your branch already includes, with the agents' handoffs:";
        for d in deps {
            let h = d.handoff.clone().unwrap_or_default();
            b += &format!("\n- {} ({}): {}", d.title, d.role.title(), clip(&h.summary, 1500));
            if !d.changed.is_empty() {
                b += &format!(" Files: {}.", d.changed.iter().take(30).cloned().collect::<Vec<_>>().join(", "));
            }
            for (label, items) in [("Decisions", &h.decisions), ("Risks", &h.risks), ("For you", &h.next_steps)] {
                if !items.is_empty() {
                    b += &format!(" {label}: {}.", items.join("; "));
                }
            }
            if let Some(tests) = h.tests.as_ref().filter(|x| !x.is_empty()) {
                b += &format!(" Tests: {}", clip(tests, 600));
            }
        }
    }
    if !conflicts.is_empty() {
        b += &format!(
            "\n\nNote: the branches {} conflicted with the rest and aren't merged into yours yet. Run `git merge {}` yourself and resolve the conflicts first.",
            conflicts.join(", "),
            conflicts.join(" ")
        );
    }
    b += "]\n\nYour task:";
    b
}

// ── the orchestrator ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct NewRun {
    pub goal: String,
    pub repo: PathBuf,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub models: BTreeMap<String, String>,
    #[serde(default)]
    pub permission: Option<String>,
    #[serde(default)]
    pub parallel: Option<usize>,
    #[serde(default)]
    pub checks: Option<Checks>,
}

pub struct Teams {
    hub: Arc<Hub>,
    home: PathBuf,
    runs: Mutex<BTreeMap<u64, TeamRun>>,
}

impl Teams {
    pub fn new(hub: Arc<Hub>) -> Arc<Self> {
        let home = hub.engine().home.clone();
        let dir = home.join("teams");
        let mut runs = BTreeMap::new();
        for e in std::fs::read_dir(&dir).into_iter().flatten().filter_map(|e| e.ok()) {
            let p = e.path().join("run.json");
            let Ok(mut run) = std::fs::read(&p).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice::<TeamRun>(&b)?)) else { continue };
            // A run that was going when fermi closed: its agents stopped with it.
            // (Unless another fermi window is running it: its agents are alive.)
            let agents: Vec<u64> = run.tasks.iter().filter_map(|t| t.task_id).chain(run.planner).collect();
            let elsewhere = agents.iter().any(|id| hub.get(*id).is_some_and(|t| t.status == HubStatus::Running));
            if !elsewhere && matches!(run.status, RunStatus::Planning | RunStatus::Running | RunStatus::Integrating | RunStatus::Checking) {
                for t in run.tasks.iter_mut().filter(|t| matches!(t.state, TaskState::Running | TaskState::Waiting)) {
                    t.state = TaskState::Interrupted;
                }
                run.error = Some("fermi was closed while this ran. Resume to continue.".into());
                run.status = RunStatus::Attention;
            }
            runs.insert(run.id, run);
        }
        let teams = Arc::new(Teams { hub: hub.clone(), home, runs: Mutex::new(runs) });
        // Agents finishing (or stopping for an approval) move the runs on.
        let weak = Arc::downgrade(&teams);
        let mut rx = hub.subscribe();
        tokio::spawn(async move {
            loop {
                let msg = match rx.recv().await {
                    Ok(m) => m,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                };
                if msg["kind"] != "task_event" || msg["event"]["type"] != "done" {
                    continue;
                }
                let (Some(teams), Some(id)) = (weak.upgrade(), msg["id"].as_u64()) else { continue };
                tokio::task::spawn_blocking(move || teams.on_done(id));
            }
        });
        teams
    }

    pub fn list(&self) -> Vec<TeamRun> {
        self.runs.lock().unwrap().values().rev().cloned().collect()
    }

    pub fn get(&self, id: u64) -> Option<TeamRun> {
        self.runs.lock().unwrap().get(&id).cloned()
    }

    fn save(&self, run: &mut TeamRun) {
        run.updated = now();
        let dir = run.dir(&self.home);
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(b) = serde_json::to_vec_pretty(run) {
            let tmp = dir.join("run.json.tmp");
            if std::fs::write(&tmp, b).is_ok() {
                let _ = std::fs::rename(&tmp, dir.join("run.json"));
            }
        }
        self.hub.publish(json!({ "kind": "team", "run": run }));
    }

    /// Runs `f` on a run and saves it.
    fn with<T>(&self, id: u64, f: impl FnOnce(&mut TeamRun) -> Result<T>) -> Result<T> {
        let mut runs = self.runs.lock().unwrap();
        let run = runs.get_mut(&id).context("no such team run")?;
        let out = f(run);
        self.save(run);
        out
    }

    pub fn create(&self, req: NewRun) -> Result<u64> {
        let goal = req.goal.trim().to_string();
        if goal.is_empty() {
            bail!("describe the goal first");
        }
        let repo = PathBuf::from(git(&req.repo, &["rev-parse", "--show-toplevel"]).context("that folder isn't a git repository")?);
        let base = git(&repo, &["rev-parse", "HEAD"]).context("the repository has no commits yet; commit something first")?;
        let branch = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]).ok().filter(|b| b != "HEAD");
        exclude_markers(&repo)?;
        let id = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let mut run = TeamRun {
            id,
            goal,
            repo,
            base,
            base_branch: branch,
            profile: req.profile.filter(|p| !p.is_empty()),
            models: req.models.into_iter().filter(|(k, v)| Role::parse(k).is_some() && !v.is_empty()).collect(),
            permission: req.permission.filter(|p| !p.is_empty()),
            parallel: req.parallel.unwrap_or(3).clamp(1, 6),
            checks: req.checks.unwrap_or(Checks { tests: true, security: true }),
            status: RunStatus::Planning,
            plan_summary: String::new(),
            tasks: vec![],
            planner: None,
            integration: None,
            log: vec![],
            error: None,
            created: now(),
            updated: now(),
        };
        let dirty = git(&run.repo, &["status", "--porcelain", "--untracked-files=no"]).unwrap_or_default();
        if !dirty.is_empty() {
            run.note("Your uncommitted changes aren't part of this run: the agents start from the last commit.");
        }
        self.start_planner(&mut run)?;
        let mut runs = self.runs.lock().unwrap();
        self.save(&mut run);
        runs.insert(id, run);
        Ok(id)
    }

    fn start_planner(&self, run: &mut TeamRun) -> Result<()> {
        let dir = run.dir(&self.home);
        let wt = dir.join("wt").join("plan");
        if !wt.exists() {
            git(&run.repo, &["worktree", "add", "--detach", &wt.to_string_lossy(), &run.base])?;
        }
        let plan_file = dir.join("plan.json");
        let _ = std::fs::remove_file(&plan_file);
        write_marker(&wt, run.id, "plan", Role::Architect, &dir.join("plan.handoff.json"), Some(&plan_file))?;
        let mut task = Task::new(run.goal.clone(), wt);
        task.context = Some(planner_brief(run));
        task.profile = run.profile.clone();
        task.model = run.models.get("architect").cloned();
        task.permission = Some("default".into()); // reading only: edits would need approval
        task.team = Some(TeamRef { run: run.id, key: "plan".into(), role: "architect".into() });
        run.planner = Some(self.hub.submit(task));
        run.status = RunStatus::Planning;
        run.error = None;
        run.note("The architect is studying the repository and planning.");
        Ok(())
    }

    /// Approves the plan (optionally as edited by the user) and starts it.
    pub fn approve(&self, id: u64, edited: Option<Value>) -> Result<()> {
        self.with(id, |run| {
            if run.status != RunStatus::Review {
                bail!("there's no plan waiting for approval");
            }
            if let Some(v) = edited {
                let (summary, tasks) = parse_plan(&v)?;
                run.plan_summary = summary;
                run.tasks = tasks;
            }
            run.status = RunStatus::Running;
            run.note(format!("Plan approved: {} tasks.", run.tasks.len()));
            Ok(())
        })?;
        self.advance(id);
        Ok(())
    }

    /// An agent finished a turn.
    fn on_done(&self, task_id: u64) {
        let Some(rec) = self.hub.get(task_id) else { return };
        let Some(team) = rec.team.clone() else { return };
        // Stopped for an approval: wait for the user to answer in its conversation.
        let turn = rec.events.iter().rposition(|e| matches!(e, Event::User { .. })).unwrap_or(0);
        let blocked = rec.events[turn..].iter().any(|e| matches!(e, Event::Blocked { .. }));
        let ok = rec.status == HubStatus::Done;
        let res = self.with(team.run, |run| {
            if team.key == "plan" {
                if run.planner != Some(task_id) || run.status != RunStatus::Planning {
                    return Ok(());
                }
                if blocked {
                    run.note("The architect is waiting for your approval in its conversation.");
                    return Ok(());
                }
                self.read_plan(run, task_id, ok)
            } else {
                let Some(t) = run.task_mut(&team.key) else { return Ok(()) };
                if t.task_id != Some(task_id) || !matches!(t.state, TaskState::Running | TaskState::Waiting) {
                    return Ok(());
                }
                if blocked {
                    t.state = TaskState::Waiting;
                    let title = t.title.clone();
                    run.note(format!("{title}: waiting for your approval in its conversation."));
                    return Ok(());
                }
                if !ok {
                    t.state = TaskState::Failed;
                    let title = t.title.clone();
                    run.note(format!("{title} failed."));
                    return Ok(());
                }
                self.finish_task(run, &team.key)
            }
        });
        if let Err(e) = res {
            let _ = self.with(team.run, |run| {
                run.status = RunStatus::Attention;
                run.error = Some(format!("{e:#}"));
                Ok(())
            });
        }
        self.advance(team.run);
    }

    fn read_plan(&self, run: &mut TeamRun, task_id: u64, ok: bool) -> Result<()> {
        let file = run.dir(&self.home).join("plan.json");
        let v = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).or_else(|| json_in_text(&last_text(&self.hub, task_id)));
        match v.map(|v| parse_plan(&v)) {
            Some(Ok((summary, tasks))) => {
                run.plan_summary = summary;
                run.tasks = tasks;
                run.status = RunStatus::Review;
                run.note(format!("The architect planned {} tasks. Review the plan, then start.", run.tasks.len()));
            }
            Some(Err(e)) => {
                run.status = RunStatus::Attention;
                run.error = Some(format!("The plan didn't work out: {e:#}. Reply to the architect, or Resume to plan again."));
            }
            None => {
                run.status = RunStatus::Attention;
                run.error = Some(if ok { "The architect didn't submit a plan. Resume to try again.".into() } else { "Planning failed. Resume to try again.".into() });
            }
        }
        Ok(())
    }

    /// Reads the handoff, commits the work and marks the task done.
    fn finish_task(&self, run: &mut TeamRun, key: &str) -> Result<()> {
        let dir = run.dir(&self.home);
        let (task_id, wt, start, title, stage) = {
            let t = run.task(key).context("no such task")?;
            (t.task_id, t.worktree.clone().context("task has no worktree")?, t.start.clone(), t.title.clone(), t.stage)
        };
        let hfile = dir.join(format!("{key}.handoff.json"));
        let local = wt.join(HANDOFF_FILE);
        let mut handoff: Handoff = [&hfile, &local]
            .iter()
            .find_map(|p| std::fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok()))
            .unwrap_or_else(|| Handoff { summary: clip(&task_id.map(|i| last_text(&self.hub, i)).unwrap_or_default(), 2000), ..Default::default() });
        let _ = std::fs::remove_file(&local);
        let _ = std::fs::remove_file(wt.join(MARKER));
        if stage == Stage::Merge {
            let left = unmerged(&wt);
            if !left.is_empty() {
                bail!("the conflict in {} isn't resolved yet; reply to the debugger or fix it by hand in {}", left.join(", "), wt.display());
            }
        }
        git(&wt, &["add", "-A"])?;
        let staged = !git_out(&wt, &["diff", "--cached", "--quiet"])?.status.success();
        let merging = git(&wt, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_ok();
        if merging {
            git_commit_args(&wt, &["commit", "--no-edit"])?;
        } else if staged {
            let t = run.task(key).unwrap();
            let msg = format!("{}: {}\n\nfermi team run {} ({})", t.role.title(), t.title, run.short(), t.key);
            git_commit_args(&wt, &["commit", "-m", &msg])?;
        }
        let changed: Vec<String> = match &start {
            Some(s) => git(&wt, &["diff", "--name-only", &format!("{s}..HEAD")]).map(|o| o.lines().map(String::from).collect()).unwrap_or_default(),
            None => vec![],
        };
        if handoff.files_changed.is_empty() {
            handoff.files_changed = changed.clone();
        }
        let t = run.task_mut(key).unwrap();
        t.handoff = Some(handoff);
        t.changed = changed;
        t.state = TaskState::Done;
        run.note(format!("{title}: done."));
        if stage == Stage::Merge {
            let k = key.trim_start_matches("merge-").to_string();
            if let Some(i) = run.integration.as_mut() {
                i.merged.push(k);
            }
        }
        Ok(())
    }

    /// Moves a run on: starts ready tasks, integrates, checks.
    fn advance(&self, id: u64) {
        let res = self.with(id, |run| match run.status {
            RunStatus::Running => self.schedule(run),
            RunStatus::Integrating | RunStatus::Checking => self.integrate(run),
            _ => Ok(()),
        });
        if let Err(e) = res {
            let _ = self.with(id, |run| {
                run.status = RunStatus::Attention;
                run.error = Some(format!("{e:#}"));
                run.note(format!("Stopped: {e:#}"));
                Ok(())
            });
        }
    }

    fn schedule(&self, run: &mut TeamRun) -> Result<()> {
        let plan: Vec<&TeamTask> = run.tasks.iter().filter(|t| t.stage == Stage::Plan).collect();
        if plan.iter().all(|t| matches!(t.state, TaskState::Done | TaskState::Skipped)) {
            run.status = RunStatus::Integrating;
            run.note("Every task is done. Merging the branches.");
            return self.integrate(run);
        }
        let busy = plan.iter().filter(|t| matches!(t.state, TaskState::Running | TaskState::Waiting)).count();
        let state = |k: &str| run.task(k).map(|t| t.state);
        let ready: Vec<String> = plan
            .iter()
            .filter(|t| t.state == TaskState::Pending && t.depends_on.iter().all(|d| matches!(state(d), Some(TaskState::Done | TaskState::Skipped))))
            .map(|t| t.key.clone())
            .take(run.parallel.saturating_sub(busy))
            .collect();
        let stuck = plan.iter().any(|t| matches!(t.state, TaskState::Failed | TaskState::Interrupted));
        for key in &ready {
            self.start_task(run, key)?;
        }
        let moving = busy > 0 || !ready.is_empty();
        if stuck && !moving {
            run.status = RunStatus::Attention;
            run.error = Some("A task failed or was interrupted. Retry it, skip it, or reply in its conversation.".into());
        }
        Ok(())
    }

    fn start_task(&self, run: &mut TeamRun, key: &str) -> Result<()> {
        let dir = run.dir(&self.home);
        let t = run.task(key).context("no such task")?.clone();
        let branch = format!("fermi/team-{}/{}-{}", run.short(), t.key, slug(&t.title));
        let wt = dir.join("wt").join(&t.key);
        let deps: Vec<String> = t.depends_on.iter().filter_map(|d| run.task(d)).filter(|d| d.state == TaskState::Done).filter_map(|d| d.branch.clone()).collect();
        let mut conflicts = vec![];
        if !wt.exists() {
            let start = deps.first().cloned().unwrap_or_else(|| run.base.clone());
            let exists = git(&run.repo, &["rev-parse", "--verify", "-q", &format!("refs/heads/{branch}")]).is_ok();
            if exists {
                git(&run.repo, &["worktree", "add", &wt.to_string_lossy(), &branch])?;
            } else {
                git(&run.repo, &["worktree", "add", "-b", &branch, &wt.to_string_lossy(), &start])?;
            }
            for d in deps.iter().skip(1) {
                if git_commit_args(&wt, &["merge", "--no-edit", d]).is_err() {
                    let _ = git(&wt, &["merge", "--abort"]);
                    conflicts.push(d.clone());
                }
            }
        }
        let start_commit = git(&wt, &["rev-parse", "HEAD"])?;
        write_marker(&wt, run.id, key, t.role, &dir.join(format!("{key}.handoff.json")), None)?;
        let _ = std::fs::remove_file(dir.join(format!("{key}.handoff.json")));
        {
            let tm = run.task_mut(key).unwrap();
            tm.branch = Some(branch.clone());
            tm.worktree = Some(wt.clone());
            tm.start.get_or_insert(start_commit);
        }
        let t = run.task(key).unwrap().clone();
        let mut task = Task::new(format!("{}\n\n{}", t.title, t.description).trim().to_string(), wt);
        task.context = Some(task_brief(run, &t, &conflicts));
        task.profile = run.profile.clone();
        task.model = run.models.get(t.role.as_str()).cloned();
        task.permission = run.permission.clone();
        task.team = Some(TeamRef { run: run.id, key: key.to_string(), role: t.role.as_str().into() });
        let task_id = self.hub.submit(task);
        let tm = run.task_mut(key).unwrap();
        tm.task_id = Some(task_id);
        tm.state = TaskState::Running;
        tm.note = (!conflicts.is_empty()).then(|| format!("{} conflicted; the agent merges it", conflicts.join(", ")));
        run.note(format!("Started {} ({}) on {branch}.", t.title, t.role.title()));
        Ok(())
    }

    /// Merges finished branches into the integration branch, then runs the checks.
    fn integrate(&self, run: &mut TeamRun) -> Result<()> {
        // Something still going in the integration worktree: wait for it.
        if run.tasks.iter().any(|t| t.stage != Stage::Plan && matches!(t.state, TaskState::Running | TaskState::Waiting)) {
            return Ok(());
        }
        if run.tasks.iter().any(|t| t.stage != Stage::Plan && matches!(t.state, TaskState::Failed | TaskState::Interrupted)) {
            run.status = RunStatus::Attention;
            run.error = Some("An integration step failed. Retry it, or reply in its conversation.".into());
            return Ok(());
        }
        let dir = run.dir(&self.home);
        if run.integration.is_none() {
            let branch = format!("fermi/team-{}/integration", run.short());
            let wt = dir.join("wt").join("integration");
            if !wt.exists() {
                git(&run.repo, &["worktree", "add", "-b", &branch, &wt.to_string_lossy(), &run.base])?;
            }
            run.integration = Some(Integration { branch, worktree: wt, merged: vec![] });
        }
        let integ = run.integration.clone().unwrap();
        let order = topo(&run.tasks.iter().filter(|t| t.stage == Stage::Plan).cloned().collect::<Vec<_>>())?;
        for key in order {
            let t = run.task(&key).unwrap().clone();
            if t.state != TaskState::Done || integ.merged.contains(&key) {
                continue;
            }
            let Some(branch) = t.branch.clone() else { continue };
            let msg = format!("Merge {}: {}", t.key, t.title);
            match git_commit_args(&integ.worktree, &["merge", "--no-ff", "-m", &msg, &branch]) {
                Ok(_) => {
                    run.integration.as_mut().unwrap().merged.push(key.clone());
                    run.note(format!("Merged {}.", t.title));
                }
                Err(e) => {
                    let files = unmerged(&integ.worktree);
                    if files.is_empty() {
                        let _ = git(&integ.worktree, &["merge", "--abort"]);
                        return Err(e);
                    }
                    run.note(format!("Merging {} conflicts in {}; the debugger resolves it.", t.title, files.join(", ")));
                    let fix = TeamTask {
                        key: format!("merge-{key}"),
                        role: Role::Debugger,
                        title: format!("Resolve merge conflict: {}", t.title),
                        description: format!(
                            "Merging branch {branch} ({}) into the integration branch conflicts in: {}. Resolve every conflict so both sides' \
                             intent is kept (read the handoffs), remove all conflict markers, make sure it builds, and `git add` the files. \
                             Don't commit: fermi completes the merge.",
                            t.title,
                            files.join(", ")
                        ),
                        depends_on: run.tasks.iter().filter(|x| integ.merged.contains(&x.key) || x.key == key).map(|x| x.key.clone()).collect(),
                        files,
                        done_when: "No unresolved conflicts remain and the code builds.".into(),
                        stage: Stage::Merge,
                        state: TaskState::Pending,
                        task_id: None,
                        branch: Some(integ.branch.clone()),
                        worktree: Some(integ.worktree.clone()),
                        start: None,
                        handoff: None,
                        changed: vec![],
                        note: None,
                    };
                    return self.start_integration_task(run, fix);
                }
            }
        }
        // All merged: the final checks, one after the other.
        let checks: Vec<(&str, Role, &str, &str)> = [
            (run.checks.tests, "check-tests", Role::Tester, "Final test pass", "Run the project's build and full test suite on the merged result. Fix what fails (in code or tests); if something can't be fixed, explain it in the handoff."),
            (run.checks.security, "check-security", Role::Security, "Final security review", "Review everything the team changed (`git diff BASE..HEAD`) for security problems. Fix clear problems; list the rest as risks in the handoff."),
        ]
        .into_iter()
        .filter(|c| c.0)
        .map(|c| (c.1, c.2, c.3, c.4))
        .collect();
        for (key, role, title, desc) in checks {
            if run.task(key).is_some() {
                continue;
            }
            run.status = RunStatus::Checking;
            let check = TeamTask {
                key: key.into(),
                role,
                title: title.into(),
                description: desc.replace("BASE", &run.base[..run.base.len().min(12)]),
                depends_on: run.tasks.iter().filter(|t| t.stage == Stage::Plan && t.state == TaskState::Done).map(|t| t.key.clone()).collect(),
                files: vec![],
                done_when: String::new(),
                stage: Stage::Check,
                state: TaskState::Pending,
                task_id: None,
                branch: Some(integ.branch.clone()),
                worktree: Some(integ.worktree.clone()),
                start: None,
                handoff: None,
                changed: vec![],
                note: None,
            };
            return self.start_integration_task(run, check);
        }
        run.status = RunStatus::Ready;
        run.error = None;
        run.note(format!("Ready: review the result on {} and merge it.", integ.branch));
        Ok(())
    }

    fn start_integration_task(&self, run: &mut TeamRun, mut t: TeamTask) -> Result<()> {
        let dir = run.dir(&self.home);
        let wt = t.worktree.clone().unwrap();
        t.start = Some(git(&wt, &["rev-parse", "HEAD"])?);
        write_marker(&wt, run.id, &t.key, t.role, &dir.join(format!("{}.handoff.json", t.key)), None)?;
        let _ = std::fs::remove_file(dir.join(format!("{}.handoff.json", t.key)));
        let mut task = Task::new(format!("{}\n\n{}", t.title, t.description), wt);
        task.context = Some(task_brief(run, &t, &[]));
        task.profile = run.profile.clone();
        task.model = run.models.get(t.role.as_str()).cloned();
        task.permission = run.permission.clone();
        task.team = Some(TeamRef { run: run.id, key: t.key.clone(), role: t.role.as_str().into() });
        t.task_id = Some(self.hub.submit(task));
        t.state = TaskState::Running;
        run.note(format!("Started {} ({}).", t.title, t.role.title()));
        run.tasks.retain(|x| x.key != t.key);
        run.tasks.push(t);
        Ok(())
    }

    // ── user actions ────────────────────────────────────────────────────────

    pub fn cancel(&self, id: u64) -> Result<()> {
        let ids: Vec<u64> = self.with(id, |run| {
            let mut ids: Vec<u64> = run.tasks.iter().filter(|t| matches!(t.state, TaskState::Running | TaskState::Waiting)).filter_map(|t| t.task_id).collect();
            if run.status == RunStatus::Planning {
                ids.extend(run.planner);
            }
            for t in run.tasks.iter_mut().filter(|t| matches!(t.state, TaskState::Running | TaskState::Waiting)) {
                t.state = TaskState::Interrupted;
            }
            run.status = RunStatus::Cancelled;
            run.note("Cancelled.");
            Ok(ids)
        })?;
        for t in ids {
            self.hub.cancel(t);
        }
        Ok(())
    }

    /// Picks a stopped run up again: replans, or restarts what was interrupted.
    pub fn resume(&self, id: u64) -> Result<()> {
        self.with(id, |run| {
            if !matches!(run.status, RunStatus::Attention | RunStatus::Cancelled) {
                bail!("this run isn't stopped");
            }
            run.error = None;
            if run.tasks.is_empty() {
                return self.start_planner(run);
            }
            for t in run.tasks.iter_mut().filter(|t| matches!(t.state, TaskState::Interrupted | TaskState::Failed)) {
                t.state = TaskState::Pending;
                t.task_id = None;
            }
            // Interrupted integration steps run again from the start.
            run.tasks.retain(|t| t.stage == Stage::Plan || t.state != TaskState::Pending);
            let plan_done = run.tasks.iter().filter(|t| t.stage == Stage::Plan).all(|t| matches!(t.state, TaskState::Done | TaskState::Skipped));
            run.status = if plan_done { RunStatus::Integrating } else { RunStatus::Running };
            run.note("Resumed.");
            Ok(())
        })?;
        self.advance(id);
        Ok(())
    }

    pub fn retry(&self, id: u64, key: &str) -> Result<()> {
        self.with(id, |run| {
            let t = run.task_mut(key).context("no such task")?;
            if !matches!(t.state, TaskState::Failed | TaskState::Interrupted) {
                bail!("only a failed or interrupted task can be retried");
            }
            if t.stage == Stage::Plan {
                t.state = TaskState::Pending;
                t.task_id = None;
            } else {
                run.tasks.retain(|x| x.key != key);
            }
            run.error = None;
            let plan_done = run.tasks.iter().filter(|t| t.stage == Stage::Plan).all(|t| matches!(t.state, TaskState::Done | TaskState::Skipped));
            run.status = if plan_done { RunStatus::Integrating } else { RunStatus::Running };
            Ok(())
        })?;
        self.advance(id);
        Ok(())
    }

    pub fn skip(&self, id: u64, key: &str) -> Result<()> {
        self.with(id, |run| {
            let t = run.task_mut(key).context("no such task")?;
            if t.stage != Stage::Plan || matches!(t.state, TaskState::Done | TaskState::Running | TaskState::Waiting) {
                bail!("that task can't be skipped now");
            }
            t.state = TaskState::Skipped;
            run.note(format!("Skipped {key}."));
            if run.status == RunStatus::Attention {
                run.status = RunStatus::Running;
                run.error = None;
            }
            Ok(())
        })?;
        self.advance(id);
        Ok(())
    }

    /// Merges the integration branch into the branch the run started on.
    pub fn merge(&self, id: u64) -> Result<String> {
        let run = self.get(id).context("no such team run")?;
        if run.status != RunStatus::Ready {
            bail!("the run isn't ready to merge");
        }
        let integ = run.integration.clone().context("nothing to merge")?;
        let current = git(&run.repo, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        if let Some(b) = &run.base_branch {
            if &current != b {
                bail!("check out {b} in {} first (it's on {current})", run.repo.display());
            }
        }
        if !git(&run.repo, &["status", "--porcelain", "--untracked-files=no"])?.is_empty() {
            bail!("{} has uncommitted changes; commit or stash them first", run.repo.display());
        }
        let msg = format!("fermi team: {}", clip(&run.goal, 72));
        if let Err(e) = git_commit_args(&run.repo, &["merge", "--no-ff", "-m", &msg, &integ.branch]) {
            let _ = git(&run.repo, &["merge", "--abort"]);
            bail!("the merge conflicts with changes made on {current} since the run started ({e:#}); merge {} by hand", integ.branch);
        }
        self.cleanup(&run, false);
        self.with(id, |run| {
            run.status = RunStatus::Merged;
            run.note(format!("Merged into {current}."));
            Ok(())
        })?;
        Ok(current)
    }

    /// Stops the run and removes its worktrees and branches.
    pub fn discard(&self, id: u64) -> Result<()> {
        let _ = self.cancel(id);
        let run = self.get(id).context("no such team run")?;
        self.cleanup(&run, true);
        self.with(id, |run| {
            run.status = RunStatus::Discarded;
            run.note("Discarded: worktrees and branches removed.");
            Ok(())
        })
    }

    fn cleanup(&self, run: &TeamRun, all_branches: bool) {
        let wt_root = run.dir(&self.home).join("wt");
        for e in std::fs::read_dir(&wt_root).into_iter().flatten().filter_map(|e| e.ok()) {
            let _ = git(&run.repo, &["worktree", "remove", "--force", &e.path().to_string_lossy()]);
        }
        let _ = git(&run.repo, &["worktree", "prune"]);
        let _ = std::fs::remove_dir_all(&wt_root);
        let prefix = format!("fermi/team-{}/", run.short());
        if let Ok(list) = git(&run.repo, &["branch", "--list", &format!("{prefix}*"), "--format=%(refname:short)"]) {
            for b in list.lines().filter(|b| all_branches || !b.is_empty()) {
                let _ = git(&run.repo, &["branch", "-D", b]);
            }
        }
    }

    /// The changes of one task (or the whole run) against where it started.
    pub fn diff(&self, id: u64, key: Option<&str>, file: Option<&str>) -> Result<String> {
        let run = self.get(id).context("no such team run")?;
        let (from, to) = match key {
            Some(k) => {
                let t = run.task(k).context("no such task")?;
                (t.start.clone().context("not started")?, t.branch.clone().context("no branch")?)
            }
            None => (run.base.clone(), run.integration.as_ref().map(|i| i.branch.clone()).context("not integrated yet")?),
        };
        let range = format!("{from}..{to}");
        let mut args = vec!["diff", "--no-color", range.as_str()];
        if let Some(f) = file {
            args.extend(["--", f]);
        } else {
            args.push("--stat");
        }
        git(&run.repo, &args)
    }
}

fn write_marker(wt: &Path, run: u64, key: &str, role: Role, handoff: &Path, plan: Option<&Path>) -> Result<()> {
    let v = json!({ "run": run, "key": key, "role": role, "handoff": handoff, "plan": plan });
    std::fs::write(wt.join(MARKER), serde_json::to_vec_pretty(&v)?)?;
    Ok(())
}

/// For the MCP bridge: where this folder's agent writes its handoff and plan.
pub fn marker(cwd: &Path) -> Option<Value> {
    serde_json::from_slice(&std::fs::read(cwd.join(MARKER)).ok()?).ok()
}

/// Gathers each run's latest status for the dashboard list (no logs).
pub fn summary(run: &TeamRun) -> Value {
    let counts: HashMap<&str, usize> = run.tasks.iter().fold(HashMap::new(), |mut m, t| {
        *m.entry(match t.state {
            TaskState::Done | TaskState::Skipped => "done",
            TaskState::Running | TaskState::Waiting => "running",
            _ => "other",
        })
        .or_insert(0) += 1;
        m
    });
    json!({ "id": run.id, "goal": run.goal, "status": run.status, "repo": run.repo, "tasks": run.tasks.len(), "done": counts.get("done"), "running": counts.get("running"), "updated": run.updated })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_parse_and_order() {
        let v = json!({ "summary": "s", "tasks": [
            { "id": "api", "role": "developer", "title": "API", "depends_on": ["schema"] },
            { "id": "schema", "role": "architect", "title": "Schema" },
            { "id": "tests", "role": "QA", "title": "Tests", "depends_on": ["api"] }
        ]});
        let (summary, tasks) = parse_plan(&v).unwrap();
        assert_eq!(summary, "s");
        assert_eq!(tasks[2].role, Role::Tester);
        assert_eq!(topo(&tasks).unwrap(), ["schema", "api", "tests"]);
        let cyc = json!({ "tasks": [{ "id": "a", "depends_on": ["b"] }, { "id": "b", "depends_on": ["a"] }] });
        assert!(parse_plan(&cyc).is_err());
        assert!(parse_plan(&json!({ "tasks": [{ "id": "a", "depends_on": ["zzz"] }] })).is_err());
        assert!(parse_plan(&json!({ "tasks": [{ "id": "a", "role": "wizard" }] })).is_err());
    }

    #[test]
    fn json_from_replies() {
        let t = "Here's the plan:\n```json\n{\"tasks\": [{\"id\": \"a\"}]}\n```\nDone.";
        assert_eq!(json_in_text(t).unwrap()["tasks"][0]["id"], "a");
        assert_eq!(json_in_text("plain {\"x\": 1} text").unwrap()["x"], 1);
        assert!(json_in_text("no json").is_none());
        assert_eq!(slug("Add OAuth login!"), "add-oauth-login");
    }
}
