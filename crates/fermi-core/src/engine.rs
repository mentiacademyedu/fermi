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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Task {
    pub prompt: String,
    pub cwd: PathBuf,
    /// Profile name; None = resolve from `cwd`.
    pub profile: Option<String>,
    /// Force a model (`claude:opus`); None = route.
    pub model: Option<String>,
    /// Force an account; disables failover.
    pub account: Option<String>,
    /// Reasoning effort (see `models::EFFORTS`); None = profile default, else the CLI's.
    pub effort: Option<String>,
    /// Output file for image tasks.
    pub out: Option<PathBuf>,
    /// Continue this provider session (a follow-up). Needs `model` and
    /// `account` set to the ones the session lives on.
    pub resume: Option<String>,
    /// Overrides the profile's permission mode for this task
    /// (`acceptEdits`, `bypassPermissions`, `plan`, `default`).
    pub permission: Option<String>,
    /// The conversation so far, for a model that wasn't part of it (after a
    /// model switch, or for an imported session). Sent ahead of the prompt;
    /// routing still looks at the prompt alone.
    pub context: Option<String>,
    /// Extra tool rules the user allowed for this conversation (Claude
    /// `--allowedTools` syntax, e.g. `Bash(node:*)`).
    pub allowed: Vec<String>,
    /// Set when the task is one agent of a team run.
    pub team: Option<TeamRef>,
    /// Run the project's checks after the agent and send failures back to it
    /// (see `verify::run_task`). The hub honors it; `Engine::run` ignores it.
    pub verify: Option<crate::verify::VerifyOpts>,
    /// The hub task this run belongs to, and the task that delegated it (usage attribution).
    pub id: Option<u64>,
    pub parent: Option<u64>,
    /// Which run of the hub task this is (1 = the first; follow-ups and resumes count up).
    pub attempt: u32,
}

/// Which team run and plan task an agent's conversation belongs to.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TeamRef {
    pub run: u64,
    pub key: String,
    pub role: String,
}

/// Permission modes a task may ask for.
pub const PERMISSIONS: [&str; 4] = ["acceptEdits", "bypassPermissions", "plan", "default"];

impl Task {
    pub fn new(prompt: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Task {
            prompt: prompt.into(),
            cwd: cwd.into(),
            profile: None,
            model: None,
            account: None,
            effort: None,
            out: None,
            resume: None,
            permission: None,
            context: None,
            allowed: vec![],
            team: None,
            verify: None,
            id: None,
            parent: None,
            attempt: 1,
        }
    }

    /// What the model is sent: the context (if any), then the prompt.
    pub fn input(&self) -> std::borrow::Cow<'_, str> {
        match &self.context {
            Some(c) => format!(
                "{c}

{}",
                self.prompt
            )
            .into(),
            None => self.prompt.as_str().into(),
        }
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
    /// Set when an economy or quality policy ordered the models.
    pub decision: Option<router::Decision>,
}

pub struct Engine {
    pub cfg: Config,
    pub catalog: Vec<ModelSpec>,
    pub home: PathBuf,
    pub store: Store,
    /// Nesting level: 0 for a user task, 1 inside the MCP bridge. The bridge
    /// is only attached at depth 0 so delegation can't recurse.
    pub depth: u32,
    /// Budget reservations (see `billing`).
    pub ledger: crate::billing::Ledger,
    /// The egress proxy for sandboxed agents, started on first use and kept
    /// for the engine's life (dropping it stops it).
    egress: tokio::sync::OnceCell<Option<crate::sandbox::Proxy>>,
    /// Removes secrets before anything about a task is saved (see `redact`).
    pub redactor: Arc<crate::redact::Redactor>,
    /// Set for a replay: recorded responses stand in for the CLIs (see `trace::replay`).
    pub replay: Option<crate::trace::Replay>,
}

impl Engine {
    pub fn load() -> Result<Arc<Self>> {
        let cfg = Config::load()?;
        let home = Config::home();
        let depth = std::env::var("FERMI_DEPTH").ok().and_then(|d| d.parse().ok()).unwrap_or(0);
        Ok(Arc::new(Engine::new(cfg, home, depth)))
    }

    pub fn new(cfg: Config, home: PathBuf, depth: u32) -> Engine {
        Engine {
            catalog: cfg.catalog(),
            store: Store::open(&home),
            ledger: crate::billing::Ledger::open(&home),
            egress: tokio::sync::OnceCell::new(),
            redactor: Arc::new(
                crate::redact::Redactor::new(&cfg.trace.redact)
                    .unwrap_or_else(|_| crate::redact::Redactor::new(&[]).expect("built-in patterns"))
                    .with_env(),
            ),
            replay: None,
            cfg,
            home,
            depth,
        }
    }

    /// The egress proxy's port for sandboxed agents (None if it couldn't start).
    pub async fn egress_port(&self) -> Option<u16> {
        self.egress
            .get_or_init(|| async {
                let mut allow: Vec<String> = crate::sandbox::PROVIDER_HOSTS.iter().map(|s| s.to_string()).collect();
                allow.extend(self.cfg.security.net_allow.iter().cloned());
                crate::sandbox::Proxy::start(allow, self.home.join("egress.jsonl")).await.ok()
            })
            .await
            .as_ref()
            .map(|p| p.port)
    }

    pub fn plan(&self, task: &Task) -> Result<Plan> {
        if let Some(e) = task.effort.as_deref().filter(|e| !crate::models::EFFORTS.contains(e)) {
            bail!("unknown effort `{e}` (use {})", crate::models::EFFORTS.join(", "));
        }
        if let Some(p) = task.permission.as_deref().filter(|p| !PERMISSIONS.contains(p)) {
            bail!("unknown permission mode `{p}` (use {})", PERMISSIONS.join(", "));
        }
        if task.resume.is_some() && (task.model.is_none() || task.account.is_none()) {
            bail!("a follow-up must name the model and account its session lives on");
        }
        let profile = self.cfg.resolve_profile(task.profile.as_deref(), &task.cwd)?;
        let need = router::classify(&task.prompt);
        let accounts: Vec<&Account> = match &task.account {
            Some(id) => match self.cfg.account(id) {
                Some(a) => vec![a],
                None => bail!("unknown account `{id}`"),
            },
            None => self.cfg.profile_accounts(&profile).collect(),
        };
        let mut models = router::candidates(&self.catalog, &profile, &accounts, &need, task.model.as_deref())?;
        let mut decision = None;
        if profile.routing != crate::config::Routing::Auto && task.model.is_none() && profile.mode != crate::config::Mode::Single {
            let at = now();
            let remaining = match &profile.budget {
                Some(b) => Some(self.ledger.state(crate::billing::budget(&self.cfg, b)?, at).remaining),
                None => None,
            };
            let info = |m: &ModelSpec| {
                let est = crate::billing::estimate(&self.cfg, m, task, at).micros;
                (est, crate::billing::price_at(&self.cfg.prices, &m.id, at).and_then(|(_, p)| p.quality))
            };
            let (ordered, d) = router::policy(profile.routing, models, &need, &info, remaining)?;
            models = ordered;
            decision = Some(d);
        }
        Ok(Plan { profile, need, models, decision })
    }

    /// Next account to try for `provider`. Priority order unless the profile
    /// rotates, in which case the least busy / least used account wins.
    pub fn pick_account(&self, profile: &Profile, provider: Provider, forced: Option<&str>, tried: &HashSet<String>) -> Option<Account> {
        let snap = self.store.snapshot();
        let t = now();
        let mut avail: Vec<&Account> = match forced {
            Some(id) => self.cfg.account(id).into_iter().collect(),
            None => self.cfg.profile_accounts(profile).collect(),
        };
        avail.retain(|a| {
            a.provider == provider
                && !tried.contains(&a.id)
                && (forced.is_some() || (!snap.get(&a.id).is_some_and(|s| s.limited(t)) && a.credentials_present() != Some(false)))
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
            Ok(o) => emit(
                tx,
                Event::Done {
                    ok: true,
                    summary: format!(
                        "{} @ {} · {} in / {} out{}",
                        o.model,
                        o.account,
                        fmt_tokens(o.usage.input),
                        fmt_tokens(o.usage.output),
                        o.usage.cost_usd.map(|c| format!(" · ${c:.4}")).unwrap_or_default()
                    ),
                },
            ),
            Err(e) => emit(tx, Event::Done { ok: false, summary: format!("{e:#}") }),
        }
        r
    }

    async fn run_inner(&self, task: &Task, tx: &Tx) -> Result<Outcome> {
        let plan = self.plan(task)?;
        let profile = &plan.profile;
        let mut tried = HashSet::new();
        let mut last_err: Option<String> = None;
        if let Some(d) = &plan.decision {
            emit(tx, Event::Decision { decision: serde_json::to_value(d).unwrap_or_default() });
        }
        let budget = match &profile.budget {
            Some(b) => Some(crate::billing::budget(&self.cfg, b)?),
            None => None,
        };
        let who = crate::billing::Attribution::of(task);
        let mut attempt = 0u32;

        for model in &plan.models {
            let Some(provider) = model.provider() else { continue };
            'accounts: while let Some(acct) = self.pick_account(profile, provider, task.account.as_deref(), &tried) {
                tried.insert(acct.id.clone());
                attempt += 1;
                // Before sending: the estimate, the context window, the budget.
                let at = now();
                let est = crate::billing::estimate(&self.cfg, model, task, at);
                emit(tx, Event::Estimate { estimate: serde_json::to_value(&est).unwrap_or_default() });
                if est.over_context && !self.cfg.billing.allow_over_context {
                    let why = est.warning.clone().unwrap_or_default();
                    emit(tx, Event::Log { line: format!("not sent to {}: {why}", model.name()) });
                    last_err = Some(why);
                    break 'accounts; // try a model with a larger window
                }
                let reservation = match budget {
                    Some(b) => {
                        let need = est.micros.or(b.reserve).ok_or_else(|| {
                            anyhow::anyhow!(
                                "{} has no price, so budget `{}` can't be checked: add it to [[prices]] or give the budget a `reserve` amount",
                                model.id,
                                b.name
                            )
                        })?;
                        match self.ledger.reserve(b, need, &model.id, &acct.id, &who)? {
                            Ok(r) => {
                                let state = self.ledger.state(b, now());
                                emit(
                                    tx,
                                    Event::Budget {
                                        action: "reserved".into(),
                                        state: serde_json::to_value(&state).unwrap_or_default(),
                                        note: format!("reserved {} for this call", crate::billing::fmt_usd(need)),
                                    },
                                );
                                Some(r)
                            }
                            Err(denied) => {
                                emit(
                                    tx,
                                    Event::Budget {
                                        action: "denied".into(),
                                        state: serde_json::to_value(&denied.state).unwrap_or_default(),
                                        note: denied.to_string(),
                                    },
                                );
                                bail!("{denied}");
                            }
                        }
                    }
                    None => None,
                };
                emit(
                    tx,
                    Event::Routed {
                        profile: profile.name.clone(),
                        model: model.id.clone(),
                        label: model.name().to_string(),
                        effort: task.effort.clone().or_else(|| profile.effort.clone()),
                        account: acct.id.clone(),
                        why: match (&task.resume, &task.model) {
                            (Some(_), _) => "continuing the conversation".into(),
                            (None, _) if task.context.is_some() => "continuing; the conversation so far was passed on".into(),
                            (None, Some(_)) => "chosen".into(),
                            (None, None) => plan.need.why.clone(),
                        },
                    },
                );

                let started = Instant::now();
                self.store.begin(&acct.id);
                let ctx = RunCtx { engine: self, profile, account: &acct, model, task, tx };
                let res = providers::run(&ctx).await.unwrap_or_else(|e| RunResult::failed(format!("{e:#}"), false));
                self.store.end(&acct.id);

                let status = match &res.status {
                    Status::Ok => "ok",
                    Status::Limited { .. } => "limited",
                    Status::Failed { .. } => "failed",
                };
                // The price in effect when the call was made, recorded with it.
                let cost = crate::billing::price_at(&self.cfg.prices, &model.id, at)
                    .map(|(v, p)| crate::billing::cost(v, &model.id, p, &res.usage));
                self.store.record(&UsageRecord {
                    ts: now(),
                    profile: &profile.name,
                    account: &acct.id,
                    model: &model.id,
                    status,
                    input_tokens: res.usage.known(Usage::INPUT, res.usage.input),
                    cached_input_tokens: res.usage.known(Usage::CACHED, res.usage.cached_input),
                    cache_write_tokens: res.usage.known(Usage::CACHE_WRITE, res.usage.cache_write),
                    output_tokens: res.usage.known(Usage::OUTPUT, res.usage.output),
                    cost_usd: res.usage.cost_usd,
                    cost: cost.as_ref(),
                    estimate_micros: est.micros,
                    attempt,
                    who: &who,
                    secs: started.elapsed().as_secs_f64(),
                });
                if let (Some(r), Some(b)) = (reservation, budget) {
                    let (actual, source) = match (&cost, res.usage.cost_usd) {
                        (Some(c), _) => (c.micros, "price"),
                        (None, Some(usd)) => ((usd * 1e6).round() as crate::billing::Micros, "reported"),
                        (None, None) => (r.micros, "estimate"),
                    };
                    let reserved = r.micros;
                    r.settle(actual, source)?;
                    let state = self.ledger.state(b, now());
                    let note = if actual > reserved {
                        format!(
                            "this call cost {}, {} more than its estimate; {} of {} left",
                            crate::billing::fmt_usd(actual),
                            crate::billing::fmt_usd(actual - reserved),
                            crate::billing::fmt_usd(state.remaining.max(0)),
                            crate::billing::fmt_usd(state.limit)
                        )
                    } else {
                        format!(
                            "this call cost {} ({source}); {} of {} left",
                            crate::billing::fmt_usd(actual),
                            crate::billing::fmt_usd(state.remaining.max(0)),
                            crate::billing::fmt_usd(state.limit)
                        )
                    };
                    emit(tx, Event::Budget { action: "settled".into(), state: serde_json::to_value(&state).unwrap_or_default(), note });
                }

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
                    None if a.credentials_present() == Some(false) => format!("not logged in (fermi account login {})", a.id),
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
