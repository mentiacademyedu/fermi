//! System 12: evaluation and self-improvement.

use acceptance_tests::{account, repo_root, Case, Fermi};
use fermi_core::eval::{self, Dataset, EvalConfig, Report};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const TASKS: [&str; 5] = ["add", "reverse", "fizzbuzz", "palindrome", "word_count"];

fn fixtures() -> PathBuf {
    repo_root().join("tests").join("fixtures").join("F-EVAL")
}

/// A copy of the F-EVAL `smoke` dataset (the pinned one stays untouched).
fn smoke(c: &Case, name: &str) -> Dataset {
    let to = c.work.join("datasets").join(name);
    copy(&fixtures().join("smoke"), &to);
    Dataset::load(&to).unwrap()
}

fn copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        if e.path().is_dir() {
            copy(&e.path(), &to.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

fn solution(task: &str, kind: &str) -> String {
    std::fs::read_to_string(fixtures().join("solutions").join(task).join(format!("{kind}.rs"))).unwrap()
}

/// What a scripted candidate does on a task.
enum Does {
    Right,
    Wrong,
    Nothing,
    Fails,
    /// The right code plus file actions (`ops`).
    RightWith(Value),
    WrongWith(Value),
}

fn steps(plan: &[(&str, Does)], input: u64, output: u64) -> Vec<Value> {
    let mut out: Vec<Value> = plan
        .iter()
        .map(|(task, does)| {
            let edit = |kind: &str| json!([{ "path": "src/lib.rs", "content": solution(task, kind) }]);
            let mut s = json!({ "match": format!("Task {task}:"), "text": format!("Worked on {task}."), "usage": { "input": input, "output": output } });
            match does {
                Does::Right => s["edits"] = edit("correct"),
                Does::Wrong => s["edits"] = edit("wrong"),
                Does::Nothing => {}
                Does::Fails => s["error"] = json!("the model declined the request"),
                Does::RightWith(ops) => {
                    s["edits"] = edit("correct");
                    s["ops"] = ops.clone();
                    s["text"] = json!("");
                }
                Does::WrongWith(ops) => {
                    s["edits"] = edit("wrong");
                    s["ops"] = ops.clone();
                    s["text"] = json!("");
                }
            }
            s
        })
        .collect();
    // An improvement proposal, when asked for one.
    out.push(json!({ "match": "Propose ONE change", "text": "```json\n{\"name\": \"claude-sonnet\", \"model\": \"claude:sonnet\", \"verify\": false, \"rationale\": \"fizzbuzz and palindrome failed their hidden tests on openai:gpt; claude:sonnet solved both in earlier runs\"}\n```" }));
    out
}

fn all(does: fn() -> Does) -> Vec<(&'static str, Does)> {
    TASKS.iter().map(|t| (*t, does())).collect()
}

/// Two accounts: claude solves everything; openai gets fizzbuzz and palindrome wrong.
fn two_models(c: &Case, home: &str) -> Fermi {
    let gpt: Vec<(&str, Does)> =
        TASKS.iter().map(|t| (*t, if matches!(*t, "fizzbuzz" | "palindrome") { Does::Wrong } else { Does::Right })).collect();
    let prices = "\n[[prices]]\nversion = \"E1\"\neffective = \"2026-01-01\"\n[prices.models.\"claude:sonnet\"]\ninput = 3\noutput = 15\n";
    Fermi::at(
        c,
        home,
        &[account("cl", "claude", steps(&all(|| Does::Right), 1000, 200)), account("cx", "openai", steps(&gpt, 800, 150))],
        prices,
    )
    .unwrap()
}

fn config(name: &str, model: &str) -> EvalConfig {
    EvalConfig { name: name.into(), profile: Some("p".into()), model: Some(model.into()), ..Default::default() }
}

fn task<'a>(r: &'a Report, id: &str) -> &'a eval::TaskResult {
    r.tasks.iter().find(|t| t.id == id).unwrap()
}

fn evidence(c: &mut Case, home: &Path, r: &Report, label: &str) {
    c.json(&format!("{label}-report.json"), &serde_json::to_value(r).unwrap());
    let dir = eval::runs_dir(home).join(&r.id);
    for t in &r.tasks {
        c.text(&format!("{label}/{}/patch.diff", t.id), &std::fs::read_to_string(dir.join(&t.patch)).unwrap_or_default());
        for ch in &t.checks {
            c.text(
                &format!("{label}/{}/{}.txt", t.id, ch.name.replace(' ', "-")),
                &std::fs::read_to_string(dir.join(&ch.output)).unwrap_or_default(),
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ev_01_five_task_smoke_evaluation() {
    let mut c = Case::start("EV-01", "T1");
    c.real(&["evaluation runner", "fermi engine", "independent scoring (cargo build and test on a pristine copy)", "git"]);
    c.mocked(&["the candidate model (fake-cli applying scripted solutions)"]);
    let plan = vec![
        ("add", Does::Right),
        ("reverse", Does::Right),
        ("fizzbuzz", Does::Nothing),
        ("palindrome", Does::Fails),
        ("word_count", Does::Right),
    ];
    let f = Fermi::new(&c, &[account("cl", "claude", steps(&plan, 1000, 200))], "").unwrap();
    let ds = smoke(&c, "smoke");
    let r = eval::run(&f.engine, &ds, &config("sonnet", "claude:sonnet")).await.unwrap();
    evidence(&mut c, &f.home, &r, "run");
    let dir = eval::runs_dir(&f.home).join(&r.id);
    let recorded = r.tasks.len() == 5
        && r.tasks.iter().all(|t| {
            t.max_score == 5
                && t.checks.len() == 3
                && dir.join(&t.patch).is_file()
                && t.checks.iter().all(|ch| dir.join(&ch.output).is_file())
        });
    c.check(
        "fiveScoreRecords",
        recorded,
        format!("{} task records, each with a score, its checks and their outputs, and the candidate's patch", r.tasks.len()),
    );
    let line = |id: &str| {
        let t = task(&r, id);
        format!("{id}: {} {}/{}", t.status, t.score, t.max_score)
    };
    c.check(
        "solvedTasksScore",
        ["add", "reverse", "word_count"].iter().all(|id| task(&r, id).status == "passed" && task(&r, id).score == 5),
        format!("{}; {}; {}", line("add"), line("reverse"), line("word_count")),
    );
    let nothing = task(&r, "fizzbuzz");
    let failed = task(&r, "palindrome");
    c.check(
        "noCreditWithoutOutput",
        nothing.status == "no_output" && nothing.score == 0 && nothing.checks.iter().any(|ch| ch.name == "builds" && ch.passed),
        format!("{} (the untouched stub still builds, which earns nothing)", line("fizzbuzz")),
    );
    c.check("noCreditForFailedRun", failed.status == "error" && failed.score == 0, format!("{}: {}", line("palindrome"), failed.note));
    c.check(
        "summary",
        r.summary.score == 15 && r.summary.max_score == 25 && r.summary.passed == 3,
        format!("{}/{} with {} of 5 passed", r.summary.score, r.summary.max_score, r.summary.passed),
    );
    c.finish();
}

/// The fields of a report and of its first task, for comparing schemas.
fn report_keys(r: &Report) -> Vec<String> {
    let v = serde_json::to_value(r).unwrap();
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.extend(v["tasks"][0].as_object().unwrap().keys().map(|x| format!("task.{x}")));
    k.sort();
    k
}

/// A real account and its two models for a live case, or the case blocked.
fn live_setup(c: Case, accounts: &[String], homes: &[&str]) -> Option<(Case, Vec<Fermi>, String, String)> {
    let mut fs = vec![];
    for h in homes {
        match Fermi::live(&c, h, accounts, "") {
            Ok(f) => fs.push(f),
            Err(e) => {
                c.block(&format!("{e:#}"));
                return None;
            }
        }
    }
    match acceptance_tests::live_models(&fs[0]) {
        Some((a, b)) => Some((c, fs, a, b)),
        None => {
            c.block("the live comparison is set up for Claude accounts");
            None
        }
    }
}

/// EV-02 on real models: two models of a real account, one frozen task.
async fn ev_02_live(c: Case, accounts: &[String]) {
    let Some((mut c, fs, ma, mb)) = live_setup(c, accounts, &["home-a", "home-b"]) else { return };
    c.real(&["evaluation runner", "fermi engine", "the provider's own CLI on a real account", "two real models", "billing"]);
    let ds = acceptance_tests::smoke_dataset(&c.work.join("datasets").join("smoke"), 1).unwrap();
    let (ca, cb) = (config("A", &ma), config("B", &mb));
    let (a, b) = tokio::join!(eval::run(&fs[0].engine, &ds, &ca), eval::run(&fs[1].engine, &ds, &cb));
    let (a, b) = (a.unwrap(), b.unwrap());
    evidence(&mut c, &fs[0].home, &a, "a");
    evidence(&mut c, &fs[1].home, &b, "b");
    c.check("sameSchema", report_keys(&a) == report_keys(&b) && a.schema == b.schema, format!("{} fields each", report_keys(&a).len()));
    c.check(
        "sameBasis",
        a.dataset.hash == b.dataset.hash && a.evaluator == b.evaluator && a.limits == b.limits,
        format!("dataset {} {} ({}), evaluator {}", a.dataset.name, a.dataset.version, &a.dataset.hash[..12], a.evaluator),
    );
    c.check(
        "identityRecorded",
        a.tasks[0].model.as_deref() == Some(ma.as_str())
            && b.tasks[0].model.as_deref() == Some(mb.as_str())
            && a.fingerprint != b.fingerprint,
        format!("A {:?} on {:?}, B {:?} on {:?}", a.tasks[0].model, a.tasks[0].account, b.tasks[0].model, b.tasks[0].account),
    );
    let measured = |r: &Report| {
        r.tasks.iter().all(|t| t.latency_ms > 0 && t.input_tokens.is_some_and(|n| n > 0) && t.output_tokens.is_some_and(|n| n > 0))
    };
    c.check(
        "latencyAndUsage",
        measured(&a) && measured(&b),
        format!(
            "A {} ms, {} in / {} out; B {} ms, {} in / {} out (as the provider reported)",
            a.summary.latency_ms,
            a.summary.input_tokens,
            a.summary.output_tokens,
            b.summary.latency_ms,
            b.summary.input_tokens,
            b.summary.output_tokens
        ),
    );
    let stated = |r: &Report| r.summary.cost_micros.is_some() || r.summary.missing.iter().any(|m| m.starts_with("cost:"));
    c.check(
        "costStated",
        stated(&a) && stated(&b),
        format!(
            "A cost {:?}, B cost {:?}; unknown costs listed: {:?} / {:?}",
            a.summary.cost_micros, b.summary.cost_micros, a.summary.missing, b.summary.missing
        ),
    );
    c.check("separateEnvironments", a.id != b.id && fs[0].home != fs[1].home, "each run in its own fermi home and run folder");
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ev_02_cross_model_reports() {
    let c = Case::start("EV-02", "T1 T4");
    let (Some(mut c), live) = acceptance_tests::live_or_block(c) else { return };
    if let Some(accounts) = live {
        return ev_02_live(c, &accounts).await;
    }
    c.real(&["evaluation runner", "fermi engine with two providers' adapters", "billing (price table)"]);
    c.mocked(&["both models (fake-cli speaking the Claude and Codex formats)"]);
    let ds = smoke(&c, "smoke");
    let fa = two_models(&c, "home-a");
    let fb = two_models(&c, "home-b");
    let a = eval::run(&fa.engine, &ds, &config("sonnet", "claude:sonnet")).await.unwrap();
    let b = eval::run(&fb.engine, &ds, &config("gpt", "openai:gpt")).await.unwrap();
    evidence(&mut c, &fa.home, &a, "a");
    evidence(&mut c, &fb.home, &b, "b");
    let keys = |r: &Report| -> Vec<String> {
        let v = serde_json::to_value(r).unwrap();
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.extend(v["tasks"][0].as_object().unwrap().keys().map(|x| format!("task.{x}")));
        k.sort();
        k
    };
    c.check("sameSchema", keys(&a) == keys(&b) && a.schema == b.schema, format!("{} fields each", keys(&a).len()));
    c.check(
        "sameBasis",
        a.dataset.hash == b.dataset.hash && a.evaluator == b.evaluator && a.limits == b.limits,
        format!(
            "dataset {} {} ({}), evaluator {}, limits {}",
            a.dataset.name,
            a.dataset.version,
            &a.dataset.hash[..12],
            a.evaluator,
            a.limits
        ),
    );
    let models = |r: &Report| r.tasks.iter().filter_map(|t| t.model.clone()).collect::<std::collections::BTreeSet<_>>();
    c.check(
        "identityRecorded",
        models(&a).into_iter().collect::<Vec<_>>() == ["claude:sonnet"]
            && models(&b).into_iter().collect::<Vec<_>>() == ["openai:gpt"]
            && a.fingerprint != b.fingerprint,
        format!("A {:?} {} / B {:?} {}", models(&a), a.fingerprint, models(&b), b.fingerprint),
    );
    let measured = |r: &Report| r.tasks.iter().all(|t| t.latency_ms > 0 && t.input_tokens.is_some() && t.output_tokens.is_some());
    c.check(
        "latencyAndUsage",
        measured(&a) && measured(&b),
        format!(
            "A {} ms, {} tokens; B {} ms, {} tokens",
            a.summary.latency_ms,
            a.summary.input_tokens + a.summary.output_tokens,
            b.summary.latency_ms,
            b.summary.input_tokens + b.summary.output_tokens
        ),
    );
    c.check(
        "costAssumptions",
        a.summary.cost_micros.is_some() && b.summary.cost_micros.is_none() && b.summary.missing.iter().any(|m| m.starts_with("cost:")),
        format!(
            "A cost {:?} µ$ (price table E1); B cost unknown ({:?}): no price for openai:gpt",
            a.summary.cost_micros, b.summary.missing
        ),
    );
    c.check("separateEnvironments", a.id != b.id && fa.home != fb.home, "each run in its own fermi home and run folder");
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ev_03_known_failure_detection() {
    let mut c = Case::start("EV-03", "T1");
    c.real(&["evaluation runner", "hidden tests laid over a pristine copy", "cargo test"]);
    c.mocked(&["the candidate (scripted wrong and correct implementations)"]);
    let ds = smoke(&c, "smoke");
    let before = ds.hash();
    let fa = two_models(&c, "home-a");
    let fb = two_models(&c, "home-b");
    let good = eval::run(&fa.engine, &ds, &config("sonnet", "claude:sonnet")).await.unwrap();
    let bad = eval::run(&fb.engine, &ds, &config("gpt", "openai:gpt")).await.unwrap();
    evidence(&mut c, &fa.home, &good, "control");
    evidence(&mut c, &fb.home, &bad, "wrong");
    for id in ["fizzbuzz", "palindrome"] {
        let (w, ok) = (task(&bad, id), task(&good, id));
        let hidden = |t: &eval::TaskResult| t.checks.iter().find(|ch| ch.name == "hidden tests").map(|ch| ch.passed);
        let visible = |t: &eval::TaskResult| t.checks.iter().find(|ch| ch.name == "visible tests").map(|ch| ch.passed);
        let out = std::fs::read_to_string(eval::runs_dir(&fb.home).join(&bad.id).join(&w.checks[2].output)).unwrap_or_default();
        c.check(
            &format!("{id}WrongFails"),
            w.status == "failed" && hidden(w) == Some(false) && visible(w) == Some(true) && w.score == 2 && out.contains("panicked"),
            format!("wrong: {} {}/5 (visible tests pass, hidden fail; the failure output is kept)", w.status, w.score),
        );
        c.check(&format!("{id}ControlPasses"), ok.status == "passed" && ok.score == 5, format!("control: {} {}/5", ok.status, ok.score));
    }
    // The evaluator's assets never reached the candidate, and the dataset is unchanged.
    let work = eval::runs_dir(&fb.home).join(&bad.id).join("tasks/fizzbuzz/work");
    c.check(
        "evaluatorUntouched",
        !work.join("tests/hidden.rs").exists() && ds.hash() == before,
        "the hidden tests weren't in the candidate's folder; the dataset hash is unchanged",
    );
    c.finish();
}

/// EV-04 on real models: the dashboard compares two real configurations.
async fn ev_04_live(c: Case, accounts: &[String]) {
    let Some((mut c, fs, ma, mb)) = live_setup(c, accounts, &["home"]) else { return };
    c.real(&["evaluation runner", "comparison", "the dashboard's HTTP API", "the provider's own CLI on a real account", "two real models"]);
    let ds = acceptance_tests::smoke_dataset(&c.work.join("datasets").join("smoke"), 1).unwrap();
    let f = &fs[0];
    let (ca, cb) = (config("A", &ma), config("B", &mb));
    let (a, b) = tokio::join!(eval::run(&f.engine, &ds, &ca), eval::run(&f.engine, &ds, &cb));
    let (a, b) = (a.unwrap(), b.unwrap());
    let server = fermi::server::start(f.engine.clone(), 0, c.work.clone(), "acceptance").await.unwrap();
    let (base, token) = server.url.split_once("/#t=").map(|(b, t)| (b.to_string(), t.to_string())).unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(server.serve(async {
        let _ = stop_rx.await;
    }));
    let http = reqwest::Client::new();
    let get = |path: String| http.get(format!("{base}{path}")).header("x-fermi-token", &token).send();
    let list: Value = get("/api/evals".into()).await.unwrap().json().await.unwrap();
    let cmp: Value = get(format!("/api/evals/compare?a={}&b={}", a.id, b.id)).await.unwrap().json().await.unwrap();
    let _ = stop_tx.send(());
    let _ = serving.await;
    evidence(&mut c, &f.home, &a, "a");
    evidence(&mut c, &f.home, &b, "b");
    c.json("dashboard-list.json", &list);
    c.json("dashboard-comparison.json", &cmp);
    c.check("listed", list["runs"].as_array().map_or(0, |r| r.len()) == 2, "both runs on the dashboard");
    let raw = |r: &Report| (r.tasks.iter().map(|t| t.score as i64).sum::<i64>(), r.tasks.iter().map(|t| t.latency_ms as i64).sum::<i64>());
    let ((sa, la), (sb, lb)) = (raw(&a), raw(&b));
    let d = &cmp["differences"];
    c.check(
        "reconciles",
        cmp["a"]["summary"]["score"] == sa && cmp["b"]["summary"]["score"] == sb && d["score"] == sb - sa && d["latency_ms"] == lb - la,
        format!("{ma} score {sa} in {la} ms → {mb} score {sb} in {lb} ms (Δ {} points, Δ {} ms)", d["score"], d["latency_ms"]),
    );
    let rows = cmp["tasks"].as_array().cloned().unwrap_or_default();
    let per_task = rows.iter().all(|row| {
        let id = row["task"].as_str().unwrap_or_default();
        row["a"]["score"] == task(&a, id).score && row["b"]["score"] == task(&b, id).score
    });
    c.check("perTask", rows.len() == 1 && per_task, "the task's quality, latency and cost for A and B, matching the reports");
    let cost_ok = match (a.summary.cost_micros, b.summary.cost_micros) {
        (Some(x), Some(y)) => d["cost_micros"] == y - x,
        _ => d["cost_micros"].is_null(),
    };
    c.check(
        "costDifference",
        cost_ok,
        format!("A cost {:?} µ$, B cost {:?} µ$, difference shown: {}", a.summary.cost_micros, b.summary.cost_micros, d["cost_micros"]),
    );
    let note = cmp["note"].as_str().unwrap_or_default();
    c.check(
        "honest",
        cmp["sample_size"] == 1 && note.contains("no statistical significance is claimed"),
        format!("n = {}; missing {}; note: {note}", cmp["sample_size"], cmp["missing"]),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ev_04_configuration_comparison() {
    let c = Case::start("EV-04", "T1 T4");
    let (Some(mut c), live) = acceptance_tests::live_or_block(c) else { return };
    if let Some(accounts) = live {
        return ev_04_live(c, &accounts).await;
    }
    c.real(&["evaluation runner", "comparison", "the dashboard's HTTP API"]);
    c.mocked(&["both configurations' models (fake-cli)"]);
    let ds = smoke(&c, "smoke");
    let f = two_models(&c, "home");
    let a = eval::run(&f.engine, &ds, &config("A sonnet", "claude:sonnet")).await.unwrap();
    let b = eval::run(&f.engine, &ds, &config("B gpt", "openai:gpt")).await.unwrap();
    let server = fermi::server::start(f.engine.clone(), 0, c.work.clone(), "acceptance").await.unwrap();
    let (base, token) = server.url.split_once("/#t=").map(|(b, t)| (b.to_string(), t.to_string())).unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(server.serve(async {
        let _ = stop_rx.await;
    }));
    let http = reqwest::Client::new();
    let get = |path: String| http.get(format!("{base}{path}")).header("x-fermi-token", &token).send();
    let list: Value = get("/api/evals".into()).await.unwrap().json().await.unwrap();
    let cmp: Value = get(format!("/api/evals/compare?a={}&b={}", a.id, b.id)).await.unwrap().json().await.unwrap();
    let _ = stop_tx.send(());
    let _ = serving.await;
    c.json("dashboard-list.json", &list);
    c.json("dashboard-comparison.json", &cmp);
    c.check("listed", list["runs"].as_array().map_or(0, |r| r.len()) == 2, "both runs on the dashboard");
    // Reconcile the dashboard's numbers with the raw task records.
    let raw = |r: &Report| (r.tasks.iter().map(|t| t.score as i64).sum::<i64>(), r.tasks.iter().map(|t| t.latency_ms as i64).sum::<i64>());
    let ((sa, la), (sb, lb)) = (raw(&a), raw(&b));
    let d = &cmp["differences"];
    c.check(
        "reconciles",
        cmp["a"]["summary"]["score"] == sa && cmp["b"]["summary"]["score"] == sb && d["score"] == sb - sa && d["latency_ms"] == lb - la,
        format!("scores {sa} → {sb} (Δ {}), latency {la} → {lb} ms (Δ {})", d["score"], d["latency_ms"]),
    );
    let rows = cmp["tasks"].as_array().cloned().unwrap_or_default();
    let per_task = rows.iter().all(|row| {
        let id = row["task"].as_str().unwrap_or_default();
        row["a"]["score"] == task(&a, id).score
            && row["b"]["score"] == task(&b, id).score
            && row["score_delta"] == task(&b, id).score as i64 - task(&a, id).score as i64
    });
    c.check("perTask", rows.len() == 5 && per_task, "each task's quality, latency and cost for A and B, matching the reports");
    c.check(
        "costDifference",
        cmp["a"]["summary"]["cost_micros"] == a.summary.cost_micros.unwrap_or(-1) && cmp["differences"]["cost_micros"].is_null(),
        format!("A cost {} µ$; B unknown, so no cost difference is shown", cmp["a"]["summary"]["cost_micros"]),
    );
    let note = cmp["note"].as_str().unwrap_or_default();
    c.check(
        "honest",
        cmp["sample_size"] == 5
            && cmp["missing"].as_array().is_some_and(|m| m.iter().any(|x| x.as_str().unwrap_or("").starts_with("B cost")))
            && note.contains("no statistical significance is claimed"),
        format!("n = {}; missing {}; note: {note}", cmp["sample_size"], cmp["missing"]),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ev_05_reproducible_evaluation_setup() {
    let mut c = Case::start("EV-05", "T1");
    c.real(&["evaluation runner", "fixture and dataset hashing", "independent scoring"]);
    c.mocked(&["the model (fake-cli: a deterministic provider)"]);
    let ds = smoke(&c, "smoke");
    let cfg = config("sonnet", "claude:sonnet");
    let one = two_models(&c, "clean-1");
    let two = two_models(&c, "clean-2");
    let r1 = eval::run(&one.engine, &ds, &cfg).await.unwrap();
    let r2 = eval::run(&two.engine, &ds, &cfg).await.unwrap();
    let rep = eval::reproducibility(&r1, &r2);
    c.json("manifest-1.json", &json!({ "dataset": r1.dataset, "fixtures": r1.tasks.iter().map(|t| (t.id.clone(), t.fixture_hash.clone())).collect::<Vec<_>>(), "evaluator": r1.evaluator, "fingerprint": r1.fingerprint, "limits": r1.limits }));
    c.json("manifest-2.json", &json!({ "dataset": r2.dataset, "fixtures": r2.tasks.iter().map(|t| (t.id.clone(), t.fixture_hash.clone())).collect::<Vec<_>>(), "evaluator": r2.evaluator, "fingerprint": r2.fingerprint, "limits": r2.limits }));
    c.json("reproducibility.json", &rep);
    c.check("sameSetup", rep["same_setup"] == true, format!("{}", rep["setup_differences"]));
    c.check(
        "sameScores",
        rep["same_scores"] == true && r1.summary.score == r2.summary.score,
        format!("{} and {} of {}; identical patches", r1.summary.score, r2.summary.score, r1.summary.max_score),
    );
    // A model that answers differently the third time: reported as variation, not averaged away.
    let plan: Vec<(&str, Does)> = TASKS.iter().map(|t| (*t, if *t == "reverse" { Does::Wrong } else { Does::Right })).collect();
    let three = Fermi::at(&c, "clean-3", &[account("cl", "claude", steps(&plan, 1000, 200))], "").unwrap();
    let r3 = eval::run(&three.engine, &ds, &cfg).await.unwrap();
    let var = eval::reproducibility(&r1, &r3);
    c.json("variation.json", &var);
    c.check(
        "variationReported",
        var["same_setup"] == true
            && var["same_scores"] == false
            && var["variation"].as_array().is_some_and(|v| v.len() == 1 && v[0]["task"] == "reverse"),
        format!("same setup, one task varied: {}", var["variation"]),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ev_06_protected_evaluation_assets() {
    let mut c = Case::start("EV-06", "T3");
    c.real(&[
        "evaluation runner",
        "read-only protected assets (file and folder permissions)",
        "pre and post hashes",
        "independent scoring",
    ]);
    c.mocked(&["the candidate (fake-cli doing real file operations)"]);
    let ds = smoke(&c, "smoke");
    let fake_test = "#[test]\nfn visible() {}\n";
    let tamper = json!([
        { "do": { "write": "tests/visible.rs", "content": fake_test } },
        { "do": { "rm": "tests/visible.rs" } },
        { "do": { "replace": "tests/visible.rs", "content": fake_test } },
    ]);
    // `add`: a wrong implementation that tries to weaken the visible test; `reverse`: works around the read-only flag.
    let bypass = json!([{ "do": { "unlock": "tests/visible.rs" } }, { "do": { "write": "tests/visible.rs", "content": fake_test } }]);
    let plan = vec![
        ("add", Does::WrongWith(tamper)),
        ("reverse", Does::RightWith(bypass)),
        ("fizzbuzz", Does::Right),
        ("palindrome", Does::Right),
        ("word_count", Does::Right),
    ];
    let f = Fermi::new(&c, &[account("cl", "claude", steps(&plan, 1000, 200))], "").unwrap();
    let r = eval::run(&f.engine, &ds, &config("sonnet", "claude:sonnet")).await.unwrap();
    evidence(&mut c, &f.home, &r, "run");
    let add = task(&r, "add");
    let failures = add.reply.matches("failed").count();
    c.json("attempts.json", &json!({ "add": add.reply, "reverse": task(&r, "reverse").reply }));
    c.check(
        "writesDenied",
        failures == 3 && !add.reply.contains("wrote") && !add.reply.contains("removed"),
        format!("modify, delete and replace all failed: {}", add.reply.replace('\n', " | ")),
    );
    c.check(
        "hashesUnchanged",
        add.protection.intact && add.protection.before == add.protection.after,
        format!("tests/ hashed {} before and after", &add.protection.before[..12]),
    );
    c.check(
        "scoredOnPristineTests",
        add.status == "failed" && add.score == 2,
        format!("add: {} {}/5 (the wrong code still fails the hidden tests)", add.status, add.score),
    );
    // `reverse` clears the read-only flag first. Where the OS still refuses (Windows holds the
    // files locked) the files stay intact; where it lets the owner through (Unix), the change
    // must invalidate the task instead of counting.
    let rev = task(&r, "reverse");
    let outcome = if rev.protection.intact {
        rev.reply.contains("write tests/visible.rs failed") && rev.status == "passed"
    } else {
        rev.status == "invalidated" && rev.score == 0
    };
    c.check(
        "tamperingInvalidates",
        outcome,
        format!(
            "reverse: {} → {} {}/5{}",
            rev.reply.replace('\n', " | "),
            rev.status,
            rev.score,
            if rev.protection.intact { " (the write was refused; files intact)".to_string() } else { format!(" ({})", rev.note) }
        ),
    );
    c.check("evaluatorIdentity", r.evaluator == eval::EVALUATOR, format!("scored by {}", r.evaluator));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ev_07_dataset_history() {
    let mut c = Case::start("EV-07", "T1");
    c.real(&["dataset registry", "evaluation runner", "versioned reports"]);
    c.mocked(&["the model (fake-cli)"]);
    let f = two_models(&c, "home");
    let v1 = smoke(&c, "smoke-v1");
    let r1 = eval::run(&f.engine, &v1, &config("sonnet", "claude:sonnet")).await.unwrap();
    // Version 2: a stricter hidden test for add, and a new version number.
    let v2dir = c.work.join("datasets").join("smoke-v2");
    copy(&v1.dir, &v2dir);
    let toml = std::fs::read_to_string(v2dir.join("dataset.toml")).unwrap().replace("version = \"1\"", "version = \"2\"");
    std::fs::write(v2dir.join("dataset.toml"), toml).unwrap();
    let hidden = v2dir.join("tasks/add/hidden/tests/hidden.rs");
    let stricter = std::fs::read_to_string(&hidden)
        .unwrap()
        .replace("assert_eq!(add(0, 0), 0);", "assert_eq!(add(0, 0), 0);\n    assert_eq!(add(i64::MIN, 0), i64::MIN);");
    std::fs::write(&hidden, stricter).unwrap();
    let v2 = Dataset::load(&v2dir).unwrap();
    let r2 = eval::run(&f.engine, &v2, &config("sonnet", "claude:sonnet")).await.unwrap();
    let hist = eval::history(&f.home, "smoke");
    c.json("history.json", &hist);
    c.json("registry.json", &json!(eval::datasets(&f.home)));
    let versions = hist["versions"].as_array().cloned().unwrap_or_default();
    let runs_of = |v: &str| versions.iter().find(|x| x["version"] == v).map(|x| x["runs"].as_array().map_or(0, |r| r.len())).unwrap_or(0);
    c.check(
        "twoLineages",
        versions.len() == 2 && runs_of("1") == 1 && runs_of("2") == 1,
        format!("versions {:?}", versions.iter().map(|v| v["version"].clone()).collect::<Vec<_>>()),
    );
    let old = eval::load_report(&f.home, &r1.id).unwrap();
    c.check(
        "oldBoundToV1",
        old.dataset.version == "1" && old.dataset.hash == v1.hash() && r2.dataset.version == "2" && r2.dataset.hash != old.dataset.hash,
        format!("v1 {} · v2 {}", &old.dataset.hash[..12], &r2.dataset.hash[..12]),
    );
    let cross = eval::compare(&r1, &r2);
    c.check(
        "noCrossVersionComparison",
        cross.as_ref().is_err_and(|e| e.to_string().contains("aren't compared across dataset versions")),
        format!("{:?}", cross.err().map(|e| e.to_string())),
    );
    // Changing version 1's contents without a new version is refused.
    std::fs::write(v1.dir.join("tasks/add/hidden/tests/hidden.rs"), "// weakened\n").unwrap();
    let silent = eval::register(&f.home, &Dataset::load(&v1.dir).unwrap());
    c.check(
        "silentChangeRefused",
        silent.as_ref().is_err_and(|e| e.to_string().contains("new version")),
        format!("{:?}", silent.err().map(|e| e.to_string())),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ev_08_evaluated_improvement_proposals() {
    let mut c = Case::start("EV-08", "T1");
    c.real(&["proposal records", "evaluation of the proposal against the baseline", "adoption gate"]);
    c.mocked(&["the proposing agent and the models (fake-cli)"]);
    let ds = smoke(&c, "smoke");
    let f = two_models(&c, "home");
    let gpt = config("gpt", "openai:gpt");
    eval::init_active(&f.home, &gpt, "acceptance test (initial setup)").unwrap();
    let base = eval::run(&f.engine, &ds, &gpt).await.unwrap();
    let fixed = eval::active(&f.home).unwrap().fingerprint;
    let p = eval::propose(&f.engine, std::slice::from_ref(&base.id), Some("p")).await.unwrap();
    c.json("proposal.json", &serde_json::to_value(&p).unwrap());
    let cited: Vec<&str> = p.cites.iter().filter_map(|x| x["task"].as_str()).collect();
    c.check(
        "citesFailures",
        cited == ["fizzbuzz", "palindrome"] && p.cites.iter().all(|x| x["run"] == base.id.as_str()) && p.rationale.contains("fizzbuzz"),
        format!("cites {cited:?} from run {}; rationale: {}", base.id, p.rationale),
    );
    c.check(
        "evaluationPlan",
        p.plan["dataset"]["hash"] == ds.hash()
            && p.plan["baseline"]["fingerprint"] == fixed.as_str()
            && p.plan["rule"].as_str().is_some_and(|r| r.contains("person approves"))
            && p.status == "proposed",
        format!("plan: {}", p.plan),
    );
    let early = eval::adopt(&f.home, &p.id, "Ada Reviewer");
    c.check(
        "gatedBeforeEvaluation",
        early.as_ref().is_err_and(|e| e.to_string().contains("hasn't been evaluated"))
            && eval::active(&f.home).unwrap().fingerprint == fixed,
        format!("{:?}; the active configuration is unchanged", early.err().map(|e| e.to_string())),
    );
    let evaluated = eval::evaluate(&f.engine, &p.id, &ds).await.unwrap();
    c.json("evaluated.json", &serde_json::to_value(&evaluated).unwrap());
    let ev = evaluated.evaluation.clone().unwrap_or_default();
    c.check(
        "evaluated",
        evaluated.status == "evaluated" && ev["verdict"] == "pass",
        format!(
            "baseline {} → candidate {} of {}, regressions {}",
            ev["baseline_score"], ev["candidate_score"], ev["max"], ev["regressions"]
        ),
    );
    let by_agent = eval::adopt(&f.home, &p.id, "agent (claude:sonnet @ cl)");
    c.check(
        "needsAPerson",
        by_agent.as_ref().is_err_and(|e| e.to_string().contains("a person has to approve"))
            && eval::active(&f.home).unwrap().fingerprint == fixed,
        format!("{:?}", by_agent.err().map(|e| e.to_string())),
    );
    let adopted = eval::adopt(&f.home, &p.id, "Ada Reviewer").unwrap();
    c.json("active.json", &serde_json::to_value(&adopted).unwrap());
    c.check(
        "adoptedAfterReview",
        adopted.fingerprint == p.proposed.fingerprint()
            && adopted.fingerprint != fixed
            && adopted.approved_by == "Ada Reviewer"
            && adopted.proposal.as_deref() == Some(p.id.as_str()),
        format!("active: {} ({}), approved by {}", adopted.config.name, adopted.fingerprint, adopted.approved_by),
    );
    let log: Vec<Value> = std::fs::read_to_string(eval::dir(&f.home).join("events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let refusals = log.iter().filter(|e| e["event"] == "adoption_refused").count();
    c.json("events.json", &json!(log));
    c.check("attemptsLogged", refusals == 2, format!("{refusals} refused adoption attempts in the log"));
    c.finish();
}
