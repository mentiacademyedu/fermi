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

pub async fn probe(cfg: &Config, a: &Account) -> AuthInfo {
    let fallback = AuthInfo { logged_in: a.credentials_present(), ..Default::default() };
    match (a.auth, a.provider) {
        (Auth::Api, _) => AuthInfo { plan: Some("api key".into()), ..fallback },
        (Auth::Cli, Provider::Openai) => fallback,
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
            AuthInfo { logged_in: v["loggedIn"].as_bool().or(fallback.logged_in), email: s("email"), plan: s("subscriptionType") }
        }
    }
}
