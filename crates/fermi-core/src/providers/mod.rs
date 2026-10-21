//! Provider adapters. CLI adapters spawn the official CLIs headless with an
//! isolated config dir per account; API adapters call the HTTP APIs.

mod api;
mod claude_cli;
mod codex_cli;
pub mod gemini_cli;
mod image;
mod proc;

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
    pub input: u64,
    pub output: u64,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone)]
pub enum Status {
    Ok,
    /// Account hit a usage/rate limit; `until` = reset time if known.
    Limited { until: Option<u64> },
    /// `retry` = worth trying the next account (auth/availability problems).
    Failed { msg: String, retry: bool },
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
    if ctx.model.image_only() {
        return image::run(ctx).await;
    }
    match (ctx.account.provider, ctx.account.auth) {
        (Provider::Claude, Auth::Cli) => claude_cli::run(ctx).await,
        (Provider::Openai, Auth::Cli) => codex_cli::run(ctx, &ctx.task.prompt, ctx.model.cli.as_deref(), None).await,
        (Provider::Claude, Auth::Api) => api::anthropic(ctx).await,
        (Provider::Openai, Auth::Api) => api::openai(ctx).await,
        // The Gemini CLI takes API keys too, so both go through it.
        (Provider::Gemini, _) => gemini_cli::run(ctx).await,
    }
}

pub(crate) fn is_limit(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "usage limit", "rate limit", "rate_limit", "hit your limit", "limit reached", "quota", "too many requests",
        "resource exhausted", "resource_exhausted", "429",
    ]
        .iter()
        .any(|w| m.contains(w))
}

pub(crate) fn is_auth(msg: &str) -> bool {
    let m = msg.to_lowercase();
    [
        "/login", "log in", "logged in", "login", "authenticat", "api key", "unauthorized", "401", "credit balance",
        "not signed in", "auth method",
    ]
        .iter()
        .any(|w| m.contains(w))
}

/// Turns a CLI's final error text into a status.
pub(crate) fn classify_error(msg: String, resets: Option<u64>) -> RunResult {
    if is_limit(&msg) {
        RunResult::limited(resets)
    } else {
        let retry = is_auth(&msg);
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
