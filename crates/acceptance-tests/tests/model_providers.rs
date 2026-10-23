//! System 1: model-agnostic AI engine, with deterministic fake CLIs.

use acceptance_tests::{account, fixture, repo_root, wait_task, Case, Fermi};
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
            Event::Limit { .. } => "limit",
            Event::Failover { .. } => "failover",
            Event::Blocked { .. } => "blocked",
            Event::Artifact { .. } => "artifact",
            Event::Log { .. } => "log",
            Event::Done { .. } => "done",
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ai_01_adapter_normalization() {
    let mut c = Case::start("AI-01", "T1");
    c.real(&["fermi engine", "claude, codex and gemini CLI adapters", "process spawning and stream parsing"]);
    c.mocked(&["Claude Code, Codex and Gemini CLIs (fake-cli with scripted output)"]);
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
    // the finished `text` replaces it. The lifecycle must match without it.
    let lifecycle = |n: &Value| -> Vec<String> {
        n["eventKinds"].as_array().unwrap().iter().filter_map(|k| k.as_str()).filter(|k| *k != "delta").map(String::from).collect()
    };
    let cycles: Vec<Vec<String>> = normalized.iter().map(lifecycle).collect();
    c.check("sameLifecycle", cycles.iter().all(|l| *l == cycles[0]), format!("{cycles:?} (optional delta events excluded)"));
    for (acct, flavor) in [("cl", "claude"), ("cx", "codex"), ("gm", "gemini")] {
        let calls = f.calls(acct);
        c.check(
            &format!("rightAdapter:{flavor}"),
            calls.len() == 1 && calls[0]["flavor"] == flavor,
            format!("{} call(s) to account {acct}, flavor {:?}", calls.len(), calls.first().map(|x| x["flavor"].clone())),
        );
    }
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
    c.check(
        "sameLifecycle",
        runs[0]["eventKinds"] == runs[1]["eventKinds"],
        format!("{} / {}", runs[0]["eventKinds"], runs[1]["eventKinds"]),
    );
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
