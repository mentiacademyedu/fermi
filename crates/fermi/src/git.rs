//! Read-only git views for the dashboard's Changes panel: what changed in a
//! workspace and the diff of one file. Runs the user's `git` binary.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::Path;
use std::process::Command;

/// Diffs above this are cut off.
const MAX_DIFF: usize = 1024 * 1024;

/// Runs git in `root` without flashing a console window; any exit code.
fn git_output(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.output().context("running git (is it installed?)")
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = git_output(root, args)?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(out.stdout)
}

#[derive(Serialize)]
pub struct Change {
    /// Path relative to the repository's working tree, `/`-separated.
    pub rel: String,
    pub path: String,
    /// `M` modified, `A` added, `D` deleted, `R` renamed, `U` untracked, `C` conflict.
    pub status: String,
    pub staged: bool,
}

#[derive(Serialize)]
pub struct Status {
    pub repo: bool,
    pub branch: Option<String>,
    pub changes: Vec<Change>,
}

pub fn status(root: &Path) -> Result<Status> {
    if git(root, &["rev-parse", "--is-inside-work-tree"]).is_err() {
        return Ok(Status { repo: false, branch: None, changes: vec![] });
    }
    let top = String::from_utf8_lossy(&git(root, &["rev-parse", "--show-toplevel"])?).trim().to_string();
    let branch = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).ok().map(|b| String::from_utf8_lossy(&b).trim().to_string());
    let raw = git(root, &["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    let mut changes = vec![];
    let mut parts = raw.split(|b| *b == 0).filter(|p| !p.is_empty());
    while let Some(entry) = parts.next() {
        let entry = String::from_utf8_lossy(entry);
        if entry.len() < 4 {
            continue;
        }
        let (x, y, rel) = (&entry[0..1], &entry[1..2], entry[3..].to_string());
        if x == "R" || x == "C" {
            parts.next(); // the original path of a rename/copy
        }
        let status = match (x, y) {
            ("?", "?") => "U",
            ("U", _) | (_, "U") | ("A", "A") | ("D", "D") => "C",
            (_, "D") | ("D", _) => "D",
            ("R", _) => "R",
            ("A", _) => "A",
            _ => "M",
        };
        let path = Path::new(&top).join(&rel).to_string_lossy().into_owned();
        changes.push(Change { rel, path, status: status.into(), staged: x != " " && x != "?" });
        if changes.len() >= 2000 {
            break;
        }
    }
    Ok(Status { repo: true, branch, changes })
}

/// Unified diff of one file against HEAD (untracked files show as all added).
pub fn diff(root: &Path, file: &Path) -> Result<String> {
    // Relative paths keep the diff headers readable (`a/src/x.rs`).
    let f = file.strip_prefix(root).unwrap_or(file).to_string_lossy().replace('\\', "/");
    let mut text = String::from_utf8_lossy(&git(root, &["diff", "HEAD", "--no-color", "--", &f]).unwrap_or_default()).into_owned();
    if text.trim().is_empty() {
        // Untracked (or a repo with no commits yet): diff against nothing.
        // `--no-index` exits 1 when files differ, so the exit code is ignored.
        let null = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let out = git_output(root, &["diff", "--no-color", "--no-index", "--", null, &f])?;
        text = String::from_utf8_lossy(&out.stdout).into_owned();
    }
    if text.len() > MAX_DIFF {
        let mut cut = MAX_DIFF;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n… diff truncated\n");
    }
    Ok(text)
}
