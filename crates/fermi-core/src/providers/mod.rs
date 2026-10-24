//! Provider adapters. CLI adapters spawn the official CLIs headless with an
//! isolated config dir per account; API adapters call the HTTP APIs.

mod api;
mod claude_cli;
mod codex_cli;
pub mod gemini_cli;
mod image;
pub(crate) mod proc;

use crate::config::{Account, Auth, Profile, Provider};
use crate::engine::{Engine, Task};
use crate::events::Tx;
use crate::models::ModelSpec;
use anyhow::Result;
use std::path::PathBuf;

pub struct RunCtx<'a> {
    pub engine: &'a Engine,
    pub profile: &'a Profile,
    pub account: &'a Account,
    pub model: &'a ModelSpec,
    pub task: &'a Task,
    pub tx: &'a Tx,
}

impl RunCtx<'_> {
    /// The sandbox for this run's CLI, when `[security] sandbox` is on.
    pub async fn jail(&self, bin: &std::path::Path) -> Result<Option<crate::sandbox::Jail>> {
        let e = self.engine;
        if !e.cfg.security.sandbox {
            return Ok(None);
        }
        let port = e.egress_port().await;
        crate::sandbox::jail(&e.cfg, &e.home, &self.task.cwd, self.account.home_dir().as_deref(), bin, port).map(Some)
    }
    /// Starts the CLI, recording its responses (see `trace`); in a replay,
    /// plays the recorded ones instead and starts nothing.
    pub async fn spawn(&self, bin: &std::path::Path, args: &[String], envs: &[(String, String)], input: &str) -> Result<proc::Proc> {
        if let Some(r) = &self.engine.replay {
            return Ok(proc::Proc::replay(r.open(self)?, self.tx));
        }
        let jail = self.jail(bin).await?;
        let mut p = proc::Proc::spawn(bin, args, envs, &self.task.cwd, input, self.tx, jail.as_ref()).await?;
        p.redact = Some(self.engine.redactor.clone());
        p.tape = crate::trace::TapeOut::create(self, args);
        Ok(p)
    }
    /// The CLI to run (a replay starts none, so it needn't be installed).
    pub fn bin(&self, name: &str, overridden: Option<&str>) -> Result<PathBuf> {
        match &self.engine.replay {
            Some(_) => Ok(PathBuf::from(name)),
            None => crate::util::resolve_bin(name, overridden),
        }
    }
    /// A new provider session id (in a replay, the recorded one).
    pub fn new_session(&self) -> Result<String> {
        match &self.engine.replay {
            Some(r) => Ok(r.session(self.task)?.unwrap_or_else(crate::util::uuid_v4)),
            None => Ok(crate::util::uuid_v4()),
        }
    }
    /// Task effort, else the profile's default.
    pub fn effort(&self) -> Option<&str> {
        self.task.effort.as_deref().or(self.profile.effort.as_deref())
    }
    /// Claude-style permission mode: the task's, else the profile's.
    pub fn permission(&self) -> Option<&str> {
        self.task.permission.as_deref().or(self.profile.permission_mode.as_deref()).filter(|p| *p != "default")
    }
    /// Codex sandbox: follows a per-task permission override, else the profile's setting.
    pub fn codex_sandbox(&self) -> Option<&str> {
        match self.task.permission.as_deref() {
            Some("plan") => Some("read-only"),
            Some("acceptEdits") => Some("workspace-write"),
            Some("bypassPermissions") => Some("danger-full-access"),
            _ => self.profile.codex_sandbox.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    /// Every input token, including the cached and cache-written ones.
    pub input: u64,
    /// Input tokens read from the provider's prompt cache.
    pub cached_input: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write: u64,
    pub output: u64,
    /// Cost the provider (CLI) reported, if any.
    pub cost_usd: Option<f64>,
    /// Which counts the provider reported (`INPUT | OUTPUT…`); the others are unknown, not zero.
    pub reported: u8,
}

impl Usage {
    pub const INPUT: u8 = 1;
    pub const CACHED: u8 = 2;
    pub const CACHE_WRITE: u8 = 4;
    pub const OUTPUT: u8 = 8;

    /// `value` if the provider reported that count.
    pub fn known(&self, field: u8, value: u64) -> Option<u64> {
        (self.reported & field != 0).then_some(value)
    }

    /// The bits for the keys present in a provider's usage object.
    pub fn present(u: &serde_json::Value, keys: &[(&str, u8)]) -> u8 {
        keys.iter().filter(|(k, _)| u.get(*k).is_some_and(|v| v.is_u64())).fold(0, |acc, (_, b)| acc | b)
    }
}

#[derive(Debug, Clone)]
pub enum Status {
    Ok,
    /// Account hit a usage/rate limit; `until` = reset time if known.
    Limited {
        until: Option<u64>,
    },
    /// `retry` = worth trying the next account (auth/availability problems).
    Failed {
        msg: String,
        retry: bool,
    },
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub status: Status,
    pub text: String,
    pub usage: Usage,
    pub artifacts: Vec<PathBuf>,
    /// The provider session this run used, when the adapter knows it.
    pub session: Option<String>,
}

impl RunResult {
    fn new(status: Status, text: String, usage: Usage) -> Self {
        RunResult { status, text, usage, artifacts: vec![], session: None }
    }
    pub fn ok(text: String, usage: Usage) -> Self {
        Self::new(Status::Ok, text, usage)
    }
    pub fn failed(msg: String, retry: bool) -> Self {
        Self::new(Status::Failed { msg, retry }, String::new(), Usage::default())
    }
    pub fn limited(until: Option<u64>) -> Self {
        Self::new(Status::Limited { until }, String::new(), Usage::default())
    }
}

pub async fn run(ctx: &RunCtx<'_>) -> Result<RunResult> {
    if ctx.engine.replay.is_some() && ctx.account.auth == Auth::Api && ctx.account.provider != Provider::Gemini {
        anyhow::bail!("replays cover CLI accounts; `{}` calls an HTTP API, which isn't recorded", ctx.account.id);
    }
    if let Some(spec) = &ctx.account.adapter {
        return crate::plugins::run_adapter(ctx, spec).await;
    }
    if ctx.model.image_only() {
        return image::run(ctx).await;
    }
    match (ctx.account.provider, ctx.account.auth) {
        (Provider::Claude, Auth::Cli) => claude_cli::run(ctx).await,
        (Provider::Openai, Auth::Cli) => codex_cli::run(ctx, &ctx.task.input(), ctx.model.cli.as_deref(), None).await,
        (Provider::Claude, Auth::Api) => api::anthropic(ctx).await,
        (Provider::Openai, Auth::Api) => api::openai(ctx).await,
        // The Gemini CLI takes API keys too, so both go through it.
        (Provider::Gemini, _) => gemini_cli::run(ctx).await,
    }
}

pub(crate) fn is_limit(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "usage limit",
        "rate limit",
        "rate_limit",
        "hit your limit",
        "limit reached",
        "quota",
        "too many requests",
        "resource exhausted",
        "resource_exhausted",
        "429",
    ]
    .iter()
    .any(|w| m.contains(w))
}

pub(crate) fn is_auth(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "/login",
        "log in",
        "logged in",
        "login",
        "authenticat",
        "api key",
        "unauthorized",
        "401",
        "credit balance",
        "not signed in",
        "auth method",
    ]
    .iter()
    .any(|w| m.contains(w))
}

/// A provider-side outage another account may not have (the API adapters
/// treat HTTP 5xx the same way).
pub(crate) fn is_transient(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "500 ",
        "502",
        "503",
        "504",
        "529",
        "overloaded",
        "service unavailable",
        "internal server error",
        "bad gateway",
        "gateway timeout",
        "timed out",
        "connection reset",
        "econnreset",
    ]
    .iter()
    .any(|w| m.contains(w))
}

/// Turns a CLI's final error text into a status. Login problems and
/// provider outages are worth trying on the profile's next account.
pub(crate) fn classify_error(msg: String, resets: Option<u64>) -> RunResult {
    if is_limit(&msg) {
        RunResult::limited(resets)
    } else {
        let retry = is_auth(&msg) || is_transient(&msg);
        RunResult::failed(msg, retry)
    }
}

/// Short one-line preview of a tool input for display.
pub(crate) fn brief(v: &serde_json::Value) -> String {
    let s = match v {
        serde_json::Value::Object(o) => ["command", "file_path", "path", "pattern", "url", "query", "prompt", "description"]
            .iter()
            .find_map(|k| o.get(*k).and_then(|x| x.as_str()))
            .map(str::to_string)
            .unwrap_or_else(|| v.to_string()),
        serde_json::Value::String(s) => s.clone(),
        _ => v.to_string(),
    };
    let line = s.lines().next().unwrap_or("").to_string();
    if line.chars().count() > 120 {
        format!("{}…", line.chars().take(120).collect::<String>())
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outages_fail_over_and_bugs_do_not() {
        let retry = |m: &str| matches!(classify_error(m.into(), None).status, Status::Failed { retry: true, .. });
        assert!(retry("API Error: 503 Service Unavailable"));
        assert!(retry("Overloaded"));
        assert!(retry("Please run /login"));
        assert!(!retry("syntax error in your request"));
        assert!(matches!(classify_error("429 Too Many Requests".into(), None).status, Status::Limited { .. }));
    }
}
