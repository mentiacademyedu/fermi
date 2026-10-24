//! Prices, cost, budgets and usage attribution.
//!
//! **Prices** are versioned tables in `config.toml`, in USD per million
//! tokens; a call uses the version in effect when it ran and records it, so
//! later price changes never rewrite history:
//!
//! ```toml
//! [[prices]]
//! version = "2026-10"
//! effective = "2026-10-01T00:00:00Z"
//! [prices.models."claude:sonnet"]
//! input = 3.0             # per 1M uncached input tokens
//! cached_input = 0.3      # per 1M input tokens read from the prompt cache
//! cache_write = 3.75      # per 1M input tokens written to the cache
//! output = 15.0           # per 1M output tokens
//! context = 200000        # context window, for warnings
//! quality = 80            # declared rank for quality routing (higher is better)
//! ```
//!
//! **Arithmetic** is exact: rates are held in picodollars (1e-12 USD) per
//! token and a call's parts are summed exactly, then rounded once, half up, to
//! the micro-dollar (USD 0.000001). Cached input is discounted only when the
//! model's price has a `cached_input` rate; otherwise it's charged as input.
//!
//! **Estimates** use one token per 4 bytes of UTF-8 (rounded up) for input and
//! the model's `max_output` (default 4096) for output. They're used before a
//! call: for context-window warnings, budget reservations and routing.
//!
//! **Budgets** (`[[budgets]]`, referenced by a profile's `budget`) are
//! enforced with reservations in an append-only ledger (`~/.fermi/ledger.jsonl`):
//! a call reserves its estimate first and is refused when the reservation
//! doesn't fit; when it ends, the actual cost settles the reservation. A call
//! can still cost more than estimated (an agent run's length isn't known in
//! advance), so a budget stops new calls; it is not a hard cap on an invoice.

use crate::config::Config;
use crate::engine::Task;
use crate::models::ModelSpec;
use crate::providers::Usage;
use crate::util::now;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Money in micro-dollars (USD 1e-6).
pub type Micros = i64;
/// Output tokens assumed when a model's price doesn't say.
pub const DEFAULT_OUTPUT_ESTIMATE: u64 = 4096;
/// Share of the context window at which a request gets a warning, by default.
pub const DEFAULT_CONTEXT_WARN: f64 = 0.8;
/// Open reservations older than this stop counting (their process is gone).
const RESERVATION_TTL: u64 = 24 * 3600;

pub fn usd(m: Micros) -> f64 {
    m as f64 / 1e6
}

pub fn fmt_usd(m: Micros) -> String {
    let sign = if m < 0 { "-" } else { "" };
    let a = m.unsigned_abs();
    let (whole, frac) = (a / 1_000_000, a % 1_000_000);
    // Cents when that's exact, else all six digits.
    if frac % 10_000 == 0 {
        format!("{sign}${whole}.{:02}", frac / 10_000)
    } else {
        format!("{sign}${whole}.{frac:06}")
    }
}

// ── prices ──────────────────────────────────────────────────────────────────

/// A rate in picodollars per token, from USD per million tokens (at most six decimals).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Rate(pub u64);

impl Rate {
    pub fn parse(s: &str) -> Result<Rate> {
        let s = s.trim();
        let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
        if whole.is_empty() && frac.is_empty() || !whole.chars().chain(frac.chars()).all(|c| c.is_ascii_digit()) {
            bail!("`{s}` isn't a price (USD per million tokens, e.g. 3.75)");
        }
        if frac.len() > 6 {
            bail!("`{s}` has more than six decimals");
        }
        let w: u64 = if whole.is_empty() { 0 } else { whole.parse()? };
        let f: u64 = format!("{frac:0<6}").parse()?;
        // USD per 1M tokens = 1e6 picodollars per token.
        Ok(Rate(w * 1_000_000 + f))
    }
}

impl<'de> Deserialize<'de> for Rate {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        // Floats print in their shortest exact form (0.3 → "0.3"), so the decimal survives.
        let s = match &v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return Err(serde::de::Error::custom("a price must be a number")),
        };
        Rate::parse(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPrice {
    pub input: Rate,
    #[serde(default)]
    pub cached_input: Option<Rate>,
    #[serde(default)]
    pub cache_write: Option<Rate>,
    pub output: Rate,
    /// Context window in tokens.
    #[serde(default)]
    pub context: Option<u64>,
    /// Output tokens to assume in estimates.
    #[serde(default)]
    pub max_output: Option<u64>,
    /// Declared quality rank for quality routing; higher is better.
    #[serde(default)]
    pub quality: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceVersion {
    pub version: String,
    /// When it takes effect: unix seconds or RFC 3339.
    #[serde(deserialize_with = "time_de")]
    pub effective: u64,
    #[serde(default = "usd_code")]
    pub currency: String,
    #[serde(default)]
    pub models: BTreeMap<String, ModelPrice>,
}

fn usd_code() -> String {
    "USD".into()
}

fn time_de<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<u64, D::Error> {
    match Value::deserialize(d)? {
        Value::Number(n) => n.as_u64().ok_or_else(|| serde::de::Error::custom("bad time")),
        Value::String(s) => crate::sessions::parse_time(&s)
            .or_else(|| crate::sessions::parse_time(&format!("{s}T00:00:00Z")))
            .ok_or_else(|| serde::de::Error::custom(format!("`{s}` isn't a date (use 2026-10-01 or RFC 3339)"))),
        _ => Err(serde::de::Error::custom("bad time")),
    }
}

/// The price version in effect at `at`: the latest one whose `effective` has passed.
pub fn version_at(prices: &[PriceVersion], at: u64) -> Option<&PriceVersion> {
    prices.iter().filter(|p| p.effective <= at).max_by_key(|p| (p.effective, p.version.clone()))
}

/// A model's price in effect at `at`, with its version.
pub fn price_at<'a>(prices: &'a [PriceVersion], model: &str, at: u64) -> Option<(&'a PriceVersion, &'a ModelPrice)> {
    let v = version_at(prices, at)?;
    Some((v, v.models.get(model)?))
}

// ── cost ────────────────────────────────────────────────────────────────────

/// The cost of one call, part by part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub version: String,
    pub currency: String,
    pub model: String,
    /// Token counts: input without cache, read from cache, written to cache, output.
    pub input_tokens: u64,
    pub cached_tokens: u64,
    pub cache_write_tokens: u64,
    pub output_tokens: u64,
    /// Rates applied (picodollars per token) for each part.
    pub rates: [u64; 4],
    /// Exact total in picodollars, and rounded (half up) to micro-dollars.
    pub picos: u128,
    pub micros: Micros,
    /// Usage that didn't add up (cache counts larger than the input), corrected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Rounds picodollars half up to micro-dollars.
pub fn round_micros(picos: u128) -> Micros {
    ((picos + 500_000) / 1_000_000) as Micros
}

/// What `usage` costs at `price`. `usage.input` is every input token,
/// including the cached and cache-written ones.
pub fn cost(v: &PriceVersion, model: &str, p: &ModelPrice, u: &Usage) -> Cost {
    let mut note = None;
    let (mut cached, mut write) = (u.cached_input, u.cache_write);
    if cached + write > u.input {
        note = Some(format!("cache tokens ({}) exceed input tokens ({}); counted only up to the input", cached + write, u.input));
        cached = cached.min(u.input);
        write = write.min(u.input - cached);
    }
    let plain = u.input - cached - write;
    // Without a cache rate there is no discount: those tokens are input.
    let rates = [p.input.0, p.cached_input.unwrap_or(p.input).0, p.cache_write.unwrap_or(p.input).0, p.output.0];
    let counts = [plain, cached, write, u.output];
    let picos: u128 = counts.iter().zip(rates).map(|(n, r)| *n as u128 * r as u128).sum();
    Cost {
        version: v.version.clone(),
        currency: v.currency.clone(),
        model: model.into(),
        input_tokens: plain,
        cached_tokens: cached,
        cache_write_tokens: write,
        output_tokens: u.output,
        rates,
        picos,
        micros: round_micros(picos),
        note,
    }
}

// ── estimates ───────────────────────────────────────────────────────────────

/// fermi's token estimator (v1): one token per 4 bytes of UTF-8, rounded up.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Estimate {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// None when the model has no price.
    pub micros: Option<Micros>,
    pub version: Option<String>,
    pub context: Option<u64>,
    /// Over the warning share of the context window, or over the window.
    pub warning: Option<String>,
    pub over_context: bool,
}

/// What a task will cost on a model, before it runs.
pub fn estimate(cfg: &Config, model: &ModelSpec, task: &Task, at: u64) -> Estimate {
    let input_tokens = estimate_tokens(&task.input());
    let price = price_at(&cfg.prices, &model.id, at);
    let output_tokens = price.and_then(|(_, p)| p.max_output).unwrap_or(DEFAULT_OUTPUT_ESTIMATE);
    let micros =
        price.map(|(v, p)| cost(v, &model.id, p, &Usage { input: input_tokens, output: output_tokens, ..Default::default() }).micros);
    let context = price.and_then(|(_, p)| p.context);
    let warn_at = cfg.billing.context_warn.unwrap_or(DEFAULT_CONTEXT_WARN);
    let (mut warning, mut over) = (None, false);
    if let Some(limit) = context {
        if input_tokens > limit {
            over = true;
            warning = Some(format!("about {input_tokens} input tokens is over {}'s context window ({limit})", model.name()));
        } else if input_tokens as f64 >= limit as f64 * warn_at {
            warning = Some(format!(
                "about {input_tokens} input tokens is {:.0}% of {}'s context window ({limit})",
                input_tokens as f64 / limit as f64 * 100.0,
                model.name()
            ));
        }
    }
    Estimate {
        model: model.id.clone(),
        input_tokens,
        output_tokens,
        micros,
        version: price.map(|(v, _)| v.version.clone()),
        context,
        warning,
        over_context: over,
    }
}

// ── budgets ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Period {
    /// Everything ever spent.
    #[default]
    Total,
    /// Since 00:00 UTC.
    Day,
    /// Since the 1st of the month, UTC.
    Month,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetCfg {
    pub name: String,
    /// Allowance in USD for the period.
    #[serde(deserialize_with = "usd_de")]
    pub limit: Micros,
    #[serde(default)]
    pub period: Period,
    /// What a call reserves when its model has no price (USD); without it such calls are refused.
    #[serde(default, deserialize_with = "opt_usd_de")]
    pub reserve: Option<Micros>,
}

fn usd_de<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Micros, D::Error> {
    let v = Value::deserialize(d)?;
    let s = match &v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Err(serde::de::Error::custom("an amount must be a number")),
    };
    // USD with up to six decimals, through the same exact parser (1 USD = 1e6 micros).
    Rate::parse(&s).map(|r| r.0 as Micros).map_err(serde::de::Error::custom)
}

fn opt_usd_de<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Option<Micros>, D::Error> {
    usd_de(d).map(Some)
}

/// Who a call is for (usage attribution and the ledger).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Attribution {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<u64>,
    /// The task that delegated this call (through the MCP bridge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_run: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl Attribution {
    pub fn of(task: &Task) -> Attribution {
        Attribution {
            task: task.id,
            parent_task: task.parent,
            team_run: task.team.as_ref().map(|t| t.run),
            team_key: task.team.as_ref().map(|t| t.key.clone()),
            role: task.team.as_ref().map(|t| t.role.clone()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    Reserve {
        id: String,
        ts: u64,
        budget: String,
        micros: Micros,
        model: String,
        account: String,
        #[serde(flatten)]
        who: Attribution,
    },
    /// The actual cost of a reserved call. `source`: price | reported | estimate.
    Settle { id: String, ts: u64, budget: String, micros: Micros, estimate: Micros, source: String },
    /// A call refused because its reservation didn't fit.
    Deny {
        ts: u64,
        budget: String,
        micros: Micros,
        remaining: Micros,
        model: String,
        #[serde(flatten)]
        who: Attribution,
    },
}

/// Where a budget stands.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetState {
    pub name: String,
    pub period: Period,
    pub since: u64,
    pub limit: Micros,
    /// Settled actual costs in the period.
    pub spent: Micros,
    /// Open reservations (calls running now).
    pub reserved: Micros,
    pub open: usize,
    /// limit - spent - reserved; negative after an overrun.
    pub remaining: Micros,
    /// How much settled calls cost beyond what they reserved.
    pub overrun: Micros,
    pub denied: usize,
}

fn period_start(p: Period, at: u64) -> u64 {
    match p {
        Period::Total => 0,
        Period::Day => at - at % 86_400,
        Period::Month => {
            // Walk back to the 1st (UTC) by day.
            let mut t = at - at % 86_400;
            while crate::util::day_of_month(t) != 1 {
                t -= 86_400;
            }
            t
        }
    }
}

/// The budget ledger: `~/.fermi/ledger.jsonl`, shared by every fermi process.
pub struct Ledger {
    path: PathBuf,
    lock: Mutex<()>,
}

/// An open reservation; settle it with the actual cost. Dropped unsettled
/// (the call was cancelled), it settles at its estimate.
pub struct Reservation<'a> {
    ledger: &'a Ledger,
    pub id: String,
    pub budget: String,
    pub micros: Micros,
    settled: bool,
}

impl Reservation<'_> {
    pub fn settle(mut self, actual: Micros, source: &str) -> Result<()> {
        self.settled = true;
        self.ledger.settle(&self.id, &self.budget, actual, self.micros, source)
    }
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self.ledger.settle(&self.id, &self.budget, self.micros, self.micros, "estimate (cancelled)");
        }
    }
}

/// A refused reservation, with the numbers behind the decision.
#[derive(Debug, Clone)]
pub struct Denied {
    pub state: BudgetState,
    pub need: Micros,
}

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = &self.state;
        write!(
            f,
            "budget `{}` can't cover this call: it needs about {} and {} of {} is left ({} spent, {} reserved by running calls)",
            s.name,
            fmt_usd(self.need),
            fmt_usd(s.remaining.max(0)),
            fmt_usd(s.limit),
            fmt_usd(s.spent),
            fmt_usd(s.reserved)
        )
    }
}

impl Ledger {
    pub fn open(home: &Path) -> Ledger {
        Ledger { path: home.join("ledger.jsonl"), lock: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> Vec<Entry> {
        std::fs::read_to_string(&self.path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
    }

    /// Runs `f` holding the ledger: this process's lock and a lock file other
    /// fermi processes respect.
    fn locked<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let _g = self.lock.lock().unwrap();
        let lock = self.path.with_extension("lock");
        if let Some(d) = lock.parent() {
            std::fs::create_dir_all(d)?;
        }
        let start = Instant::now();
        loop {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(&lock) {
                Ok(_) => break,
                Err(_) if start.elapsed() > Duration::from_secs(5) => {
                    let _ = std::fs::remove_file(&lock); // a crashed holder
                }
                Err(_) => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        let out = f();
        let _ = std::fs::remove_file(&lock);
        out
    }

    fn append(&self, e: &Entry) -> Result<()> {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        writeln!(f, "{}", serde_json::to_string(e)?)?;
        Ok(())
    }

    /// A budget's state from the ledger.
    pub fn state(&self, b: &BudgetCfg, at: u64) -> BudgetState {
        state_of(&self.entries(), b, at)
    }

    /// Reserves `micros` against a budget, atomically: refused when the
    /// remaining allowance (after other running calls' reservations) can't cover it.
    pub fn reserve(
        &self,
        b: &BudgetCfg,
        micros: Micros,
        model: &str,
        account: &str,
        who: &Attribution,
    ) -> Result<std::result::Result<Reservation<'_>, Denied>> {
        let at = now();
        self.locked(|| {
            let state = state_of(&self.entries(), b, at);
            if micros > state.remaining {
                self.append(&Entry::Deny {
                    ts: at,
                    budget: b.name.clone(),
                    micros,
                    remaining: state.remaining,
                    model: model.into(),
                    who: who.clone(),
                })?;
                let mut state = state;
                state.denied += 1;
                return Ok(Err(Denied { state, need: micros }));
            }
            let id = format!("r-{}", crate::util::uuid_v4());
            self.append(&Entry::Reserve {
                id: id.clone(),
                ts: at,
                budget: b.name.clone(),
                micros,
                model: model.into(),
                account: account.into(),
                who: who.clone(),
            })?;
            Ok(Ok(Reservation { ledger: self, id, budget: b.name.clone(), micros, settled: false }))
        })
    }

    fn settle(&self, id: &str, budget: &str, actual: Micros, estimate: Micros, source: &str) -> Result<()> {
        let at = now();
        self.locked(|| {
            self.append(&Entry::Settle { id: id.into(), ts: at, budget: budget.into(), micros: actual, estimate, source: source.into() })
        })
    }
}

fn state_of(entries: &[Entry], b: &BudgetCfg, at: u64) -> BudgetState {
    let since = period_start(b.period, at);
    let mut open: BTreeMap<&str, (Micros, u64)> = BTreeMap::new();
    let (mut spent, mut overrun, mut denied) = (0, 0, 0);
    for e in entries {
        match e {
            Entry::Reserve { id, ts, budget, micros, .. } if budget == &b.name => {
                open.insert(id, (*micros, *ts));
            }
            Entry::Settle { id, ts, budget, micros, estimate, .. } if budget == &b.name => {
                let reserved_at = open.remove(id.as_str()).map(|r| r.1).unwrap_or(*ts);
                if reserved_at >= since {
                    spent += micros;
                    overrun += (micros - estimate).max(0);
                }
            }
            Entry::Deny { ts, budget, .. } if budget == &b.name && *ts >= since => denied += 1,
            _ => {}
        }
    }
    let live: Vec<Micros> =
        open.values().filter(|(_, ts)| *ts >= since && at.saturating_sub(*ts) < RESERVATION_TTL).map(|(m, _)| *m).collect();
    let reserved: Micros = live.iter().sum();
    BudgetState {
        name: b.name.clone(),
        period: b.period,
        since,
        limit: b.limit,
        spent,
        reserved,
        open: live.len(),
        remaining: b.limit - spent - reserved,
        overrun,
        denied,
    }
}

// ── usage attribution ───────────────────────────────────────────────────────

/// Totals of `usage.jsonl` grouped by `by` (task, team_run, agent, provider,
/// model, account), reconciled: every call counted exactly once, calls with
/// no task shown as unallocated, failed attempts (retries) counted separately.
pub fn report(home: &Path, by: &str, since: u64) -> Result<Value> {
    let text = std::fs::read_to_string(home.join("usage.jsonl")).unwrap_or_default();
    let rows: Vec<Value> =
        text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|v| v["ts"].as_u64().unwrap_or(0) >= since).collect();
    let key = |r: &Value| -> Option<String> {
        let s = |k: &str| r[k].as_str().map(str::to_string).or_else(|| r[k].as_u64().map(|n| n.to_string()));
        match by {
            "task" => s("task"),
            "team_run" | "run" => s("team_run"),
            "agent" => match (s("team_run"), s("team_key")) {
                (Some(run), Some(k)) => Some(format!("{run}/{k}")),
                _ => s("task").map(|t| format!("task {t}")),
            },
            "provider" => s("model").map(|m| m.split(':').next().unwrap_or("").to_string()),
            "model" => s("model"),
            "account" => s("account"),
            other => s(other),
        }
    };
    let mut groups: BTreeMap<String, Value> = BTreeMap::new();
    let mut total = Totals::default();
    for r in &rows {
        let k = key(r).unwrap_or_else(|| "(unallocated)".into());
        let g = groups.entry(k).or_insert_with(|| serde_json::to_value(Totals::default()).unwrap());
        let mut t: Totals = serde_json::from_value(g.clone()).unwrap_or_default();
        t.add(r);
        *g = serde_json::to_value(&t).unwrap();
        total.add(r);
    }
    let sum: u64 = groups.values().map(|g| g["calls"].as_u64().unwrap_or(0)).sum();
    Ok(json!({
        "by": by, "since": since, "groups": groups, "total": total,
        "reconciled": sum == rows.len() as u64,
        "unallocated": groups.get("(unallocated)").cloned(),
    }))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Totals {
    calls: u64,
    /// Calls that didn't succeed (limits, outages): retried elsewhere or failed.
    failed_calls: u64,
    input_tokens: u64,
    cached_tokens: u64,
    output_tokens: u64,
    /// From the price table (micro-dollars).
    cost_micros: Micros,
    /// Calls with no price: their cost isn't in `cost_micros`.
    unpriced_calls: u64,
}

impl Totals {
    fn add(&mut self, r: &Value) {
        self.calls += 1;
        if r["status"] != "ok" {
            self.failed_calls += 1;
        }
        self.input_tokens += r["input_tokens"].as_u64().unwrap_or(0);
        self.cached_tokens += r["cached_input_tokens"].as_u64().unwrap_or(0);
        self.output_tokens += r["output_tokens"].as_u64().unwrap_or(0);
        match r["cost"]["micros"].as_i64() {
            Some(m) => self.cost_micros += m,
            None => self.unpriced_calls += 1,
        }
    }
}

/// Checks a config's billing settings (profiles name existing budgets).
pub fn validate(cfg: &Config) -> Result<()> {
    for p in &cfg.profiles {
        if let Some(b) = &p.budget {
            if !cfg.budgets.iter().any(|x| &x.name == b) {
                bail!("profile `{}` uses budget `{b}`, which isn't defined in [[budgets]]", p.name);
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    for v in &cfg.prices {
        if !seen.insert(&v.version) {
            bail!("two price tables are version `{}`", v.version);
        }
    }
    Ok(())
}

/// A budget by name.
pub fn budget<'a>(cfg: &'a Config, name: &str) -> Result<&'a BudgetCfg> {
    cfg.budgets.iter().find(|b| b.name == name).with_context(|| format!("no budget named `{name}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(input: &str, cached: Option<&str>, output: &str) -> ModelPrice {
        ModelPrice {
            input: Rate::parse(input).unwrap(),
            cached_input: cached.map(|c| Rate::parse(c).unwrap()),
            cache_write: None,
            output: Rate::parse(output).unwrap(),
            context: None,
            max_output: None,
            quality: None,
        }
    }

    fn v() -> PriceVersion {
        PriceVersion { version: "t".into(), effective: 0, currency: "USD".into(), models: BTreeMap::new() }
    }

    #[test]
    fn rates_are_exact() {
        assert_eq!(Rate::parse("3").unwrap().0, 3_000_000);
        assert_eq!(Rate::parse("0.3").unwrap().0, 300_000);
        assert_eq!(Rate::parse("0.000001").unwrap().0, 1);
        assert!(Rate::parse("0.0000001").is_err());
        assert!(Rate::parse("-1").is_err());
    }

    #[test]
    fn costs_round_half_up_once() {
        // 1 token at $0.5/M = 0.5 micro-dollars: rounds up to 1.
        let p = price("0.5", None, "0");
        let c = cost(&v(), "m", &p, &Usage { input: 1, ..Default::default() });
        assert_eq!((c.picos, c.micros), (500_000, 1));
        // 1 token at $0.4/M rounds down to 0.
        let c = cost(&v(), "m", &price("0.4", None, "0"), &Usage { input: 1, ..Default::default() });
        assert_eq!(c.micros, 0);
        assert_eq!(cost(&v(), "m", &p, &Usage::default()).micros, 0);
    }

    #[test]
    fn cache_discount_only_when_priced() {
        let u = Usage { input: 1_000_000, cached_input: 400_000, output: 0, ..Default::default() };
        let with = cost(&v(), "m", &price("3", Some("0.3"), "15"), &u);
        assert_eq!(with.micros, 600_000 * 3 + 400_000 * 3 / 10); // $1.80 + $0.12
        let without = cost(&v(), "m", &price("3", None, "15"), &u);
        assert_eq!(without.micros, 3_000_000);
    }

    #[test]
    fn money_prints_exactly() {
        assert_eq!(fmt_usd(500_000), "$0.50");
        assert_eq!(fmt_usd(1_234_567), "$1.234567");
        assert_eq!(fmt_usd(-20_000), "-$0.02");
    }
}
