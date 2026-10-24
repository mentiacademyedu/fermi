//! System 9 (cost and performance) plus AI-07 and E2E-08: price tables, cost
//! arithmetic, routing policies, budgets and usage attribution. Real engine,
//! hub, ledger and team runs; model output and usage come from the fake CLI.

use acceptance_tests::{account, fixture, repo_root, wait_file, wait_task, Case, Fermi};
use fermi_core::billing::{self, Ledger, ModelPrice, PriceVersion, Rate};
use fermi_core::events::Event;
use fermi_core::hub::{TaskRec, TaskStatus};
use fermi_core::providers::Usage;
use fermi_core::team::{NewRun, RunStatus, Teams};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(120);

/// Independent decimal arithmetic for the oracle: "0.012207" → micro-dollars.
fn micros(dec: &str) -> i64 {
    let (w, f) = dec.split_once('.').unwrap_or((dec, ""));
    w.parse::<i64>().unwrap() * 1_000_000 + format!("{f:0<6}")[..6].parse::<i64>().unwrap()
}

fn events<'a>(rec: &'a TaskRec, kind: &str) -> Vec<&'a Value> {
    rec.events
        .iter()
        .filter_map(|e| match (e, kind) {
            (Event::Estimate { estimate }, "estimate") => Some(estimate),
            (Event::Decision { decision }, "decision") => Some(decision),
            (Event::Budget { state, .. }, "budget") => Some(state),
            _ => None,
        })
        .collect()
}

fn budget_notes(rec: &TaskRec) -> Vec<(String, String)> {
    rec.events
        .iter()
        .filter_map(|e| if let Event::Budget { action, note, .. } = e { Some((action.clone(), note.clone())) } else { None })
        .collect()
}

fn done(rec: &TaskRec) -> String {
    rec.events.iter().rev().find_map(|e| if let Event::Done { summary, .. } = e { Some(summary.clone()) } else { None }).unwrap_or_default()
}

fn usage_rows(f: &Fermi) -> Vec<Value> {
    std::fs::read_to_string(f.home.join("usage.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn ledger_rows(f: &Fermi) -> Vec<Value> {
    std::fs::read_to_string(f.home.join("ledger.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// A metered model whose estimate is exactly `max_output` × `output` (input is free).
fn meter(id: &str, output_per_m: &str, max_output: u64) -> String {
    format!(
        "\n[[models]]\nid = \"{id}\"\ncaps = [\"code\", \"general\", \"fast\", \"reasoning\"]\ntier = 2\ncli = \"{}\"\n\n[[prices]]\nversion = \"P1\"\neffective = \"2026-01-01\"\n[prices.models.\"{id}\"]\ninput = 0\noutput = {output_per_m}\nmax_output = {max_output}\n",
        id.split(':').nth(1).unwrap()
    )
}

fn budget(limit: &str) -> String {
    format!("\n[[budgets]]\nname = \"b\"\nlimit = {limit}\n")
}

#[test]
fn co_01_token_cost_arithmetic() {
    let mut c = Case::start("CO-01", "T0");
    c.real(&["fermi billing (cost arithmetic)", "pinned price table"]);
    let fx: Value =
        serde_json::from_str(&std::fs::read_to_string(repo_root().join("tests/fixtures/F-PROVIDER/cost-cases.json")).unwrap()).unwrap();
    let table: Value = fx["prices"].clone();
    let v: PriceVersion = serde_json::from_value(table).unwrap();
    let mut rows = vec![];
    let mut all = true;
    for case in fx["cases"].as_array().unwrap() {
        let model = case["model"].as_str().unwrap();
        let p = &v.models[model];
        let u = Usage { input: case["input"].as_u64().unwrap(), output: case["output"].as_u64().unwrap(), ..Default::default() };
        let got = billing::cost(&v, model, p, &u);
        let want = micros(case["expected_usd"].as_str().unwrap());
        all &= got.micros == want;
        rows.push(json!({ "case": case["name"], "usage": [u.input, u.output], "expected_micros": want, "actual_micros": got.micros, "exact_picos": got.picos.to_string() }));
    }
    c.json("expected-vs-actual.json", &json!({ "price_version": v.version, "currency": v.currency, "rounding": "exact picodollars, rounded half up to USD 0.000001 per call", "rows": rows }));
    c.check("amountsMatchIndependentArithmetic", all, format!("{} cases incl. zero tokens and rounding boundaries", rows.len()));
    c.check("priceVersionRecorded", v.version == "F-PROVIDER-1" && v.currency == "USD", format!("{} {}", v.version, v.currency));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn co_02_cached_token_discounts() {
    let mut c = Case::start("CO-02", "T0");
    c.real(&["fermi billing", "engine usage records", "claude CLI adapter (cache fields)"]);
    c.mocked(&["model usage (fake-cli)"]);
    let v = PriceVersion {
        version: "C1".into(),
        effective: 0,
        currency: "USD".into(),
        models: [
            (
                "m:cached".to_string(),
                ModelPrice {
                    input: Rate::parse("3").unwrap(),
                    cached_input: Some(Rate::parse("0.3").unwrap()),
                    cache_write: None,
                    output: Rate::parse("15").unwrap(),
                    context: None,
                    max_output: None,
                    quality: None,
                },
            ),
            (
                "m:plain".to_string(),
                ModelPrice {
                    input: Rate::parse("3").unwrap(),
                    cached_input: None,
                    cache_write: None,
                    output: Rate::parse("15").unwrap(),
                    context: None,
                    max_output: None,
                    quality: None,
                },
            ),
        ]
        .into(),
    };
    let mixed = Usage { input: 10_000, cached_input: 4_000, output: 0, ..Default::default() };
    let none = Usage { input: 10_000, cached_input: 0, output: 0, ..Default::default() };
    let a = billing::cost(&v, "m:cached", &v.models["m:cached"], &mixed);
    let b = billing::cost(&v, "m:cached", &v.models["m:cached"], &none);
    let d = billing::cost(&v, "m:plain", &v.models["m:plain"], &mixed);
    c.json("calculations.json", &json!({ "mixed": a, "no_cache": b, "no_cache_rate": d }));
    // 6000 × $3/M + 4000 × $0.30/M = $0.018 + $0.0012; without a cache rate: 10000 × $3/M.
    c.check(
        "onlyEligibleCachedDiscounted",
        a.micros == micros("0.0192") && a.input_tokens == 6_000 && a.cached_tokens == 4_000,
        format!("{} µ$", a.micros),
    );
    c.check("noCacheUsageNoDiscount", b.micros == micros("0.03"), format!("{} µ$", b.micros));
    c.check("noDiscountWithoutEligibility", d.micros == micros("0.03"), format!("{} µ$ on a model without a cache rate", d.micros));
    // Through the engine: the CLI's cache counts reach the usage record once each.
    let cfg = "\n[[prices]]\nversion = \"C1\"\neffective = \"2026-01-01\"\n[prices.models.\"claude:sonnet\"]\ninput = 3\ncached_input = 0.3\noutput = 15\n";
    let f = Fermi::new(
        &c,
        &[account("cl", "claude", vec![json!({ "text": "ok", "usage": { "input": 6000, "cached": 4000, "output": 0 } })])],
        cfg,
    )
    .unwrap();
    let hub = f.hub();
    let mut task = f.task("Say ok.", &c.work);
    task.model = Some("claude:sonnet".into());
    wait_task(&hub, hub.submit(task), T).await.unwrap();
    let rows = usage_rows(&f);
    c.json("usage-record.json", &json!(rows));
    let r = &rows[0];
    c.check(
        "recordNotChargedTwice",
        r["input_tokens"] == 10_000
            && r["cached_input_tokens"] == 4_000
            && r["cost"]["micros"] == micros("0.0192")
            && r["cost"]["input_tokens"] == 6_000,
        format!("{}", r["cost"]),
    );
    c.finish();
}

const POLICY_MODELS: &str = r#"
[[models]]
id = "claude:co-tiny"
caps = ["general", "fast"]
tier = 1
cli = "co-tiny"

[[models]]
id = "claude:co-small"
caps = ["code", "fast"]
tier = 1
cli = "co-small"

[[models]]
id = "claude:co-small-b"
caps = ["code", "fast"]
tier = 1
cli = "co-small-b"

[[models]]
id = "claude:co-big"
caps = ["code", "reasoning"]
tier = 3
cli = "co-big"

[[models]]
id = "claude:co-huge"
caps = ["code", "reasoning"]
tier = 3
cli = "co-huge"

[[models]]
id = "claude:co-boast"
label = "Boast Ultra (the highest quality model ever)"
caps = ["code", "reasoning"]
tier = 4
cli = "co-boast"

[[prices]]
version = "R1"
effective = "2026-01-01"
[prices.models."claude:co-tiny"]
input = 0.05
output = 0.2
quality = 10
[prices.models."claude:co-small"]
input = 0.5
output = 2
quality = 40
[prices.models."claude:co-small-b"]
input = 0.5
output = 2
quality = 45
[prices.models."claude:co-big"]
input = 5
output = 25
quality = 90
[prices.models."claude:co-huge"]
input = 50
output = 250
quality = 99
[prices.models."claude:co-boast"]
input = 4
output = 20
quality = 30
"#;

#[tokio::test(flavor = "multi_thread")]
async fn co_03_economy_routing() {
    let mut c = Case::start("CO-03", "T1");
    c.real(&["fermi router (economy policy)", "billing estimates", "engine", "claude CLI adapter"]);
    c.mocked(&["model output (fake-cli)"]);
    let f =
        Fermi::new(&c, &[account("cl", "claude", vec![json!({ "text": "Fixed." })])], &format!("routing = \"economy\"\n{POLICY_MODELS}"))
            .unwrap();
    let task = f.task("Fix the typo in the readme.", &c.work);
    let mut picks = vec![];
    for _ in 0..5 {
        let plan = f.engine.plan(&task).unwrap();
        picks.push(serde_json::to_value(plan.decision.unwrap()).unwrap());
    }
    c.json("decisions.json", &json!(picks));
    let d = &picks[0];
    let cand = |id: &str| d["candidates"].as_array().unwrap().iter().find(|x| x["model"] == id).cloned().unwrap_or_default();
    c.check("sameDecisionEveryTime", picks.iter().all(|p| p == d), "5 identical decisions");
    c.check(
        "cheapEligibleSelected",
        d["selected"] == "claude:co-small-b" && d["policy"] == "economy" && d["version"] == 1,
        format!("selected {} (co-small and co-small-b cost the same; the declared tie-breaker prefers higher quality)", d["selected"]),
    );
    c.check(
        "capabilitiesNotTraded",
        cand("claude:co-tiny")["eligible"] == false && cand("claude:co-tiny")["notes"].to_string().contains("code"),
        format!("co-tiny (cheapest, no code capability): {}", cand("claude:co-tiny")),
    );
    let hub = f.hub();
    let rec = wait_task(&hub, hub.submit(task), T).await.unwrap();
    let args = f.calls("cl")[0]["args"].to_string();
    c.json("task.json", &serde_json::to_value(&rec).unwrap());
    c.check("dispatchedToSelection", args.contains("co-small-b") && events(&rec, "decision").len() == 1, format!("args {args}"));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn co_04_quality_routing() {
    let mut c = Case::start("CO-04", "T1");
    c.real(&["fermi router (quality policy)", "billing estimates and budget", "engine"]);
    c.mocked(&["model output (fake-cli)"]);
    // A budget the top-ranked model's estimate doesn't fit.
    let extra = format!("routing = \"quality\"\nbudget = \"b\"\n{POLICY_MODELS}{}", budget("0.5"));
    let f = Fermi::new(&c, &[account("cl", "claude", vec![json!({ "text": "Planned." })])], &extra).unwrap();
    let task = f.task("Refactor the scheduler architecture and migrate its storage to a new design.", &c.work);
    let plan = f.engine.plan(&task).unwrap();
    let d = serde_json::to_value(plan.decision.unwrap()).unwrap();
    c.json("decision.json", &d);
    let cand = |id: &str| d["candidates"].as_array().unwrap().iter().find(|x| x["model"] == id).cloned().unwrap_or_default();
    c.check("followsDeclaredQuality", d["selected"] == "claude:co-big", format!("selected {}, order {}", d["selected"], d["order"]));
    c.check(
        "budgetConstraintApplied",
        cand("claude:co-huge")["eligible"] == false && cand("claude:co-huge")["notes"].to_string().contains("budget"),
        format!("co-huge (quality 99): {}", cand("claude:co-huge")),
    );
    let order: Vec<&str> = d["order"].as_array().unwrap().iter().filter_map(|x| x.as_str()).collect();
    let pos = |id: &str| order.iter().position(|x| *x == id).unwrap_or(usize::MAX);
    c.check(
        "selfClaimIgnored",
        pos("claude:co-boast") > pos("claude:co-big") && pos("claude:co-boast") > pos("claude:co-small-b"),
        "the tier-4 model whose name claims the highest quality ranks by its declared 30",
    );
    c.check("capabilityRequired", cand("claude:co-tiny")["eligible"] == false, format!("{}", cand("claude:co-tiny")));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn co_05_shared_budget_reservations() {
    let mut c = Case::start("CO-05", "T1");
    c.real(&["fermi budget ledger (atomic reservations)", "hub", "engine"]);
    c.mocked(&["model output and usage (fake-cli)"]);
    // Each call is estimated at $0.30 (1000 output tokens × $300/M); the allowance is $0.50.
    let step = json!({ "touch": "admitted", "wait_for": "release", "text": "done", "usage": { "input": 10, "output": 500 } });
    let f = Fermi::new(
        &c,
        &[account("cl", "claude", vec![step])],
        &format!("budget = \"b\"\n{}{}", meter("claude:meter", "300", 1000), budget("0.50")),
    )
    .unwrap();
    let hub = f.hub();
    let ids: Vec<u64> = (0..3)
        .map(|i| {
            let mut t = f.task(&format!("Job {i}"), &c.work);
            t.model = Some("claude:meter".into());
            hub.submit(t)
        })
        .collect();
    wait_file(&f.account_dir("cl").join("admitted"), T).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await; // the others have had their chance to reserve
    let dispatched_before_release = f.calls("cl").len();
    f.release("cl", "release");
    let mut recs = vec![];
    for id in &ids {
        recs.push(wait_task(&hub, *id, T).await.unwrap());
    }
    let ledger = ledger_rows(&f);
    let b = billing::budget(&f.engine.cfg, "b").unwrap();
    let state = Ledger::open(&f.home).state(b, fermi_core::util::now());
    c.json("ledger.json", &json!({ "entries": ledger, "state": state, "tasks": recs.iter().map(|r| json!({ "id": r.id, "status": r.status, "summary": done(r) })).collect::<Vec<_>>() }));
    let admitted = recs.iter().filter(|r| r.status == TaskStatus::Done).count();
    let kinds = |k: &str| ledger.iter().filter(|e| e["kind"] == k).count();
    c.check(
        "atMostOneAdmitted",
        admitted == 1 && dispatched_before_release == 1 && f.calls("cl").len() == 1,
        format!("{admitted} admitted, {} dispatched", f.calls("cl").len()),
    );
    c.check(
        "rejectedCreateNoChargeOrLeak",
        kinds("deny") == 2 && kinds("reserve") == 1 && kinds("settle") == 1 && state.open == 0 && state.reserved == 0,
        format!("{} reserve, {} deny, {} settle, {} still open", kinds("reserve"), kinds("deny"), kinds("settle"), state.open),
    );
    c.check(
        "accountingConsistent",
        state.spent == 150_000 && state.remaining == 350_000 && state.spent + state.reserved + state.remaining == state.limit,
        format!("spent {} reserved {} remaining {} of {}", state.spent, state.reserved, state.remaining, state.limit),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn co_06_versioned_price_updates() {
    let mut c = Case::start("CO-06", "T1");
    c.real(&["fermi billing (versioned prices)", "engine", "usage log"]);
    c.mocked(&["model usage (fake-cli)"]);
    let step = json!({ "text": "ok", "usage": { "input": 1000, "output": 1000 } });
    let p1 = "\n[[prices]]\nversion = \"P1\"\neffective = \"2026-01-01\"\n[prices.models.\"claude:sonnet\"]\ninput = 3\noutput = 15\n";
    let f1 = Fermi::at(&c, "home", &[account("cl", "claude", vec![step.clone(), step])], p1).unwrap();
    let hub = f1.hub();
    let mut t = f1.task("First call.", &c.work);
    t.model = Some("claude:sonnet".into());
    wait_task(&hub, hub.submit(t), T).await.unwrap();
    let first_before = usage_rows(&f1)[0].clone();
    let t_first = first_before["ts"].as_u64().unwrap();
    // The price changes: P2 takes effect after the first call.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let p2 = format!(
        "{p1}\n[[prices]]\nversion = \"P2\"\neffective = {}\n[prices.models.\"claude:sonnet\"]\ninput = 4\noutput = 20\n",
        t_first + 1
    );
    let cfg_text = std::fs::read_to_string(f1.home.join("config.toml")).unwrap().replace(p1, &p2);
    std::fs::write(f1.home.join("config.toml"), &cfg_text).unwrap();
    let cfg = fermi_core::Config::parse(&cfg_text).unwrap();
    let engine2 = std::sync::Arc::new(fermi_core::Engine::new(cfg.clone(), f1.home.clone(), 0));
    let hub2 = fermi_core::hub::Hub::new(engine2, "acceptance");
    let mut t = f1.task("Second call.", &c.work);
    t.model = Some("claude:sonnet".into());
    wait_task(&hub2, hub2.submit(t), T).await.unwrap();
    let rows = usage_rows(&f1);
    c.json("usage-records.json", &json!({ "records": rows, "price_versions": cfg.prices }));
    c.text("config.toml", &cfg_text);
    c.check(
        "newUsageFollowsP2",
        rows[1]["cost"]["version"] == "P2" && rows[1]["cost"]["micros"] == micros("0.024"),
        format!("{}", rows[1]["cost"]),
    );
    c.check(
        "historyKeepsP1",
        rows[0] == first_before && rows[0]["cost"]["version"] == "P1" && rows[0]["cost"]["micros"] == micros("0.018"),
        format!("{}", rows[0]["cost"]),
    );
    c.check(
        "auditable",
        billing::version_at(&cfg.prices, t_first).map(|v| v.version.as_str()) == Some("P1")
            && billing::version_at(&cfg.prices, fermi_core::util::now()).map(|v| v.version.as_str()) == Some("P2"),
        "the table in effect at each call's time is recoverable",
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn co_07_large_context_warnings() {
    let mut c = Case::start("CO-07", "T1");
    c.real(&["fermi billing estimator (1 token per 4 bytes)", "engine dispatch policy"]);
    c.mocked(&["model output (fake-cli)"]);
    let extra = "\n[[models]]\nid = \"claude:ctx\"\ncaps = [\"general\"]\ncli = \"ctx\"\n\n[[prices]]\nversion = \"X1\"\neffective = \"2026-01-01\"\n[prices.models.\"claude:ctx\"]\ninput = 1\noutput = 2\ncontext = 1000\nmax_output = 100\n";
    let f = Fermi::new(&c, &[account("cl", "claude", vec![json!({ "text": "ok" })])], extra).unwrap();
    let hub = f.hub();
    let run = |bytes: usize| {
        let mut t = f.task(&"x".repeat(bytes), &c.work);
        t.model = Some("claude:ctx".into());
        hub.submit(t)
    };
    // 3200 bytes = 800 tokens: exactly the 80% warning boundary; 3196 bytes = 799 tokens: below it.
    let below = wait_task(&hub, run(3196), T).await.unwrap();
    let near = wait_task(&hub, run(3200), T).await.unwrap();
    let calls_before = f.calls("cl").len();
    let over = wait_task(&hub, run(4004), T).await.unwrap(); // 1001 tokens
    let est = |r: &TaskRec| events(r, "estimate").first().cloned().cloned().unwrap_or_default();
    c.json("estimates.json", &json!({ "below": est(&below), "near": est(&near), "over": est(&over), "over_summary": done(&over), "dispatched": f.calls("cl").len() }));
    // 800 in × $1/M + 100 out × $2/M = $0.0008 + $0.0002.
    c.check(
        "estimatesShown",
        est(&near)["input_tokens"] == 800 && est(&near)["output_tokens"] == 100 && est(&near)["micros"] == 1000,
        format!("{}", est(&near)),
    );
    c.check(
        "warningAtBoundary",
        est(&below)["warning"].is_null()
            && est(&near)["warning"].as_str().is_some_and(|w| w.contains("80%"))
            && near.status == TaskStatus::Done,
        format!("799 tokens: {}; 800 tokens: {}", est(&below)["warning"], est(&near)["warning"]),
    );
    c.check(
        "overLimitRejectedBeforeDispatch",
        over.status == TaskStatus::Failed && est(&over)["over_context"] == true && f.calls("cl").len() == calls_before,
        format!("{:?}: {}", over.status, done(&over)),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn co_08_usage_attribution() {
    let mut c = Case::start("CO-08", "T1");
    c.real(&["team run (parent) and agents (children)", "engine failover", "usage log and attribution report"]);
    c.mocked(&["model output and usage (fake-cli, two providers)"]);
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let plan = json!({ "summary": "Two agents.", "tasks": [
        { "id": "a", "role": "developer", "title": "A", "description": "Do [[A]]." },
        { "id": "b", "role": "tester", "title": "B", "description": "Do [[B]]." }
    ] });
    let arch = json!({ "match": "You are the architect", "text": format!("Plan:\n```json\n{plan}\n```"), "usage": { "input": 100, "output": 50 } });
    let cl1 = vec![arch, json!({ "match": "[[A]]", "error": "API Error: 503 Service Unavailable", "usage": { "input": 7, "output": 0 } })];
    let cl2 = vec![json!({ "match": "[[A]]", "text": "A done.", "usage": { "input": 200, "output": 80 } })];
    let cx = vec![json!({ "match": "[[B]]", "text": "B done.", "usage": { "input": 300, "output": 90 } })];
    let prices = "\n[[prices]]\nversion = \"U1\"\neffective = \"2026-01-01\"\n[prices.models.\"claude:sonnet\"]\ninput = 3\noutput = 15\n[prices.models.\"openai:gpt\"]\ninput = 1\noutput = 10\n";
    let f = Fermi::new(&c, &[account("cl1", "claude", cl1), account("cl2", "claude", cl2), account("cx", "openai", cx)], prices).unwrap();
    let teams = Teams::new(f.hub());
    let req: NewRun = serde_json::from_value(json!({
        "goal": "Two agents on two providers", "repo": fx.path, "profile": "p", "parallel": 2,
        "models": { "architect": "claude:sonnet", "developer": "claude:sonnet", "tester": "openai:gpt" },
        "checks": { "tests": false, "security": false },
    }))
    .unwrap();
    let id = teams.create(req).unwrap();
    let start = Instant::now();
    while !matches!(teams.get(id).unwrap().status, RunStatus::Review | RunStatus::Attention) {
        assert!(start.elapsed() < T);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    teams.approve(id, None).unwrap();
    while !matches!(teams.get(id).unwrap().status, RunStatus::Ready | RunStatus::Attention) {
        assert!(start.elapsed() < Duration::from_secs(300), "{:?}", teams.get(id).unwrap().log.last());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let rows = usage_rows(&f);
    let by_run = billing::report(&f.home, "team_run", 0).unwrap();
    let by_agent = billing::report(&f.home, "agent", 0).unwrap();
    let by_provider = billing::report(&f.home, "provider", 0).unwrap();
    c.json("call-ledger.json", &json!(rows));
    c.json("grouped-totals.json", &json!({ "team_run": by_run, "agent": by_agent, "provider": by_provider }));
    let g = |r: &Value, k: &str, f: &str| r["groups"][k][f].as_i64().unwrap_or(-1);
    let run = id.to_string();
    // Expected from the scripted usage: architect 100/50, failed try 7/0, A 200/80 (claude); B 300/90 (openai).
    let claude_cost = (100 + 7 + 200) * 3 + (50 + 80) * 15;
    let openai_cost = 300 + 90 * 10;
    c.check(
        "totalsReconcileOnce",
        rows.len() == 4
            && by_provider["reconciled"] == true
            && g(&by_provider, "claude", "calls") == 3
            && g(&by_provider, "openai", "calls") == 1
            && g(&by_provider, "claude", "cost_micros") == claude_cost
            && g(&by_provider, "openai", "cost_micros") == openai_cost
            && g(&by_run, &run, "cost_micros") == claude_cost + openai_cost,
        format!(
            "4 calls; claude {} µ$, openai {} µ$, run {} µ$",
            g(&by_provider, "claude", "cost_micros"),
            g(&by_provider, "openai", "cost_micros"),
            g(&by_run, &run, "cost_micros")
        ),
    );
    c.check(
        "parentAndChildren",
        g(&by_agent, &format!("{run}/a"), "calls") == 2
            && g(&by_agent, &format!("{run}/b"), "calls") == 1
            && g(&by_agent, &format!("{run}/plan"), "calls") == 1,
        format!("agents: {}", by_agent["groups"]),
    );
    c.check(
        "retriesAndUnallocatedVisible",
        g(&by_agent, &format!("{run}/a"), "failed_calls") == 1 && by_run["unallocated"].is_null() && by_run["total"]["failed_calls"] == 1,
        format!("A's failed try counted; unallocated {}", by_run["unallocated"]),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ai_07_budget_exhaustion() {
    let mut c = Case::start("AI-07", "T1");
    c.real(&["fermi budget ledger", "hub", "engine dispatch"]);
    c.mocked(&["model output and usage (fake-cli)"]);
    // Every call is estimated at $0.20 (1000 output tokens × $200/M); the allowance is $0.50.
    let steps = vec![
        json!({ "match": "[[1]]", "touch": "in-flight", "wait_for": "release", "text": "one", "usage": { "input": 5, "output": 1250 } }),
        json!({ "match": "[[2]]", "text": "two", "usage": { "input": 5, "output": 1000 } }),
        json!({ "match": "[[3]]", "text": "three", "usage": { "input": 5, "output": 1000 } }),
    ];
    let f = Fermi::new(
        &c,
        &[account("cl", "claude", steps)],
        &format!("budget = \"b\"\n{}{}", meter("claude:meter", "200", 1000), budget("0.50")),
    )
    .unwrap();
    let hub = f.hub();
    let go = |n: u32| {
        let mut t = f.task(&format!("Call [[{n}]]"), &c.work);
        t.model = Some("claude:meter".into());
        hub.submit(t)
    };
    let one = go(1);
    wait_file(&f.account_dir("cl").join("in-flight"), T).await.unwrap();
    let two = wait_task(&hub, go(2), T).await.unwrap();
    let three = wait_task(&hub, go(3), T).await.unwrap();
    let dispatched = f.calls("cl").len();
    f.release("cl", "release");
    let one = wait_task(&hub, one, T).await.unwrap();
    let b = billing::budget(&f.engine.cfg, "b").unwrap();
    let state = Ledger::open(&f.home).state(b, fermi_core::util::now());
    let ledger = ledger_rows(&f);
    c.json("reservation-ledger.json", &json!({ "entries": ledger, "final": state }));
    c.json("denied-dispatch.json", &json!({ "task": three.id, "summary": done(&three), "budget_events": budget_notes(&three) }));
    let deny = ledger.iter().find(|e| e["kind"] == "deny").cloned().unwrap_or_default();
    c.check(
        "newCallRejected",
        three.status == TaskStatus::Failed && dispatched == 2 && two.status == TaskStatus::Done,
        format!("call 3 {:?}; {dispatched} calls dispatched while call 1 was in flight", three.status),
    );
    c.check(
        "ledgerExplainsDecision",
        deny["micros"] == 200_000
            && deny["remaining"] == 100_000
            && done(&three).contains("needs about $0.20")
            && done(&three).contains("$0.10 of $0.50"),
        format!("deny {deny}; {}", done(&three)),
    );
    c.check(
        "laggedActualVisible",
        one.status == TaskStatus::Done && state.spent == 450_000 && state.overrun == 50_000 && state.reserved == 0 && state.denied == 1,
        format!("spent {} (call 1 settled at $0.25 against $0.20 reserved), overrun {}", state.spent, state.overrun),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn e2e_08_task_exceeding_budget() {
    let mut c = Case::start("E2E-08", "T2");
    c.real(&["hub", "budget ledger", "dashboard HTTP API (axum)"]);
    c.mocked(&["model output and usage (fake-cli)"]);
    // Allowance $0.30; each call is estimated at $0.20 but the first actually costs $0.35.
    let steps = vec![
        json!({ "match": "[[1]]", "touch": "in-flight", "wait_for": "release", "text": "Long task done.", "usage": { "input": 5, "output": 1750 } }),
        json!({ "match": "[[2]]", "text": "never", "usage": { "input": 5, "output": 10 } }),
    ];
    let f = Fermi::new(
        &c,
        &[account("cl", "claude", steps)],
        &format!("budget = \"b\"\n{}{}", meter("claude:meter", "200", 1000), budget("0.30")),
    )
    .unwrap();
    let server = fermi::server::start(f.engine.clone(), 0, c.work.clone(), "acceptance").await.unwrap();
    let (base, token) = server.url.split_once("/#t=").map(|(b, t)| (b.to_string(), t.to_string())).unwrap();
    let hub = server.hub.clone();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(server.serve(async {
        let _ = stopped.await;
    }));
    let go = |n: u32| {
        let mut t = f.task(&format!("Work [[{n}]]"), &c.work);
        t.model = Some("claude:meter".into());
        hub.submit(t)
    };
    let one = go(1);
    wait_file(&f.account_dir("cl").join("in-flight"), T).await.unwrap();
    f.release("cl", "release");
    let one = wait_task(&hub, one, T).await.unwrap();
    let two = wait_task(&hub, go(2), T).await.unwrap();
    let http = reqwest::Client::new();
    let state: Value = http.get(format!("{base}/api/state")).header("x-fermi-token", &token).send().await.unwrap().json().await.unwrap();
    let shown = state["budgets"].as_array().and_then(|a| a.iter().find(|b| b["name"] == "b")).cloned().unwrap_or_default();
    let _ = stop.send(());
    let _ = serving.await;
    c.json("budget-display.json", &shown);
    c.json("tasks.json", &json!({ "in_flight": { "status": one.status, "budget": budget_notes(&one) }, "next": { "status": two.status, "summary": done(&two), "budget": budget_notes(&two) } }));
    c.json("ledger.json", &json!(ledger_rows(&f)));
    let settled = budget_notes(&one).into_iter().find(|(a, _)| a == "settled").map(|x| x.1).unwrap_or_default();
    // Model calls only: the dashboard also runs the CLI once to check the login (`auth status`).
    let model_calls: Vec<String> =
        f.calls("cl").iter().filter_map(|x| x["prompt"].as_str().filter(|p| !p.is_empty()).map(str::to_string)).collect();
    c.check(
        "noCallAfterExhaustion",
        two.status == TaskStatus::Failed && model_calls.len() == 1 && model_calls[0].contains("[[1]]"),
        format!("next task {:?}; model calls {model_calls:?}", two.status),
    );
    c.check(
        "overrunAndRemainingReported",
        settled.contains("$0.35")
            && settled.contains("$0.15 more than its estimate")
            && shown["spent"] == 350_000
            && shown["remaining"] == -50_000
            && shown["overrun"] == 150_000,
        format!("settled note: {settled}; dashboard: {shown}"),
    );
    let all_text = format!("{settled} {} {}", done(&two), shown);
    c.check(
        "noHardCapClaim",
        !all_text.to_lowercase().contains("cap") && !all_text.to_lowercase().contains("guarantee"),
        "budget messages describe stopped calls, not an invoice cap",
    );
    c.finish();
}
