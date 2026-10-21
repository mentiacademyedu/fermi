//! Image generation. API-key accounts call the Images API directly;
//! ChatGPT-login accounts ask Codex to generate and save the image.

use super::api::{api_key, base, http, send};
use super::{codex_cli, RunCtx, RunResult, Usage};
use crate::config::{Auth, Provider};
use crate::events::{emit, Event};
use crate::util::now;
use anyhow::{anyhow, bail, Result};
use base64::Engine as _;
use serde_json::json;
use std::path::PathBuf;

fn out_path(ctx: &RunCtx<'_>) -> PathBuf {
    match &ctx.task.out {
        Some(p) if p.is_absolute() => p.clone(),
        Some(p) => ctx.task.cwd.join(p),
        None => ctx.task.cwd.join(format!("fermi-image-{}.png", now())),
    }
}

pub async fn run(ctx: &RunCtx<'_>) -> Result<RunResult> {
    if ctx.account.provider != Provider::Openai {
        bail!("image model `{}` needs an openai account", ctx.model.id);
    }
    let out = out_path(ctx);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let mut res = match ctx.account.auth {
        Auth::Api => {
            let key = api_key(ctx.account)?;
            let url = format!("{}/v1/images/generations", base(ctx.account, "https://api.openai.com"));
            let model = ctx.model.api.as_deref().ok_or_else(|| anyhow!("model `{}` has no `api` name", ctx.model.id))?;
            let body = json!({ "model": model, "prompt": ctx.task.prompt, "n": 1 });
            let v = match send(http().post(url).bearer_auth(key).json(&body)).await? {
                Ok(v) => v,
                Err(r) => return Ok(r),
            };
            let b64 = v["data"][0]["b64_json"].as_str().ok_or_else(|| anyhow!("no image in response"))?;
            std::fs::write(&out, base64::engine::general_purpose::STANDARD.decode(b64)?)?;
            let u = &v["usage"];
            RunResult::ok(
                format!("Saved image to {}", out.display()),
                Usage {
                    input: u["input_tokens"].as_u64().unwrap_or(0),
                    output: u["output_tokens"].as_u64().unwrap_or(0),
                    cost_usd: None,
                },
            )
        }
        Auth::Cli => {
            let prompt = format!(
                "Generate an image with your image generation tool and save it to `{}`. \
                 Reply only with the saved path.\n\nImage description:\n{}",
                out.display(),
                ctx.task.prompt
            );
            let mut r = codex_cli::run(ctx, &prompt, ctx.model.cli.as_deref()).await?;
            if matches!(r.status, super::Status::Ok) && !out.is_file() {
                r = RunResult::failed(format!("codex finished but no image was written to {}", out.display()), false);
            }
            r
        }
    };
    if matches!(res.status, super::Status::Ok) {
        emit(ctx.tx, Event::Artifact { path: out.display().to_string() });
        res.artifacts.push(out);
    }
    Ok(res)
}
