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
}
