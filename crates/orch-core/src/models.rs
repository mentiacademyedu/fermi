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
}

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
}

fn m(id: &str, caps: &[Cap], tier: u8, auto: bool, cli: Option<&str>, api: &str) -> ModelSpec {
    ModelSpec {
        id: id.into(),
        caps: caps.to_vec(),
        tier,
        auto,
        cli: cli.map(Into::into),
        api: Some(api.into()),
    }
}

/// Built-in catalog. Order breaks ties in auto mode. Users override or extend
/// it with `[[models]]` in config.
pub fn builtin() -> Vec<ModelSpec> {
    use Cap::*;
    vec![
        m("claude:fable", &[Code, Reasoning, General], 4, false, Some("fable"), "claude-fable-5-1"),
        m("claude:opus", &[Code, Reasoning, General], 3, true, Some("opus"), "claude-opus-5-5"),
        m("claude:sonnet", &[Code, Reasoning, General], 2, true, Some("sonnet"), "claude-sonnet-5-5"),
        m("claude:haiku", &[Code, General, Fast], 1, true, Some("haiku"), "claude-haiku-5-5"),
        m("openai:gpt", &[Code, Reasoning, General], 3, true, None, "gpt-6-astra"),
        m("openai:image", &[Image], 2, true, None, "gpt-image-2.5-flare"),
    ]
}
