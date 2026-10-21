use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Expands a leading `~` to the home directory.
pub fn expand(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest.trim_start_matches(['/', '\\']));
        }
    }
    PathBuf::from(p)
}

/// Normalised form used for prefix matching profile paths.
pub fn norm_path(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    if cfg!(windows) { s.to_lowercase() } else { s }
}

/// `2h13m`, `45m`, `30s`.
pub fn fmt_dur(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

pub fn fmt_tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// Finds a CLI binary on PATH. On Windows, npm `.cmd` shims that wrap a
/// native `.exe` are unwrapped so we skip the cmd.exe hop.
pub fn resolve_bin(name: &str, overridden: Option<&str>) -> Result<PathBuf> {
    if let Some(p) = overridden {
        return Ok(expand(p));
    }
    static CACHE: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(p) = cache.lock().unwrap().get(name) {
        return Ok(p.clone());
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
    let mut found = None;
    'outer: for dir in std::env::split_paths(&path) {
        for ext in exts {
            let cand = dir.join(format!("{name}{ext}"));
            if cand.is_file() {
                found = Some(if *ext == ".cmd" { unwrap_cmd_shim(&cand).unwrap_or(cand) } else { cand });
                break 'outer;
            }
        }
    }
    let p = found.ok_or_else(|| {
        let install = match name {
            "claude" => " Install Claude Code: npm install -g @anthropic-ai/claude-code",
            "codex" => " Install the Codex CLI: npm install -g @openai/codex",
            "gemini" => " Install the Gemini CLI: npm install -g @google/gemini-cli",
            _ => "",
        };
        anyhow!("`{name}` isn't installed (not found on PATH).{install}  If it's installed somewhere else, set [bins].{name} in the config.")
    })?;
    cache.lock().unwrap().insert(name.to_string(), p.clone());
    Ok(p)
}

fn unwrap_cmd_shim(cmd: &Path) -> Option<PathBuf> {
    // node-based shims also mention `%dp0%\node.exe`; only a native target counts.
    let text = std::fs::read_to_string(cmd).ok()?;
    text.split("\"%dp0%\\").skip(1).find_map(|rest| {
        let rel = &rest[..rest.find('"')?];
        let lower = rel.to_lowercase();
        if !lower.ends_with(".exe") || lower.ends_with("node.exe") {
            return None;
        }
        let exe = cmd.parent()?.join(rel.trim_start_matches('\\'));
        exe.is_file().then_some(exe)
    })
}
