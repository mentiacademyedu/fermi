//! System 1: model-agnostic AI engine, with deterministic fake CLIs.

use acceptance_tests::{account, fixture, repo_root, wait_task, Case, Fermi};
use fermi_core::engine::Task;
use fermi_core::events::Event;
use fermi_core::hub::{FollowUp, TaskStatus};
use serde_json::{json, Value};
use std::time::Duration;

const T: Duration = Duration::from_secs(60);

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::User { .. } => "user",
            Event::Routed { .. } => "routed",
            Event::Session { .. } => "session",
            Event::Delta { .. } => "delta",
            Event::Text { .. } => "text",
            Event::Tool { .. } => "tool",
            Event::ToolResult { .. } => "tool_result",
            Event::Call { .. } => "call",
            Event::Remote { .. } => "remote",
            Event::Limit { .. } => "limit",
            Event::Failover { .. } => "failover",
            Event::Retry { .. } => "retry",
            Event::Blocked { .. } => "blocked",
            Event::Artifact { .. } => "artifact",
            Event::Log { .. } => "log",
            Event::Estimate { .. } => "estimate",
            Event::Budget { .. } => "budget",
            Event::Decision { .. } => "decision",
            Event::Verify { .. } => "verify",
            Event::Repair { .. } => "repair",
            Event::Done { .. } => "done",
        })
        .collect()
}

/// Event kinds without the optional ones (see AI-01).
fn lifecycle(kinds: &Value) -> Vec<String> {
    kinds
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|k| k.as_str())
        .filter(|k| !matches!(*k, "delta" | "call"))
        .map(String::from)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ai_01_adapter_normalization() {
    let mut c = Case::start("AI-01", "T1");
    c.real(&[
        "fermi engine",
        "claude, codex and gemini CLI adapters",
        "Anthropic and OpenAI HTTP API adapters",
        "process spawning and stream parsing",
    ]);
    c.mocked(&[
        "Claude Code, Codex and Gemini CLIs (fake-cli with scripted output)",
        "Anthropic and OpenAI APIs (F-PROVIDER mock endpoints)",
    ]);
    // Equivalent scripted output, emitted in each CLI's own stream format.
    let step = json!({ "text": "The answer is 42.", "tools": [{ "name": "Bash", "input": { "command": "ls src" } }], "usage": { "input": 120, "output": 30 } });
    let f = Fermi::new(
        &c,
        &[account("cl", "claude", vec![step.clone()]), account("cx", "openai", vec![step.clone()]), account("gm", "gemini", vec![step])],
        "",
    )
    .unwrap();
    let cwd = c.work.clone();
    let mut normalized = vec![];
    for (acct, model) in [("cl", "claude:sonnet"), ("cx", "openai:gpt"), ("gm", "gemini:flash")] {
        let mut task = f.task("What is the answer?", &cwd);
        task.account = Some(acct.into());
        task.model = Some(model.into());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let out = f.engine.run(&task, &tx).await;
        drop(tx);
        let mut events = vec![];
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
        let tools: Vec<String> =
            events.iter().filter_map(|e| if let Event::Tool { detail, .. } = e { Some(detail.clone()) } else { None }).collect();
        let done_ok = events.iter().any(|e| matches!(e, Event::Done { ok: true, .. }));
        let o = out.as_ref().ok();
        normalized.push(json!({
            "account": acct, "model": model,
            "text": o.map(|o| o.text.clone()), "toolCalls": tools, "finish": if done_ok { "ok" } else { "failed" },
            "usage": o.map(|o| json!({ "input": o.usage.input, "output": o.usage.output })),
            "eventKinds": kinds(&events),
        }));
    }
    c.json("normalized-responses.json", &json!(normalized));
    c.json("request-envelopes.json", &json!({ "cl": f.calls("cl"), "cx": f.calls("cx"), "gm": f.calls("gm") }));
    let canonical =
        json!({ "text": "The answer is 42.", "toolCalls": ["ls src"], "finish": "ok", "usage": { "input": 120, "output": 30 } });
    for n in &normalized {
        let same = ["text", "toolCalls", "finish", "usage"].iter().all(|k| n[k] == canonical[k]);
        c.check(&format!("normalized:{}", n["account"].as_str().unwrap()), same, format!("{n}"));
    }
    // `delta` is optional progressive output (some CLIs stream it, others don't);
    // the finished `text` replaces it. `call` carries each provider's own usage
    // at its own granularity (Claude per model call, Codex per turn, Gemini per
    // run). The lifecycle must match without them.
    let cycles: Vec<Vec<String>> = normalized.iter().map(|n| lifecycle(&n["eventKinds"])).collect();
    c.check(
        "sameLifecycle",
        cycles.iter().all(|l| *l == cycles[0]),
        format!("{cycles:?} (optional delta and per-provider call events excluded)"),
    );
    for (acct, flavor) in [("cl", "claude"), ("cx", "codex"), ("gm", "gemini")] {
        let calls = f.calls(acct);
        c.check(
            &format!("rightAdapter:{flavor}"),
            calls.len() == 1 && calls[0]["flavor"] == flavor,
            format!("{} call(s) to account {acct}, flavor {:?}", calls.len(), calls.first().map(|x| x["flavor"].clone())),
        );
    }

    // The two HTTP API adapters (Anthropic Messages, OpenAI Responses), each
    // against an endpoint answering the same in its own wire format. They
    // send text and get text back: no tool calls to compare.
    use acceptance_tests::provider::{messages_ok, mock_api, responses_ok};
    let anthropic = mock_api(vec![messages_ok("The answer is 42.").usage(120, 30)], None).await;
    let openai = mock_api(vec![responses_ok("The answer is 42.").usage(120, 30)], None).await;
    let home = c.work.join("api-home");
    std::fs::create_dir_all(&home).unwrap();
    let cfg = format!(
        "[[accounts]]
id = \"an\"
provider = \"claude\"
auth = \"api\"
base_url = \"{}\"

         [[accounts]]
id = \"oa\"
provider = \"openai\"
auth = \"api\"
base_url = \"{}\"
",
        anthropic.base, openai.base
    );
    let api = fermi_core::Engine::new(fermi_core::config::Config::parse(&cfg).unwrap(), home, 0);
    let mut via_api = vec![];
    for (acct, model) in [("an", "claude:sonnet"), ("oa", "openai:gpt")] {
        let mut task = Task::new("What is the answer?", &cwd);
        task.account = Some(acct.into());
        task.model = Some(model.into());
        let (out, events) = run_collect(&api, &task).await;
        let o = out.as_ref().ok();
        via_api.push(json!({
            "account": acct, "model": model, "text": o.map(|o| o.text.clone()),
            "finish": if events.iter().any(|e| matches!(e, Event::Done { ok: true, .. })) { "ok" } else { "failed" },
            "usage": o.map(|o| json!({ "input": o.usage.input, "output": o.usage.output })),
            "eventKinds": kinds(&events), "error": out.as_ref().err().map(|e| e.to_string()),
        }));
    }
    c.json("api-normalized-responses.json", &json!(via_api));
    c.json("api-request-envelopes.json", &json!({ "an": anthropic.requests(), "oa": openai.requests() }));
    for n in &via_api {
        let same = ["text", "finish", "usage"].iter().all(|k| n[k] == canonical[k]);
        c.check(&format!("normalized:{}", n["account"].as_str().unwrap()), same, format!("{n}"));
    }
    let api_cycles: Vec<Vec<String>> = via_api.iter().map(|n| lifecycle(&n["eventKinds"])).collect();
    c.check("sameLifecycle:api", api_cycles.len() == 2 && api_cycles[0] == api_cycles[1], format!("{api_cycles:?}"));
    let (an, oa) = (anthropic.requests(), openai.requests());
    c.check(
        "rightAdapter:api",
        an.len() == 1
            && an[0]["path"] == "/v1/messages"
            && an[0]["body"]["model"] == "claude-sonnet-5-5"
            && oa.len() == 1
            && oa[0]["path"] == "/v1/responses"
            && oa[0]["body"]["model"] == "gpt-6-astra",
        format!(
            "anthropic {:?}, openai {:?}",
            an.first().map(|r| (&r["path"], &r["body"]["model"])),
            oa.first().map(|r| (&r["path"], &r["body"]["model"]))
        ),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ai_02_common_task_interface() {
    let mut c = Case::start("AI-02", "T1");
    c.real(&["fermi hub (public task service)", "engine", "git", "cargo test on the fixture"]);
    c.mocked(&["model output (fake-cli applies the scripted edit)"]);
    let expected = std::fs::read_to_string(repo_root().join("tests/fixtures/F-CODE/expected/engine-lib-greet.rs")).unwrap();
    let step = json!({ "text": "Added greet.", "tools": [{ "name": "Edit", "input": { "file_path": "engine/src/lib.rs" } }], "edits": [{ "path": "engine/src/lib.rs", "content": expected }] });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step.clone()]), account("cx", "openai", vec![step])], "").unwrap();
    let hub = f.hub();
    let mut runs = vec![];
    for (acct, model) in [("cl", "claude:sonnet"), ("cx", "openai:gpt")] {
        let fx = fixture("F-CODE", &c.work.join(format!("repo-{acct}"))).unwrap();
        let mut task = f.task("Add a greet(name) function to the engine crate, with a test.", &fx.path);
        task.account = Some(acct.into());
        task.model = Some(model.into());
        let id = hub.submit(task);
        let rec = wait_task(&hub, id, T).await.unwrap();
        let diff = acceptance_tests::git(&fx.path, &["diff", "--stat"]).unwrap();
        let test = std::process::Command::new("cargo")
            .args(["test", "--quiet", "--offline"])
            .current_dir(&fx.path)
            .env("CARGO_TARGET_DIR", c.work.join("fixture-target"))
            .output()
            .unwrap();
        let test_out = format!("{}{}", String::from_utf8_lossy(&test.stdout), String::from_utf8_lossy(&test.stderr));
        c.text(&format!("{acct}/diff.txt"), &acceptance_tests::git(&fx.path, &["diff"]).unwrap());
        c.text(&format!("{acct}/cargo-test.txt"), &test_out);
        let content_ok = std::fs::read_to_string(fx.path.join("engine/src/lib.rs")).unwrap() == expected;
        runs.push(json!({ "model": model, "status": format!("{:?}", rec.status), "eventKinds": kinds(&rec.events), "diffStat": diff, "testsPass": test.status.success(), "matchesOracle": content_ok }));
    }
    c.json("runs.json", &json!(runs));
    for r in &runs {
        let m = r["model"].as_str().unwrap();
        c.check(
            &format!("oracle:{m}"),
            r["matchesOracle"] == true && r["testsPass"] == true,
            format!("edit matches expected, fixture tests pass: {}", r["testsPass"]),
        );
        c.check(&format!("completed:{m}"), r["status"] == "Done", r["status"].to_string());
    }
    let (a, b) = (lifecycle(&runs[0]["eventKinds"]), lifecycle(&runs[1]["eventKinds"]));
    c.check("sameLifecycle", a == b, format!("{a:?} / {b:?} (per-provider call events excluded)"));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ai_03_model_switching_with_state() {
    let mut c = Case::start("AI-03", "T1");
    c.real(&["fermi hub follow-ups", "engine", "history"]);
    c.mocked(&["Claude Code and Codex CLIs (fake-cli)"]);
    let f = Fermi::new(
        &c,
        &[
            account(
                "cl",
                "claude",
                vec![
                    json!({ "text": "Noted: the code word is AURORA." }),
                    json!({ "text": "Still AURORA." }),
                    json!({ "text": "AURORA, from Sonnet." }),
                ],
            ),
            account("cx", "openai", vec![json!({ "text": "AURORA, from GPT." })]),
        ],
        "",
    )
    .unwrap();
    let hub = f.hub();
    let mut task = f.task("Remember the code word AURORA.", &c.work);
    task.model = Some("claude:haiku".into());
    let id = hub.submit(task);
    wait_task(&hub, id, T).await.unwrap();
    hub.follow_up(id, "What is the code word?", FollowUp::default()).unwrap();
    wait_task(&hub, id, T).await.unwrap();
    // Switch within the provider: the same session continues on another model.
    hub.follow_up(id, "And now?", FollowUp { model: Some("claude:sonnet".into()), ..Default::default() }).unwrap();
    wait_task(&hub, id, T).await.unwrap();
    // Switch provider: a new session that receives the conversation so far.
    hub.follow_up(id, "One more time?", FollowUp { model: Some("openai:gpt".into()), ..Default::default() }).unwrap();
    let rec = wait_task(&hub, id, T).await.unwrap();
    let cl = f.calls("cl");
    let cx = f.calls("cx");
    c.json("request-routing.json", &json!({ "claude": cl, "codex": cx }));
    c.json("transcript.json", &serde_json::to_value(&rec.events).unwrap());
    let arg = |call: &Value, flag: &str| -> Option<String> {
        let a: Vec<&str> = call["args"].as_array()?.iter().filter_map(|x| x.as_str()).collect();
        a.iter().position(|x| *x == flag).and_then(|i| a.get(i + 1)).map(|s| s.to_string())
    };
    let session = cl.first().and_then(|x| arg(x, "--session-id"));
    c.check("turn1Haiku", cl.first().and_then(|x| arg(x, "--model")).as_deref() == Some("haiku"), "first call on haiku");
    c.check(
        "turn2SameModel",
        cl.get(1).and_then(|x| arg(x, "--model")).as_deref() == Some("haiku") && cl.get(1).and_then(|x| arg(x, "--resume")) == session,
        "second call resumes the session on haiku",
    );
    c.check(
        "switchOnlyNextCall",
        cl.get(2).and_then(|x| arg(x, "--model")).as_deref() == Some("sonnet") && cl.get(2).and_then(|x| arg(x, "--resume")) == session,
        "third call: sonnet, same session",
    );
    let prompt = cx.first().and_then(|x| x["prompt"].as_str()).unwrap_or("");
    c.check(
        "crossProviderGetsState",
        prompt.contains("Remember the code word AURORA.") && prompt.contains("Noted: the code word is AURORA."),
        "GPT received the earlier turns",
    );
    let users: Vec<String> =
        rec.events.iter().filter_map(|e| if let Event::User { text } = e { Some(text.clone()) } else { None }).collect();
    c.check(
        "turnsOrderedAndComplete",
        users == ["Remember the code word AURORA.", "What is the code word?", "And now?", "One more time?"],
        format!("{users:?}"),
    );
    let routed: Vec<String> =
        rec.events.iter().filter_map(|e| if let Event::Routed { model, .. } = e { Some(model.clone()) } else { None }).collect();
    c.check("turnsAttributed", routed == ["claude:haiku", "claude:haiku", "claude:sonnet", "openai:gpt"], format!("{routed:?}"));
    c.check("finished", rec.status == TaskStatus::Done, format!("{:?}", rec.status));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ai_05_provider_outage_handling() {
    let mut c = Case::start("AI-05", "T1");
    c.real(&["fermi engine failover", "claude CLI adapter"]);
    c.mocked(&["provider outage (fake-cli reports a 503)"]);
    let outage = json!({ "error": "API Error: 503 Service Unavailable" });
    let ok = json!({ "text": "Done on the fallback." });
    let f = Fermi::new(
        &c,
        &[
            account("a", "claude", vec![outage.clone()]),
            account("b", "claude", vec![ok.clone()]),
            account("z", "claude", vec![ok]).outside(),
        ],
        "",
    )
    .unwrap();
    // Fallback disabled: the task is pinned to one account.
    let mut pinned = f.task("Do the thing.", &c.work);
    pinned.account = Some("a".into());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let r1 = f.engine.run(&pinned, &tx).await;
    drop(tx);
    let mut ev1 = vec![];
    while let Some(e) = rx.recv().await {
        ev1.push(e);
    }
    let calls_after_pinned = (f.calls("a").len(), f.calls("b").len());
    c.check(
        "disabledFallbackFailsClearly",
        r1.as_ref().is_err_and(|e| e.to_string().contains("503")),
        format!("{:?}", r1.as_ref().err().map(|e| e.to_string())),
    );
    c.check("noOtherAccountWhenPinned", calls_after_pinned == (1, 0), format!("calls a/b: {calls_after_pinned:?}"));
    // Fallback enabled: the profile's other account takes over.
    let task = f.task("Do the thing.", &c.work);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let r2 = f.engine.run(&task, &tx).await;
    drop(tx);
    let mut ev2 = vec![];
    while let Some(e) = rx.recv().await {
        ev2.push(e);
    }
    c.json("pinned-events.json", &serde_json::to_value(&ev1).unwrap());
    c.json("fallback-events.json", &serde_json::to_value(&ev2).unwrap());
    c.json("calls.json", &json!({ "a": f.calls("a"), "b": f.calls("b"), "z": f.calls("z") }));
    c.check(
        "fallbackUsed",
        r2.as_ref().is_ok_and(|o| o.account == "b"),
        format!("{:?}", r2.as_ref().map(|o| o.account.clone()).map_err(|e| e.to_string())),
    );
    c.check(
        "switchRecorded",
        ev2.iter().any(|e| matches!(e, Event::Failover { account, .. } if account == "a")),
        "Failover event names the failed account",
    );
    c.check("onlyPermittedProvider", f.calls("z").is_empty(), "the account outside the profile was never called");
    c.finish();
}

/// Runs a task on an engine, returning the result and every event.
async fn run_collect(
    engine: &fermi_core::Engine,
    task: &fermi_core::engine::Task,
) -> (anyhow::Result<fermi_core::engine::Outcome>, Vec<Event>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let r = engine.run(task, &tx).await;
    drop(tx);
    let mut ev = vec![];
    while let Some(e) = rx.recv().await {
        ev.push(e);
    }
    (r, ev)
}

/// An engine whose one account calls the HTTP API at `base` (no key: a mock),
/// with a three-attempt retry ceiling and a manual clock.
fn api_engine(c: &Case, name: &str, base: &str, clock: &fermi_core::clock::Clock) -> fermi_core::Engine {
    let home = c.work.join(name);
    std::fs::create_dir_all(&home).unwrap();
    let cfg = format!(
        "default_profile = \"p\"\n\n[retry]\nattempts = 3\nbackoff_secs = 1\nmax_wait_secs = 60\n\n\
         [[accounts]]\nid = \"api\"\nprovider = \"claude\"\nauth = \"api\"\nbase_url = \"{base}\"\n\n\
         [[profiles]]\nname = \"p\"\naccounts = [\"api\"]\nmode = \"single\"\nmodels = [\"claude:sonnet\"]\n"
    );
    std::fs::write(home.join("config.toml"), &cfg).unwrap();
    let mut e = fermi_core::Engine::new(fermi_core::config::Config::parse(&cfg).unwrap(), home, 0);
    e.clock = clock.clone();
    e
}

fn retries(ev: &[Event]) -> Vec<(u32, u64)> {
    ev.iter()
        .filter_map(|e| match e {
            Event::Retry { attempt, wait_secs, .. } => Some((*attempt, *wait_secs)),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ai_04_rate_limit_handling() {
    use acceptance_tests::provider::{messages_ok, mock_api, rate_limited};
    use fermi_core::clock::Clock;
    let mut c = Case::start("AI-04", "T1");
    c.real(&["fermi engine retry policy", "Anthropic Messages adapter (HTTP)", "HTTP client"]);
    c.mocked(&["provider API (F-PROVIDER mock endpoint answering scripted 429s)", "time (a manual clock: waits advance it)"]);

    // Recoverable: a 429 without Retry-After (back off 1s), one asking for 2s, then an answer.
    let clock = Clock::manual();
    let api = mock_api(vec![rate_limited(None), rate_limited(Some(2)), messages_ok("Recovered.")], Some(clock.clone())).await;
    let e = api_engine(&c, "recoverable", &api.base, &clock);
    let (r, ev) = run_collect(&e, &Task::new("Say something.", &c.work)).await;
    let reqs = api.requests();
    let at: Vec<u64> = reqs.iter().map(|q| q["at"].as_u64().unwrap()).collect();
    c.json("recoverable-attempts.json", &json!({ "requests": reqs, "sleeps": clock.sleeps(), "events": ev }));
    c.check(
        "recoversWithinCeiling",
        r.as_ref().is_ok_and(|o| o.text == "Recovered.") && reqs.len() == 3,
        format!("{} requests; result {:?}", reqs.len(), r.as_ref().map(|o| o.text.clone()).map_err(|e| e.to_string())),
    );
    c.check(
        "attemptTimesObeyPolicy",
        at == [0, 1, 3] && clock.sleeps() == [1, 2] && retries(&ev) == [(2, 1), (3, 2)],
        format!("attempts at t={at:?}s (backoff 1s, then Retry-After 2s); waits {:?}; retry events {:?}", clock.sleeps(), retries(&ev)),
    );

    // Exhausted: every call is limited; a fourth answer is ready but must never be asked for.
    let clock = Clock::manual();
    let api = mock_api(
        vec![rate_limited(Some(30)), rate_limited(Some(30)), rate_limited(Some(30)), messages_ok("Too late.")],
        Some(clock.clone()),
    )
    .await;
    let e = api_engine(&c, "exhausted", &api.base, &clock);
    let (r, ev) = run_collect(&e, &Task::new("Say something.", &c.work)).await;
    let after_first = api.requests().len();
    // The next task doesn't start another retry loop: the account is marked limited (for the 30s asked).
    let (r2, ev2) = run_collect(&e, &Task::new("Say something else.", &c.work)).await;
    let reqs = api.requests();
    c.json("exhausted-attempts.json", &json!({ "requests": reqs, "sleeps": clock.sleeps(), "events": ev, "next_task_events": ev2 }));
    let err = r.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
    c.check(
        "attemptsNeverExceedCeiling",
        after_first == 3 && api.left() == 1 && clock.sleeps() == [30, 30] && retries(&ev) == [(2, 30), (3, 30)],
        format!("{after_first} requests, {} scripted answer(s) unused, waits {:?}", api.left(), clock.sleeps()),
    );
    let done_fail = ev.iter().filter(|e| matches!(e, Event::Done { ok: false, .. })).count() == 1;
    c.check("terminalFailure", r.is_err() && err.contains("3 attempts") && done_fail, format!("error: {err}"));
    c.check(
        "noRetryLoop",
        r2.is_err() && reqs.len() == 3 && retries(&ev2).is_empty(),
        format!("next task: {:?}; requests still {}", r2.err().map(|e| e.to_string()), reqs.len()),
    );

    // A Retry-After past max_wait_secs isn't waited out.
    let clock = Clock::manual();
    let api = mock_api(vec![rate_limited(Some(3600)), messages_ok("Never.")], Some(clock.clone())).await;
    let e = api_engine(&c, "long-wait", &api.base, &clock);
    let (r, ev) = run_collect(&e, &Task::new("Say something.", &c.work)).await;
    c.json("long-wait.json", &json!({ "requests": api.requests(), "sleeps": clock.sleeps(), "events": ev }));
    c.check(
        "longWaitNotTaken",
        api.requests().len() == 1 && clock.sleeps().is_empty() && r.as_ref().is_err_and(|e| e.to_string().contains("max_wait_secs")),
        format!("{:?}", r.err().map(|e| e.to_string())),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ai_06_capability_validation() {
    let mut c = Case::start("AI-06", "T1");
    c.real(&["fermi config validation", "fermi engine routing", "claude CLI adapter"]);
    c.mocked(&["Claude Code CLI (fake-cli)"]);
    // T0: a profile that needs tools but only has a model that can't call them is rejected.
    let model = "[[models]]\nid = \"claude:haiku\"\ncaps = [\"code\", \"general\", \"fast\"]\ntier = 1\ncli = \"haiku\"\ntools = false\n";
    let profile = |models: &str| {
        format!("[[accounts]]\nid = \"cl\"\nprovider = \"claude\"\n\n[[profiles]]\nname = \"p\"\naccounts = [\"cl\"]\nmode = \"pinned\"\nmodels = [{models}]\nrequire_tools = true\n\n{model}")
    };
    let bad = fermi_core::config::Config::parse(&profile("\"claude:haiku\""));
    let good = fermi_core::config::Config::parse(&profile("\"claude:haiku\", \"claude:sonnet\""));
    c.json(
        "config-validation.json",
        &json!({ "only_haiku": bad.as_ref().err().map(|e| e.to_string()), "haiku_and_sonnet_ok": good.is_ok() }),
    );
    c.check(
        "configurationRejected",
        bad.as_ref().is_err_and(|e| e.to_string().contains("tool calling")) && good.is_ok(),
        format!("{:?}", bad.err().map(|e| e.to_string())),
    );

    let step = json!({ "text": "Fixed.", "tools": [{ "name": "Edit", "input": { "file_path": "README.md" } }] });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step.clone(), step.clone(), step])], &format!("\n{model}")).unwrap();
    let prompt = "Fix the typo in README.md";
    // Control: without the need for tools, auto routing picks the light model.
    let (r0, ev0) = run_collect(&f.engine, &f.task(prompt, &c.work)).await;
    let routed = |ev: &[Event]| {
        ev.iter().find_map(|e| match e {
            Event::Routed { model, .. } => Some(model.clone()),
            _ => None,
        })
    };
    c.check(
        "controlPicksModelWithoutTools",
        r0.is_ok() && routed(&ev0).as_deref() == Some("claude:haiku"),
        format!("without tools: {:?}", routed(&ev0)),
    );
    // Forced onto the model that can't call tools: rejected before dispatch.
    let calls_before = f.calls("cl").len();
    let mut forced = f.task(prompt, &c.work);
    forced.tools = true;
    forced.model = Some("claude:haiku".into());
    let (r1, ev1) = run_collect(&f.engine, &forced).await;
    let calls_after = f.calls("cl").len();
    c.json("rejected-events.json", &serde_json::to_value(&ev1).unwrap());
    c.check(
        "rejectedBeforeDispatch",
        r1.as_ref().is_err_and(|e| e.to_string().contains("can't call tools")) && calls_after == calls_before,
        format!("{:?}; CLI calls {calls_before} -> {calls_after}", r1.as_ref().err().map(|e| e.to_string())),
    );
    c.check(
        "noFabricatedToolResult",
        !ev1.iter().any(|e| matches!(e, Event::Tool { .. } | Event::ToolResult { .. } | Event::Text { .. } | Event::Routed { .. })),
        format!("events of the rejected task: {:?}", kinds(&ev1)),
    );
    // Routed automatically: the documented alternative, a model that can call tools, and the skip is shown.
    let mut auto = f.task(prompt, &c.work);
    auto.tools = true;
    let (r2, ev2) = run_collect(&f.engine, &auto).await;
    c.json("alternative-events.json", &serde_json::to_value(&ev2).unwrap());
    c.json("calls.json", &json!(f.calls("cl")));
    let chosen = routed(&ev2).unwrap_or_default();
    let spec = f.engine.catalog.iter().find(|m| m.id == chosen);
    let last_args = f.calls("cl").last().map(|c| c["args"].to_string()).unwrap_or_default();
    c.check(
        "alternativeUsedAndObservable",
        r2.is_ok()
            && spec.is_some_and(|m| m.tool_calling())
            && chosen != "claude:haiku"
            && !last_args.contains("\"haiku\"")
            && ev2.iter().any(|e| matches!(e, Event::Log { line } if line.contains("skipped claude:haiku"))),
        format!("routed to {chosen}; CLI args {last_args}"),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ai_08_local_endpoint_support() {
    use acceptance_tests::provider::{chat_ok, egress_recorder, mock_api};
    let mut c = Case::start("AI-08", "T1");
    c.real(&["fermi engine in its own process", "OpenAI-compatible chat completions adapter (HTTP)", "config loading"]);
    c.mocked(&["local model server (F-PROVIDER mock endpoint speaking chat completions)"]);
    let local = mock_api(vec![chat_ok("Hello from the local model.")], None).await;
    let egress = egress_recorder().await;
    let home = c.work.join("fermi-home");
    std::fs::create_dir_all(&home).unwrap();
    // The only account is the local endpoint: no hosted account, no key.
    let cfg = format!(
        "default_profile = \"local\"\n\n[[accounts]]\nid = \"local\"\nprovider = \"openai\"\nauth = \"api\"\nbase_url = \"{}/v1\"\napi_format = \"chat\"\n\n\
         [[models]]\nid = \"openai:qwen-local\"\nlabel = \"Qwen 2.5 Coder (local)\"\ncaps = [\"code\", \"general\"]\napi = \"qwen2.5-coder\"\ntools = false\n\n\
         [[profiles]]\nname = \"local\"\naccounts = [\"local\"]\nmode = \"single\"\nmodels = [\"openai:qwen-local\"]\n",
        local.base
    );
    std::fs::write(home.join("config.toml"), &cfg).unwrap();
    c.text("config.toml", &cfg);
    let mut cmd = std::process::Command::new(acceptance_tests::fermi_run());
    cmd.args(["--profile", "local", "--cwd"]).arg(&c.work).arg("Say hello.").env("FERMI_HOME", &home);
    // No hosted credentials, and anything bound outside goes through the recorder.
    for k in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY", "NO_PROXY", "no_proxy"] {
        cmd.env_remove(k);
    }
    for k in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
        cmd.env(k, &egress.url);
    }
    cmd.env("NO_PROXY", "127.0.0.1,localhost");
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap()).await.unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    c.text("fermi-run.jsonl", &stdout);
    c.text("fermi-run.stderr.txt", &String::from_utf8_lossy(&out.stderr));
    let lines: Vec<Value> = stdout.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let outcome = lines.iter().find_map(|l| l.get("outcome")).cloned().unwrap_or(Value::Null);
    let reqs = local.requests();
    let outside = egress.log.lock().unwrap().clone();
    c.json("endpoint-log.json", &json!(reqs));
    c.json("outbound-records.json", &json!({ "through_proxy": outside }));
    c.check(
        "normalizedResult",
        out.status.success()
            && outcome["text"] == "Hello from the local model."
            && outcome["account"] == "local"
            && outcome["model"] == "openai:qwen-local"
            && outcome["input"] == 21
            && outcome["output"] == 9
            && lines.iter().any(|l| l["type"] == "text")
            && lines.iter().any(|l| l["type"] == "done" && l["ok"] == true),
        format!("outcome {outcome}"),
    );
    c.check(
        "endpointCalled",
        reqs.len() == 1
            && reqs[0]["path"] == "/v1/chat/completions"
            && reqs[0]["body"]["model"] == "qwen2.5-coder"
            && reqs[0]["body"]["messages"][0]["content"].as_str().is_some_and(|s| s.contains("Say hello.")),
        format!("{} request(s): {:?}", reqs.len(), reqs.first().map(|r| (&r["method"], &r["path"]))),
    );
    c.check(
        "noHostedAccountOrRequest",
        reqs[0]["credentials"] == json!([]) && outside.is_empty() && !cfg.contains("api_key_env"),
        format!("credentials sent: {}; requests through the outbound recorder: {outside:?}", reqs[0]["credentials"]),
    );
    c.finish();
}
