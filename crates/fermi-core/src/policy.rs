//! File-access policy for agents: `[security] deny_read` globs that agents
//! must not read (secrets, keys, other projects).
//!
//! ```toml
//! [security]
//! deny_read = ["~/.ssh/**", "**/.env", "C:/work/secrets/**"]
//! ```
//!
//! Enforcement: Claude agents get the globs as `--disallowedTools
//! "Read(//…)"` rules, which Claude Code applies to its file tools, to file
//! commands it recognizes in shell commands and to edits of those paths, in
//! every permission mode; arbitrary programs an agent runs aren't covered (that
//! takes an OS sandbox). fermi's own tools (code index, memory) refuse the
//! paths too. Codex and Gemini agents have no equivalent switch: the rules
//! don't reach them.
//!
//! The same policy decides every tool call Claude asks fermi about (its
//! `--permission-prompt-tool` is fermi's bridge): `deny_commands` are refused,
//! `confirm_commands` wait for an approval (see `approvals`), and shell
//! commands that name a deny_read path are refused like reads.

use anyhow::{bail, Result};
use std::path::Path;

/// A glob as an absolute pattern with `/` separators (`~` expanded).
fn absolute(glob: &str) -> Result<String> {
    let g = glob.trim().replace('\\', "/");
    let g = match g.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", dirs::home_dir().map(|h| h.to_string_lossy().replace('\\', "/")).unwrap_or_default()),
        None => g,
    };
    let drive = g.len() > 2 && g.as_bytes()[1] == b':' && g.as_bytes()[0].is_ascii_alphabetic();
    if !(g.starts_with('/') || g.starts_with("**/") || drive) {
        bail!("deny_read pattern `{glob}` must be absolute, start with ~/ or start with **/");
    }
    Ok(g)
}

/// Checks the patterns (config validation).
pub fn validate(globs: &[String]) -> Result<()> {
    for g in globs {
        let a = absolute(g)?;
        globset::Glob::new(&a).map_err(|e| anyhow::anyhow!("deny_read pattern `{g}`: {e}"))?;
    }
    Ok(())
}

/// Claude Code deny rules for the patterns (`//` anchors an absolute path; on
/// Windows Claude matches drive paths as `/c/…`).
pub fn claude_read_rules(globs: &[String]) -> Vec<String> {
    globs
        .iter()
        .filter_map(|g| absolute(g).ok())
        .map(|g| {
            let posix = if g.len() > 2 && g.as_bytes()[1] == b':' { format!("/{}{}", g[..1].to_ascii_lowercase(), &g[2..]) } else { g };
            let rooted = posix.trim_start_matches('/');
            format!("Read(//{rooted})")
        })
        .collect()
}

/// Whether `path` matches a deny_read pattern.
pub fn denied(globs: &[String], path: &Path) -> bool {
    if globs.is_empty() {
        return false;
    }
    let p = path.to_string_lossy().replace('\\', "/");
    let p = p.trim_start_matches("//?/");
    globs.iter().filter_map(|g| absolute(g).ok()).any(|g| {
        globset::GlobBuilder::new(&g).case_insensitive(cfg!(windows)).build().map(|gl| gl.compile_matcher().is_match(p)).unwrap_or(false)
    })
}

/// What the policy says about one tool call.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow,
    /// Refused outright.
    Deny(String),
    /// Needs a person's approval (see `approvals`).
    Hold(String),
}

/// Whether any policy applies (then Claude asks fermi about tool calls).
pub fn active(sec: &crate::config::SecurityCfg) -> bool {
    !sec.deny_read.is_empty() || !sec.deny_commands.is_empty() || !sec.confirm_commands.is_empty() || sec.sandbox
}

/// A Claude-style command pattern (`rm *`, `git push *`): `*` matches any
/// text. A chained command matches when any of its parts does.
pub fn command_matches(pattern: &str, command: &str) -> bool {
    let re = format!("^{}$", regex::escape(pattern.trim()).replace(r"\*", ".*"));
    let Ok(re) = regex::Regex::new(&re) else { return false };
    command.split(['\n', ';', '|', '&']).map(str::trim).filter(|p| !p.is_empty()).any(|part| re.is_match(part))
}

/// Paths a shell command names (absolute, `~/…`, or relative to `cwd`).
fn command_paths(command: &str, cwd: &Path) -> Vec<std::path::PathBuf> {
    command
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '=' | '(' | ')' | ';' | '|' | '&'))
        .filter(|t| t.contains('/') || t.contains('\\') || t.starts_with('~') || t.starts_with('.'))
        .map(|t| resolve(t, cwd))
        .collect()
}

/// A path as written by an agent, made absolute and with `..` resolved
/// lexically, so traversal can't hide where it points.
pub(crate) fn resolve(t: &str, cwd: &Path) -> std::path::PathBuf {
    let p = if let Some(rest) = t.strip_prefix("~/").or_else(|| t.strip_prefix("~\\")) {
        dirs::home_dir().unwrap_or_default().join(rest)
    } else if is_abs(t) {
        Path::new(t).to_path_buf()
    } else {
        cwd.join(t)
    };
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Absolute on any platform: `/x`, `\x` or `C:/x` (`Path::is_absolute` says
/// no to `/x` on Windows).
fn is_abs(t: &str) -> bool {
    t.starts_with('/') || t.starts_with('\\') || (t.len() > 2 && t.as_bytes()[1] == b':')
}

/// The resource a tool call acts on: the command, or the file.
pub fn resource(tool: &str, input: &serde_json::Value) -> String {
    match tool {
        "Bash" | "PowerShell" => input["command"].as_str().unwrap_or("").to_string(),
        _ => ["file_path", "path", "notebook_path", "url"].iter().find_map(|k| input[*k].as_str()).unwrap_or("").to_string(),
    }
}

/// The policy's decision on a tool call made from `cwd`.
pub fn decide(sec: &crate::config::SecurityCfg, tool: &str, input: &serde_json::Value, cwd: &Path) -> Decision {
    let res = resource(tool, input);
    let is_shell = matches!(tool, "Bash" | "PowerShell");
    if is_shell {
        if let Some(p) = sec.deny_commands.iter().find(|p| command_matches(p, &res)) {
            return Decision::Deny(format!("the command matches `{p}`, which this project's policy forbids"));
        }
        // Reading a forbidden file through the shell is still reading it.
        if let Some(p) = command_paths(&res, cwd).into_iter().find(|p| denied(&sec.deny_read, p) || symlink_into_denied(&sec.deny_read, p))
        {
            return Decision::Deny(format!("{} is off limits to agents (deny_read)", p.display()));
        }
        if let Some(p) = sec.confirm_commands.iter().find(|p| command_matches(p, &res)) {
            return Decision::Hold(format!("the command matches `{p}`, which needs the user's approval"));
        }
    } else if !res.is_empty() {
        let p = resolve(&res, cwd);
        if denied(&sec.deny_read, &p) || symlink_into_denied(&sec.deny_read, &p) {
            return Decision::Deny(format!("{res} is off limits to agents (deny_read)"));
        }
    }
    Decision::Allow
}

/// A link (or a path through one) that ends at a forbidden file.
fn symlink_into_denied(globs: &[String], p: &Path) -> bool {
    !globs.is_empty() && std::fs::canonicalize(p).is_ok_and(|real| denied(globs, &real))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_and_matching() {
        let globs = vec!["/srv/secret/**".to_string(), "**/.env".to_string(), "C:/work/keys/**".to_string()];
        assert_eq!(claude_read_rules(&globs), ["Read(//srv/secret/**)", "Read(//**/.env)", "Read(//c/work/keys/**)"]);
        assert!(denied(&globs, Path::new("/srv/secret/a.txt")));
        assert!(denied(&globs, Path::new("/home/x/app/.env")));
        assert!(!denied(&globs, Path::new("/srv/public/a.txt")));
        assert!(validate(&["relative/**".to_string()]).is_err());
    }

    #[test]
    fn commands_and_paths() {
        assert!(command_matches("rm *", "rm -rf build"));
        assert!(command_matches("rm *", "cd x && rm a.txt"));
        assert!(!command_matches("rm *", "firm a"));
        let globs = vec!["/srv/secret/**".to_string()];
        let sec = crate::config::SecurityCfg { deny_read: globs, confirm_commands: vec!["rm *".into()], ..Default::default() };
        let cwd = Path::new("/srv/app");
        let bash = |c: &str| decide(&sec, "Bash", &serde_json::json!({ "command": c }), cwd);
        assert!(matches!(bash("cat ../secret/token"), Decision::Deny(_)));
        assert!(matches!(bash("cat /srv/secret/token"), Decision::Deny(_)));
        assert!(matches!(bash("rm build/x"), Decision::Hold(_)));
        assert_eq!(bash("ls src"), Decision::Allow);
        assert!(matches!(decide(&sec, "Read", &serde_json::json!({ "file_path": "../secret/a" }), cwd), Decision::Deny(_)));
    }
}
