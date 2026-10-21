//! `codex exec --json`, one config dir per account (`CODEX_HOME`).

use super::proc::Proc;
use super::{brief, classify_error, RunCtx, RunResult, Usage};
use crate::events::{emit, Event};
use crate::util::resolve_bin;
use anyhow::Result;

pub async fn run(ctx: &RunCtx<'_>, prompt: &str, model: Option<&str>) -> Result<RunResult> {
    let e = ctx.engine;
    let bin = resolve_bin("codex", e.cfg.bins.codex.as_deref())?;
    let mut args: Vec<String> = vec![
        "exec".into(),
        "--json".into(),
        "--skip-git-repo-check".into(),
        "-C".into(),
        ctx.task.cwd.display().to_string(),
    ];
    if let Some(m) = model {
        args.extend(["-m".into(), m.to_string()]);
    }
    if let Some(s) = &ctx.profile.codex_sandbox {
        args.extend(["--sandbox".into(), s.clone()]);
    }
    args.extend(ctx.profile.codex_args.iter().cloned());
    args.push("-".into()); // prompt from stdin

    let mut envs = vec![("ORCH_DEPTH".to_string(), (e.depth + 1).to_string())];
    if let Some(home) = ctx.account.home_dir() {
        std::fs::create_dir_all(&home)?;
        envs.push(("CODEX_HOME".into(), home.display().to_string()));
    }

    let mut p = Proc::spawn(&bin, &args, &envs, &ctx.task.cwd, prompt, ctx.tx).await?;
    let (mut text, mut usage, mut error, mut completed) = (String::new(), Usage::default(), None::<String>, false);

    while let Some(v) = p.next_json().await? {
        match v["type"].as_str().unwrap_or_default() {
            "item.completed" => {
                let item = &v["item"];
                let s = |k: &str| item[k].as_str().unwrap_or_default().to_string();
                match item["type"].as_str().unwrap_or_default() {
                    "agent_message" => {
                        text = s("text");
                        emit(ctx.tx, Event::Text { text: text.clone() });
                    }
                    "command_execution" => emit(ctx.tx, Event::Tool { name: "shell".into(), detail: brief(&item["command"]) }),
                    "file_change" => {
                        let paths: Vec<_> = item["changes"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|c| c["path"].as_str())
                            .collect();
                        emit(ctx.tx, Event::Tool { name: "edit".into(), detail: paths.join(", ") });
                    }
                    "mcp_tool_call" => emit(ctx.tx, Event::Tool { name: format!("{}:{}", s("server"), s("tool")), detail: String::new() }),
                    "web_search" => emit(ctx.tx, Event::Tool { name: "web_search".into(), detail: s("query") }),
                    "error" => emit(ctx.tx, Event::Log { line: s("message") }),
                    _ => {}
                }
            }
            "turn.completed" => {
                completed = true;
                let u = &v["usage"];
                usage.input += u["input_tokens"].as_u64().unwrap_or(0);
                usage.output += u["output_tokens"].as_u64().unwrap_or(0);
            }
            "turn.failed" => error = v["error"]["message"].as_str().map(str::to_string),
            "error" => error = v["message"].as_str().map(str::to_string),
            _ => {}
        }
    }

    let (success, stderr) = p.finish().await?;
    let mut res = match error {
        Some(msg) if !completed => classify_error(msg, None),
        _ if !completed || !success => {
            classify_error(if stderr.is_empty() { "codex exited without completing".into() } else { stderr }, None)
        }
        _ => RunResult::ok(text, usage),
    };
    res.usage = usage;
    Ok(res)
}
