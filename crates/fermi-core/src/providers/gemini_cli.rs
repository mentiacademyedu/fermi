//! `gemini -p --output-format stream-json`, one config root per account
//! (`GEMINI_CLI_HOME`; the CLI keeps its state in `<root>/.gemini`).
//! API-key accounts run the same CLI with `GEMINI_API_KEY` set.

use super::{classify_error, RunCtx, RunResult, Usage};
use crate::config::Auth;
use crate::events::{emit, Event};
use crate::trace::Ops;
use anyhow::Result;

/// Gemini's approval modes, derived from the profile's Claude permission mode
/// so one setting covers both.
fn approval_mode(permission_mode: Option<&str>) -> &'static str {
    match permission_mode {
        Some("bypassPermissions") => "yolo",
        Some("plan") => "plan",
        Some("acceptEdits") | Some("auto") => "auto_edit",
        _ => "default",
    }
}

pub async fn run(ctx: &RunCtx<'_>) -> Result<RunResult> {
    let e = ctx.engine;
    let bin = ctx.bin("gemini", e.cfg.bins.gemini.as_deref())?;
    let mut args: Vec<String> = vec![
        "--output-format".into(),
        "stream-json".into(),
        "--skip-trust".into(),
        "--approval-mode".into(),
        approval_mode(ctx.permission()).into(),
    ];
    if let Some(m) = &ctx.model.cli {
        args.extend(["--model".into(), m.clone()]);
    }
    match &ctx.task.resume {
        Some(id) => args.extend(["--resume".into(), id.clone()]),
        None => {
            let id = ctx.new_session()?;
            args.extend(["--session-id".into(), id.clone()]);
            emit(ctx.tx, Event::Session { id });
        }
    }
    args.extend(ctx.profile.gemini_args.iter().cloned());
    // The prompt goes through stdin; -p switches to headless mode and is
    // appended to it, so a blank one adds nothing.
    args.extend(["-p".into(), " ".into()]);

    let mut envs = vec![("FERMI_DEPTH".to_string(), (e.depth + 1).to_string())];
    if let Some(home) = ctx.account.home_dir() {
        std::fs::create_dir_all(&home)?;
        envs.push(("GEMINI_CLI_HOME".into(), home.display().to_string()));
    }
    if ctx.account.auth == Auth::Api {
        envs.push(("GEMINI_API_KEY".into(), super::api::api_key(ctx.account)?));
    }

    let mut p = ctx.spawn(&bin, &args, &envs, &ctx.task.input()).await?;
    let mut ops = Ops::new(ctx);
    // Assistant text arrives as deltas; collect them into one block per turn.
    let (mut text, mut pending, mut usage) = (String::new(), String::new(), Usage::default());
    let (mut error, mut done) = (None::<String>, false);
    let flush = |pending: &mut String, text: &mut String| {
        if !pending.trim().is_empty() {
            emit(ctx.tx, Event::Text { text: pending.trim_end().to_string() });
            *text = std::mem::take(pending);
        }
        pending.clear();
    };

    while let Some(v) = p.next_json().await? {
        match v["type"].as_str().unwrap_or_default() {
            "message" if v["role"] == "assistant" => {
                let chunk = v["content"].as_str().unwrap_or_default();
                pending.push_str(chunk);
                emit(ctx.tx, Event::Delta { text: chunk.to_string() });
            }
            "tool_use" => {
                flush(&mut pending, &mut text);
                ops.tool(v["tool_id"].as_str(), v["tool_name"].as_str().unwrap_or("tool"), &v["parameters"], None);
            }
            "tool_result" => {
                let id = v["tool_id"].as_str().unwrap_or_default();
                if v["status"] == "error" {
                    let msg = v["error"]["message"].as_str().unwrap_or("tool failed");
                    let code = v["error"]["type"].as_str().unwrap_or("tool_error").to_string();
                    ops.result(id, "error", msg, Some(code));
                } else {
                    ops.result(id, "ok", &crate::trace::content_text(&v["output"]), None);
                }
            }
            "error" => {
                let msg = v["message"].as_str().unwrap_or_default().to_string();
                emit(ctx.tx, Event::Log { line: msg.clone() });
                if v["severity"] == "error" {
                    error = Some(msg);
                }
            }
            "result" => {
                done = true;
                let s = &v["stats"];
                usage.input = s["input_tokens"].as_u64().unwrap_or(0);
                usage.cached_input = s["cached"].as_u64().unwrap_or(0);
                usage.output = s["output_tokens"].as_u64().unwrap_or(0);
                usage.reported =
                    Usage::present(s, &[("input_tokens", Usage::INPUT), ("cached", Usage::CACHED), ("output_tokens", Usage::OUTPUT)]);
                // Gemini reports usage per model for the whole run, not per call.
                match s["models"].as_object().filter(|m| !m.is_empty()) {
                    Some(models) => {
                        for (m, u) in models {
                            ops.call(&format!("run:{m}"), Some(m), u);
                        }
                    }
                    None if s.is_object() => ops.call("run", ctx.model.cli.as_deref(), s),
                    None => {}
                }
                if v["status"] == "error" {
                    error = v["error"]["message"].as_str().map(str::to_string).or(error);
                }
            }
            _ => {}
        }
    }
    flush(&mut pending, &mut text);
    ops.finish();

    let (success, stderr) = p.finish().await?;
    let mut res = match error {
        Some(msg) => classify_error(msg, None),
        None if !done || !success => classify_error(if stderr.is_empty() { "gemini exited without a result".into() } else { stderr }, None),
        None => RunResult::ok(text, usage),
    };
    res.usage = usage;
    Ok(res)
}

/// Pre-selects "Login with Google" so the login window goes straight to the
/// browser sign-in instead of asking which method to use.
pub fn prepare_login(home: Option<&std::path::Path>) -> Result<()> {
    let Some(root) = home.map(|h| h.to_path_buf()).or_else(dirs::home_dir) else { return Ok(()) };
    let dir = root.join(".gemini");
    let settings = dir.join("settings.json");
    if !settings.exists() {
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&settings, r#"{ "security": { "auth": { "selectedType": "oauth-personal" } } }"#)?;
    }
    Ok(())
}
