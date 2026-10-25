//! Isolated workspaces: a task that asks for one (`Task::isolate`) runs in a
//! git worktree of its own, on branch `fermi/task-<id>` from the repository's
//! current commit, so tasks started together on one repository can't step on
//! each other. The work stays there, uncommitted, for review ([`diff`]); the
//! repository's own folder is never touched.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    /// The repository the task was started on.
    pub repo: PathBuf,
    /// The worktree it runs in.
    pub path: PathBuf,
    pub branch: String,
    /// The commit the worktree started from.
    pub base: String,
}

fn git(dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    if let Some(i) = index {
        cmd.env("GIT_INDEX_FILE", i);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().context("running git")?;
    if !out.status.success() {
        bail!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn dir(home: &Path, id: u64) -> PathBuf {
    home.join("worktrees").join(format!("task-{id}"))
}

/// A worktree for task `id` of the repository containing `cwd`. Returns it,
/// and the folder to work in (`cwd`'s place inside the worktree).
pub fn create(home: &Path, cwd: &Path, id: u64) -> Result<(Workspace, PathBuf)> {
    let root = git(cwd, &["rev-parse", "--show-toplevel"], None)
        .map(|s| PathBuf::from(s.trim()))
        .map_err(|_| anyhow::anyhow!("{} isn't in a git repository, so it can't get an isolated worktree", cwd.display()))?;
    let base = git(&root, &["rev-parse", "HEAD"], None)
        .map(|s| s.trim().to_string())
        .map_err(|_| anyhow::anyhow!("{} has no commit yet to start an isolated worktree from", root.display()))?;
    let path = dir(home, id);
    let branch = format!("fermi/task-{id}");
    std::fs::create_dir_all(path.parent().unwrap())?;
    git(&root, &["worktree", "add", "-q", "-b", &branch, &path.to_string_lossy(), &base], None)?;
    // `rev-parse` gives the root with forward slashes on Windows: compare canonical paths.
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let rel = canon(cwd).strip_prefix(canon(&root)).map(Path::to_path_buf).unwrap_or_default();
    let work = path.join(rel);
    std::fs::create_dir_all(&work)?;
    Ok((Workspace { repo: root, path, branch, base }, work))
}

/// The task's work for review: changed files and the full patch against the
/// base commit (new files included), and the fingerprint `verify` gives the
/// same files, to match it with a check report.
pub fn diff(ws: &Workspace) -> Result<Value> {
    if !ws.path.is_dir() {
        bail!("the worktree {} is gone", ws.path.display());
    }
    // New files go in a scratch index, never the worktree's own.
    let git_dir = PathBuf::from(git(&ws.path, &["rev-parse", "--git-dir"], None)?.trim());
    let git_dir = if git_dir.is_absolute() { git_dir } else { ws.path.join(git_dir) };
    let index = git_dir.join("fermi-review-index");
    let _ = std::fs::remove_file(&index);
    git(&ws.path, &["read-tree", "HEAD"], Some(&index))?;
    git(&ws.path, &["add", "-A"], Some(&index))?;
    let patch = git(&ws.path, &["diff", "--cached", "--binary", &ws.base], Some(&index))?;
    let names = git(&ws.path, &["diff", "--cached", "--name-status", &ws.base], Some(&index))?;
    let _ = std::fs::remove_file(&index);
    let files: Vec<Value> = names
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(status, path)| json!({ "status": status, "path": path.replace('\t', " -> ") }))
        .collect();
    let (_, dirty, fingerprint) = crate::verify::fingerprint(&ws.path);
    Ok(json!({
        "repo": ws.repo, "path": ws.path, "branch": ws.branch, "base": ws.base,
        "files": files, "patch": patch, "fingerprint": fingerprint, "uncommitted": dirty,
    }))
}

/// Removes the worktree and its branch.
pub fn remove(ws: &Workspace) {
    let _ = git(&ws.repo, &["worktree", "remove", "--force", &ws.path.to_string_lossy()], None);
    let _ = git(&ws.repo, &["branch", "-D", &ws.branch], None);
    let _ = std::fs::remove_dir_all(&ws.path);
}
