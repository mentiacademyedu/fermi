use crate::config::{Account, Config, Profile, Provider};
use crate::events::{emit, Event, Tx};
use crate::models::ModelSpec;
use crate::providers::{self, RunCtx, RunResult, Status, Usage};
use crate::router::{self, Need};
use crate::state::{Store, UsageRecord};
use crate::util::{fmt_tokens, now};
use anyhow::{bail, Result};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct Task {
    pub prompt: String,
    pub cwd: PathBuf,
    /// Profile name; None = resolve from `cwd`.
    pub profile: Option<String>,
    /// Force a model (`claude:opus`); None = route.
    pub model: Option<String>,
    /// Force an account; disables failover.
    pub account: Option<String>,
    /// Output file for image tasks.
    pub out: Option<PathBuf>,
}

impl Task {
    pub fn new(prompt: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Task { prompt: prompt.into(), cwd: cwd.into(), profile: None, model: None, account: None, out: None }
    }
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub text: String,
    pub model: String,
    pub account: String,
    pub usage: Usage,
    pub artifacts: Vec<PathBuf>,
}

pub struct Plan {
    pub profile: Profile,
    pub need: Need,
    pub models: Vec<ModelSpec>,
}

pub struct Engine {
    pub cfg: Config,
    pub catalog: Vec<ModelSpec>,
    pub home: PathBuf,
    pub store: Store,
    /// Nesting level: 0 for a user task, 1 inside the MCP bridge. The bridge
    /// is only attached at depth 0 so delegation can't recurse.
    pub depth: u32,
}

impl Engine {
    pub fn load() -> Result<Arc<Self>> {
        let cfg = Config::load()?;
        let home = Config::home();
        let depth = std::env::var("ORCH_DEPTH").ok().and_then(|d| d.parse().ok()).unwrap_or(0);
        Ok(Arc::new(Engine { catalog: cfg.catalog(), store: Store::open(&home), cfg, home, depth }))
    }

    pub fn plan(&self, task: &Task) -> Result<Plan> {
        let profile = self.cfg.resolve_profile(task.profile.as_deref(), &task.cwd)?;
        let need = router::classify(&task.prompt);
        let accounts: Vec<&Account> = match &task.account {
            Some(id) => match self.cfg.account(id) {
                Some(a) => vec![a],
                None => bail!("unknown account `{id}`"),
            },
            None => self.cfg.profile_accounts(&profile).collect(),
        };
        let models = router::candidates(&self.catalog, &profile, &accounts, &need, task.model.as_deref())?;
        Ok(Plan { profile, need, models })
    }

    /// Next account to try for `provider`. Priority order unless the profile
    /// rotates, in which case the least busy / least used account wins.
    pub fn pick_account(
        &self,
        profile: &Profile,
        provider: Provider,
        forced: Option<&str>,
        tried: &HashSet<String>,
    ) -> Option<Account> {
        let snap = self.store.snapshot();
        let t = now();
        let mut avail: Vec<&Account> = match forced {
            Some(id) => self.cfg.account(id).into_iter().collect(),
            None => self.cfg.profile_accounts(profile).collect(),
        };
        avail.retain(|a| {
            a.provider == provider
                && !tried.contains(&a.id)
                && (forced.is_some()
                    || (!snap.get(&a.id).is_some_and(|s| s.limited(t)) && a.credentials_present() != Some(false)))
        });
        if profile.rotate {
            avail.sort_by(|a, b| {
                let (sa, sb) = (snap.get(&a.id).cloned().unwrap_or_default(), snap.get(&b.id).cloned().unwrap_or_default());
                (self.store.running(&a.id), sa.utilization.unwrap_or(0.0), sa.last_used)
                    .partial_cmp(&(self.store.running(&b.id), sb.utilization.unwrap_or(0.0), sb.last_used))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        avail.first().map(|a| (*a).clone())
    }

    /// Runs a task with failover inside its profile. Always emits exactly one
    /// `Done` event.
    pub async fn run(&self, task: &Task, tx: &Tx) -> Result<Outcome> {
        let r = self.run_inner(task, tx).await;
        match &r {
            Ok(o) => emit(tx, Event::Done {
                ok: true,
                summary: format!(
                    "{} @ {} · {} in / {} out{}",
                    o.model,
                    o.account,
                    fmt_tokens(o.usage.input),
                    fmt_tokens(o.usage.output),
                    o.usage.cost_usd.map(|c| format!(" · ${c:.4}")).unwrap_or_default()
                ),
            }),
            Err(e) => emit(tx, Event::Done { ok: false, summary: format!("{e:#}") }),
        }
        r
    }

    async fn run_inner(&self, task: &Task, tx: &Tx) -> Result<Outcome> {
        let plan = self.plan(task)?;
        let profile = &plan.profile;
        let mut tried = HashSet::new();
        let mut last_err: Option<String> = None;

        for model in &plan.models {
            let Some(provider) = model.provider() else { continue };
            while let Some(acct) = self.pick_account(profile, provider, task.account.as_deref(), &tried) {
                tried.insert(acct.id.clone());
                emit(tx, Event::Routed {
                    profile: profile.name.clone(),
                    model: model.id.clone(),
                    account: acct.id.clone(),
                    why: if task.model.is_some() { "forced".into() } else { plan.need.why.clone() },
                });

                let started = Instant::now();
                self.store.begin(&acct.id);
                let ctx = RunCtx { engine: self, profile, account: &acct, model, task, tx };
                let res = providers::run(&ctx)
                    .await
                    .unwrap_or_else(|e| RunResult::failed(format!("{e:#}"), false));
                self.store.end(&acct.id);

                let status = match &res.status {
                    Status::Ok => "ok",
                    Status::Limited { .. } => "limited",
                    Status::Failed { .. } => "failed",
                };
                self.store.record(&UsageRecord {
                    ts: now(),
                    profile: &profile.name,
                    account: &acct.id,
                    model: &model.id,
                    status,
                    input_tokens: res.usage.input,
                    output_tokens: res.usage.output,
                    cost_usd: res.usage.cost_usd,
                    secs: started.elapsed().as_secs_f64(),
                });

                match res.status {
                    Status::Ok => {
                        return Ok(Outcome {
                            text: res.text,
                            model: model.id.clone(),
                            account: acct.id.clone(),
                            usage: res.usage,
                            artifacts: res.artifacts,
                        })
                    }
                    Status::Limited { until } => {
                        self.store.mark_limited(&acct.id, until);
                        let reason = "usage limit reached".to_string();
                        emit(tx, Event::Failover { account: acct.id.clone(), reason: reason.clone() });
                        last_err = Some(format!("{}: {reason}", acct.id));
                    }
                    Status::Failed { msg, retry: true } => {
                        emit(tx, Event::Failover { account: acct.id.clone(), reason: msg.clone() });
                        last_err = Some(format!("{}: {msg}", acct.id));
                    }
                    Status::Failed { msg, retry: false } => bail!("{msg}"),
                }
            }
        }
        match last_err {
            Some(e) => bail!("all accounts in profile `{}` failed; last: {e}", profile.name),
            None => bail!("no usable account in profile `{}`: {}", profile.name, self.why_unavailable(&plan)),
        }
    }

    fn why_unavailable(&self, plan: &Plan) -> String {
        let snap = self.store.snapshot();
        let t = now();
        let providers: HashSet<_> = plan.models.iter().filter_map(|m| m.provider()).collect();
        let reasons: Vec<String> = self
            .cfg
            .profile_accounts(&plan.profile)
            .filter(|a| providers.contains(&a.provider))
            .map(|a| {
                let why = match snap.get(&a.id).and_then(|s| s.limited_until).filter(|u| *u > t) {
                    Some(u) => format!("limited for {}", crate::util::fmt_dur(u - t)),
                    None if a.credentials_present() == Some(false) && a.auth == crate::config::Auth::Api => {
                        format!("{} not set", a.api_key_env.as_deref().unwrap_or("key env"))
                    }
                    None if a.credentials_present() == Some(false) => format!("not logged in (orch account login {})", a.id),
                    None => "unavailable".into(),
                };
                format!("{} {why}", a.id)
            })
            .collect();
        if reasons.is_empty() {
            let names: Vec<_> = providers.iter().map(|p| p.as_str()).collect();
            format!("it has no {} account", names.join("/"))
        } else {
            reasons.join("; ")
        }
    }
}
