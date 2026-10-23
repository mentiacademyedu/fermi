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

/// The most recently written image file in `dir`, if any.
fn newest_image(dir: &std::path::Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp"))
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
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
                Usage { input: u["input_tokens"].as_u64().unwrap_or(0), output: u["output_tokens"].as_u64().unwrap_or(0), cost_usd: None },
            )
        }
        Auth::Cli => {
            // Codex's image tool keeps what it makes in
            // `<CODEX_HOME>/generated_images/<session>/`; fermi copies it out,
            // so Codex runs read-only and never needs to write files itself.
            let prompt = format!(
                "Generate exactly one image with your image generation tool from the description below. \
                 Don't run commands or write files: the image is collected automatically. \
                 Reply with one short sentence describing it.\n\nImage description:\n{}",
                ctx.task.prompt
            );
            let mut r = codex_cli::run(ctx, &prompt, ctx.model.cli.as_deref(), Some("read-only")).await?;
            if matches!(r.status, super::Status::Ok) {
                let found =
                    r.session.as_deref().and_then(|s| newest_image(&codex_cli::codex_home(ctx.account)?.join("generated_images").join(s)));
                match found {
                    Some(src) => {
                        std::fs::copy(&src, &out)?;
                    }
                    None => {
                        r = RunResult::failed(
                            "Codex finished without generating an image (this ChatGPT plan may not include image generation)".into(),
                            false,
                        )
                    }
                }
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
