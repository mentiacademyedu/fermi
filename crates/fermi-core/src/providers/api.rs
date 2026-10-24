//! Direct HTTP adapters for API-key accounts. One shared client so
//! connections and TLS sessions are reused across tasks.

use super::{RunCtx, RunResult, Usage};
use crate::config::Account;
use crate::events::{emit, Event};
use crate::util::now;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::sync::OnceLock;

pub(crate) fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().pool_idle_timeout(None).build().expect("http client"))
}

pub(crate) fn api_key(a: &Account) -> Result<String> {
    let var = a.api_key_env.as_deref().ok_or_else(|| anyhow!("account `{}` has no api_key_env", a.id))?;
    std::env::var(var).with_context(|| format!("env var {var} is not set (account `{}`)", a.id))
}

pub(crate) fn base(a: &Account, default: &str) -> String {
    a.base_url.as_deref().unwrap_or(default).trim_end_matches('/').to_string()
}

/// Sends a request; maps 429 to Limited and other failures to Failed.
pub(crate) async fn send(req: reqwest::RequestBuilder) -> Result<std::result::Result<Value, RunResult>> {
    let resp = req.send().await?;
    let status = resp.status();
    if status.as_u16() == 429 {
        let retry = resp.headers().get("retry-after").and_then(|h| h.to_str().ok()).and_then(|s| s.parse::<u64>().ok()).map(|s| now() + s);
        return Ok(Err(RunResult::limited(retry)));
    }
    let body = resp.text().await?;
    if !status.is_success() {
        let msg =
            serde_json::from_str::<Value>(&body).ok().and_then(|v| v["error"]["message"].as_str().map(str::to_string)).unwrap_or(body);
        let retry = matches!(status.as_u16(), 401 | 403 | 500..=599);
        return Ok(Err(RunResult::failed(format!("HTTP {status}: {msg}"), retry)));
    }
    Ok(Ok(serde_json::from_str(&body)?))
}

fn api_model<'a>(ctx: &'a RunCtx<'_>) -> Result<&'a str> {
    ctx.model.api.as_deref().ok_or_else(|| anyhow!("model `{}` has no `api` name", ctx.model.id))
}

pub async fn anthropic(ctx: &RunCtx<'_>) -> Result<RunResult> {
    let key = api_key(ctx.account)?;
    let url = format!("{}/v1/messages", base(ctx.account, "https://api.anthropic.com"));
    let body = json!({
        "model": api_model(ctx)?,
        "max_tokens": 16000,
        "messages": [{ "role": "user", "content": ctx.task.input() }],
    });
    let req = http().post(url).header("x-api-key", key).header("anthropic-version", "2023-06-01").json(&body);
    let v = match send(req).await? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let text: String = v["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    emit(ctx.tx, Event::Text { text: text.clone() });
    let u = &v["usage"];
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    let usage = Usage {
        input: n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens"),
        cached_input: n("cache_read_input_tokens"),
        cache_write: n("cache_creation_input_tokens"),
        output: n("output_tokens"),
        cost_usd: None,
        reported: Usage::present(
            u,
            &[
                ("input_tokens", Usage::INPUT),
                ("cache_read_input_tokens", Usage::CACHED),
                ("cache_creation_input_tokens", Usage::CACHE_WRITE),
                ("output_tokens", Usage::OUTPUT),
            ],
        ),
    };
    Ok(RunResult::ok(text, usage))
}

pub async fn openai(ctx: &RunCtx<'_>) -> Result<RunResult> {
    let key = api_key(ctx.account)?;
    let url = format!("{}/v1/responses", base(ctx.account, "https://api.openai.com"));
    let mut body = json!({ "model": api_model(ctx)?, "input": ctx.task.input() });
    if let Some(e) = ctx.effort() {
        body["reasoning"] = json!({ "effort": e });
    }
    let v = match send(http().post(url).bearer_auth(key).json(&body)).await? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let text: String = v["output"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|o| o["content"].as_array().into_iter().flatten())
        .filter(|c| c["type"] == "output_text")
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    emit(ctx.tx, Event::Text { text: text.clone() });
    let u = &v["usage"];
    let usage = Usage {
        input: u["input_tokens"].as_u64().unwrap_or(0),
        cached_input: u["input_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0),
        output: u["output_tokens"].as_u64().unwrap_or(0),
        reported: Usage::present(u, &[("input_tokens", Usage::INPUT), ("output_tokens", Usage::OUTPUT)])
            | Usage::present(&u["input_tokens_details"], &[("cached_tokens", Usage::CACHED)]),
        ..Default::default()
    };
    Ok(RunResult::ok(text, usage))
}
