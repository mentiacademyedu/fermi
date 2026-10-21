use crate::config::Provider;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    Code,
    Reasoning,
    General,
    Fast,
    Image,
}

fn two() -> u8 {
    2
}
fn yes() -> bool {
    true
}

/// A routable model. `id` is `provider:name`, e.g. `claude:sonnet`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModelSpec {
    pub id: String,
    pub caps: Vec<Cap>,
    /// 1 = fast/cheap, 2 = balanced, 3 = strongest, 4 = premium (never auto-picked by default).
    #[serde(default = "two")]
    pub tier: u8,
    /// Whether auto mode may pick it.
    #[serde(default = "yes")]
    pub auto: bool,
    /// Name passed to the provider CLI (`--model` / `-m`). None = CLI default.
    #[serde(default)]
    pub cli: Option<String>,
    /// Name used with an API key.
    #[serde(default)]
    pub api: Option<String>,
    /// Human name with version, e.g. "Claude Opus 5.5".
    #[serde(default)]
    pub label: Option<String>,
}

/// Reasoning effort levels understood by both `claude --effort` and Codex's
/// `model_reasoning_effort`.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

impl ModelSpec {
    pub fn provider(&self) -> Option<Provider> {
        Provider::parse(self.id.split(':').next().unwrap_or(""))
    }
    pub fn has(&self, cap: Cap) -> bool {
        self.caps.contains(&cap)
    }
    pub fn image_only(&self) -> bool {
        !self.caps.is_empty() && self.caps.iter().all(|c| *c == Cap::Image)
    }
    /// Label if set, else the id.
    pub fn name(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.id)
    }
    pub fn tier_name(&self) -> &'static str {
        match self.tier {
            1 => "fast",
            2 => "balanced",
            3 => "strongest",
            _ => "premium",
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn m(id: &str, label: &str, caps: &[Cap], tier: u8, auto: bool, cli: Option<&str>, api: &str) -> ModelSpec {
    ModelSpec {
        id: id.into(),
        caps: caps.to_vec(),
        tier,
        auto,
        cli: cli.map(Into::into),
        api: Some(api.into()),
        label: Some(label.into()),
    }
}

/// Built-in catalog. Order breaks ties in auto mode. Users override or extend
/// it with `[[models]]` in config. Claude CLI names are aliases that always
/// point at the newest model of that family.
pub fn builtin() -> Vec<ModelSpec> {
    use Cap::*;
    vec![
        m("claude:fable", "Claude Fable 5.1", &[Code, Reasoning, General], 4, false, Some("fable"), "claude-fable-5-1"),
        m("claude:opus", "Claude Opus 5.5", &[Code, Reasoning, General], 3, true, Some("opus"), "claude-opus-5-5"),
        m("claude:sonnet", "Claude Sonnet 5.5", &[Code, Reasoning, General], 2, true, Some("sonnet"), "claude-sonnet-5-5"),
        m("claude:haiku", "Claude Haiku 5.5", &[Code, General, Fast], 1, true, Some("haiku"), "claude-haiku-5-5"),
        m("openai:gpt", "GPT (Codex default)", &[Code, Reasoning, General], 3, true, None, "gpt-6-astra"),
        m("openai:gpt-6.1-sol", "GPT-6.1 Sol", &[Code, Reasoning, General], 3, false, Some("gpt-6.1-sol"), "gpt-6.1-sol"),
        m("openai:gpt-6-astra", "GPT-6 Astra", &[Code, Reasoning, General], 3, false, Some("gpt-6-astra"), "gpt-6-astra"),
        m("openai:gpt-6-luna", "GPT-6 Luna", &[Code, General, Fast], 1, false, Some("gpt-6-luna"), "gpt-6-luna"),
        m("openai:image", "GPT Image 2.5 Flare", &[Image], 2, true, None, "gpt-image-2.5-flare"),
    ]
}
