//! Who is each account logged in as? Asks the CLI itself where possible.

use crate::config::{Account, Auth, Config, Provider};
use crate::util::resolve_bin;
use std::process::Stdio;
use std::time::Duration;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct AuthInfo {
    /// None = couldn't tell.
    pub logged_in: Option<bool>,
    pub email: Option<String>,
    pub plan: Option<String>,
}

/// The plan from Claude Code's account profile (`.claude.json` in the config
/// dir, or `~/.claude.json` for the default login), e.g. "Max 5x".
fn claude_plan(config_dir: Option<std::path::PathBuf>) -> Option<String> {
    let path = match config_dir {
        Some(dir) => dir.join(".claude.json"),
        None => dirs::home_dir()?.join(".claude.json"),
    };
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let account = &v["oauthAccount"];
    let org = account["organizationType"].as_str()?;
    Some(plan_label(org, account["organizationRateLimitTier"].as_str().unwrap_or("")))
}

fn plan_label(org_type: &str, tier: &str) -> String {
    let base = match org_type {
        "claude_max" => "Max".to_string(),
        "claude_pro" => "Pro".to_string(),
        "claude_team" => "Team".to_string(),
        "claude_enterprise" => "Enterprise".to_string(),
        other => title_case(other.trim_start_matches("claude_")),
    };
    match ["20x", "5x"].iter().find(|m| tier.contains(&format!("max_{m}"))) {
        Some(m) if base == "Max" => format!("Max {m}"),
        _ => base,
    }
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

pub async fn probe(cfg: &Config, a: &Account) -> AuthInfo {
    let fallback = AuthInfo { logged_in: a.credentials_present(), ..Default::default() };
    match (a.auth, a.provider) {
        (Auth::Api, _) => AuthInfo { plan: Some("api key".into()), ..fallback },
        (Auth::Cli, Provider::Openai) => fallback,
        (Auth::Cli, Provider::Gemini) => {
            // The CLI records the signed-in Google account next to its credentials.
            let root = a.home_dir().or_else(dirs::home_dir);
            let email = root
                .and_then(|r| std::fs::read(r.join(".gemini").join("google_accounts.json")).ok())
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                .and_then(|v| v["active"].as_str().map(str::to_string));
            let plan = (fallback.logged_in == Some(true)).then(|| "google".to_string());
            AuthInfo { email, plan, ..fallback }
        }
        (Auth::Cli, Provider::Claude) => {
            let Ok(bin) = resolve_bin("claude", cfg.bins.claude.as_deref()) else { return fallback };
            let mut cmd = tokio::process::Command::new(bin);
            cmd.args(["auth", "status", "--json"])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            if let Some(home) = a.home_dir() {
                cmd.env("CLAUDE_CONFIG_DIR", home);
            }
            #[cfg(windows)]
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(20), cmd.output()).await else { return fallback };
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else { return fallback };
            let s = |k: &str| v[k].as_str().map(str::to_string);
            // The token's `subscriptionType` can disagree with the account
            // (a Max account reported as "team"), so the account profile wins.
            let plan = claude_plan(a.home_dir()).or_else(|| s("subscriptionType").map(|p| title_case(&p)));
            AuthInfo { logged_in: v["loggedIn"].as_bool().or(fallback.logged_in), email: s("email"), plan }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::plan_label;

    #[test]
    fn plan_labels() {
        assert_eq!(plan_label("claude_max", "default_claude_max_5x"), "Max 5x");
        assert_eq!(plan_label("claude_max", "default_claude_max_20x"), "Max 20x");
        assert_eq!(plan_label("claude_pro", ""), "Pro");
        assert_eq!(plan_label("claude_team", "default_raven"), "Team");
    }
}
