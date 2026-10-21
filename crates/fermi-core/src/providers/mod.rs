//! Provider adapters. CLI adapters spawn the official CLIs headless with an
//! isolated config dir per account; API adapters call the HTTP APIs.

mod api;
mod claude_cli;
mod codex_cli;
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
}

impl RunResult {
    pub fn ok(text: String, usage: Usage) -> Self {
        RunResult { status: Status::Ok, text, usage, artifacts: vec![] }
    }
    pub fn failed(msg: String, retry: bool) -> Self {
        RunResult { status: Status::Failed { msg, retry }, text: String::new(), usage: Usage::default(), artifacts: vec![] }
    }
    pub fn limited(until: Option<u64>) -> Self {
        RunResult { status: Status::Limited { until }, text: String::new(), usage: Usage::default(), artifacts: vec![] }
    }
}

pub async fn run(ctx: &RunCtx<'_>) -> Result<RunResult> {
    if ctx.model.image_only() {
        return image::run(ctx).await;
    }
    match (ctx.account.provider, ctx.account.auth) {
        (Provider::Claude, Auth::Cli) => claude_cli::run(ctx).await,
        (Provider::Openai, Auth::Cli) => codex_cli::run(ctx, &ctx.task.prompt, ctx.model.cli.as_deref()).await,
        (Provider::Claude, Auth::Api) => api::anthropic(ctx).await,
        (Provider::Openai, Auth::Api) => api::openai(ctx).await,
    }
}

pub(crate) fn is_limit(msg: &str) -> bool {
    let m = msg.to_lowercase();
    ["usage limit", "rate limit", "rate_limit", "hit your limit", "limit reached", "quota", "too many requests"]
        .iter()
        .any(|w| m.contains(w))
}

pub(crate) fn is_auth(msg: &str) -> bool {
    let m = msg.to_lowercase();
    ["/login", "log in", "logged in", "login", "authenticat", "api key", "unauthorized", "401", "credit balance", "not signed in"]
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
