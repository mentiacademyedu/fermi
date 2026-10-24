//! System 8: agent observability.

use acceptance_tests::{account, sha256, wait_saved, wait_task, Case, Fermi};
use fermi_core::events::Event;
use fermi_core::hub::{Hub, TaskStatus};
use fermi_core::trace;
use serde_json::{json, Value};
use std::time::Duration;

const T: Duration = Duration::from_secs(60);

/// The saved history file of a task.
fn history(f: &Fermi, id: u64) -> Value {
    serde_json::from_slice(&std::fs::read(f.home.join("history").join(format!("{id}.json"))).unwrap_or_default()).unwrap_or_default()
}

/// Operations of one kind from a trace export.
fn ops<'a>(export: &'a Value, kind: &str) -> Vec<&'a Value> {
    export["operations"].as_array().into_iter().flatten().filter(|o| o["kind"] == kind).collect()
}

fn ids(list: &[&Value]) -> Vec<String> {
    list.iter().map(|o| o["id"].as_str().unwrap_or_default().to_string()).collect()
}

fn usage_records(f: &Fermi) -> Vec<Value> {
    std::fs::read_to_string(f.home.join("usage.jsonl")).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Every file under `dir`, as text.
fn files_text(dir: &std::path::Path) -> Vec<(String, String)> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().filter_map(|e| e.ok()) {
        let p = e.path();
        if p.is_dir() {
            out.extend(files_text(&p));
        } else {
            out.push((p.display().to_string(), String::from_utf8_lossy(&std::fs::read(&p).unwrap_or_default()).into_owned()));
        }
    }
    out
}

/// Seq numbers run 1, 2, 3… without gaps or repeats.
fn gapless(rec: &Value) -> bool {
    let seqs: Vec<u64> = rec["stamps"].as_array().into_iter().flatten().filter_map(|s| s["seq"].as_u64()).collect();
    !seqs.is_empty()
        && seqs.iter().enumerate().all(|(i, s)| *s == i as u64 + 1)
        && seqs.len() == rec["events"].as_array().map_or(0, |e| e.len())
}

/// The five-operation scenario: two model calls, three tools, known ids.
fn five_ops() -> Value {
    json!({ "calls": [
        { "id": "msg_A", "usage": { "input_tokens": 100, "output_tokens": 20 },
          "tools": [
            { "id": "toolu_1", "name": "Read", "input": { "file_path": "src/lib.rs" }, "output": "pub fn add(a: i32, b: i32) -> i32 { a + b }" },
            { "id": "toolu_2", "name": "Grep", "input": { "pattern": "add" }, "output": "src/lib.rs:1" } ] },
        { "id": "msg_B", "usage": { "input_tokens": 150, "output_tokens": 30 }, "text": "All tests pass.",
          "tools": [ { "id": "toolu_3", "name": "Bash", "input": { "command": "cargo test" }, "output": "test result: ok. 3 passed" } ] }
    ] })
}

#[tokio::test(flavor = "multi_thread")]
async fn ob_01_complete_operation_trace() {
    let mut c = Case::start("OB-01", "T1");
    c.real(&["fermi engine and hub", "Claude CLI adapter (stream parsing)", "trace export", "failover between accounts"]);
    c.mocked(&["model output (fake-cli speaking Claude Code's stream-json)"]);
    // The first account fails mid-run after one tool (a retry on the second account follows).
    let failing = json!({ "calls": [{ "id": "msg_X", "tools": [{ "id": "toolu_x", "name": "Read", "input": { "file_path": "a.txt" } }] }],
        "error": "API Error: 503 Service Unavailable" });
    let f = Fermi::new(&c, &[account("c1", "claude", vec![failing]), account("c2", "claude", vec![five_ops()])], "").unwrap();
    let hub = f.hub();
    let id = hub.submit(f.task("Check that add works.", &c.work));
    let rec = wait_task(&hub, id, T).await.unwrap();
    wait_saved(&f.home, id, T).await;
    let saved = history(&f, id);
    let export = trace::export(&fermi_core::hub::read_task(&f.home.join("history"), id).unwrap());
    c.json("trace-export.json", &export);
    c.json("history.json", &saved);
    c.check("taskSucceeded", rec.status == TaskStatus::Done, format!("{:?}", rec.status));

    // The retry: operations of the successful try (try 2) are exactly the five.
    let second: Vec<&Value> = export["operations"].as_array().unwrap().iter().filter(|o| o["try"] == 2).collect();
    let calls: Vec<&Value> = second.iter().copied().filter(|o| o["kind"] == "model_call").collect();
    let tools: Vec<&Value> = second.iter().copied().filter(|o| o["kind"] == "tool").collect();
    c.check("twoModelCalls", ids(&calls) == ["msg_A", "msg_B"], format!("model calls {:?}", ids(&calls)));
    c.check("threeTools", ids(&tools) == ["toolu_1", "toolu_2", "toolu_3"], format!("tools {:?}", ids(&tools)));
    c.check("exactlyFive", second.len() == 5, format!("{} operations in the successful try", second.len()));
    // Relationships: each tool's parent call, results after starts, call B after call A's tools.
    let parent = |t: &str| tools.iter().find(|o| o["id"] == t).map(|o| o["call"].clone()).unwrap_or_default();
    c.check(
        "parents",
        parent("toolu_1") == "msg_A" && parent("toolu_2") == "msg_A" && parent("toolu_3") == "msg_B",
        format!("toolu_1→{} toolu_2→{} toolu_3→{}", parent("toolu_1"), parent("toolu_2"), parent("toolu_3")),
    );
    let seq = |o: &Value, k: &str| o[k].as_u64().unwrap_or(0);
    let ordered = tools.iter().all(|t| t["status"] == "ok" && seq(t, "result_seq") > seq(t, "seq"))
        && seq(calls[0], "seq") < seq(tools[2], "seq")
        && seq(tools[0], "seq") < seq(tools[1], "seq")
        && seq(tools[1], "seq") < seq(tools[2], "seq");
    c.check("ordered", ordered, "each tool has one ok result after it starts; call B's tool starts after call A ended");
    c.check("gapless", gapless(&saved), format!("{} events, seq 1…n", saved["events"].as_array().map_or(0, |e| e.len())));
    // The failed try is identified separately, not mixed into the five.
    let first: Vec<String> = export["operations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["try"] == 1)
        .map(|o| o["id"].as_str().unwrap_or("").to_string())
        .collect();
    let failovers = saved["events"].as_array().unwrap().iter().filter(|e| e["type"] == "failover").count();
    c.check("retrySeparate", first == ["toolu_x", "msg_X"] && failovers == 1, format!("try 1: {first:?}; {failovers} failover event"));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ob_02_invocation_detail() {
    let mut c = Case::start("OB-02", "T1");
    c.real(&["fermi engine and hub", "Claude CLI adapter", "redaction", "monotonic timing"]);
    c.mocked(&["model output and the tool's result (fake-cli)"]);
    let key = "sk-ant-api03-OB02CANARYkeyvalue0123456789";
    let aws = "AKIAOB02CANARY000000";
    let step = json!({ "calls": [{ "id": "msg_1", "text": "Status checked.", "tools": [{
        "id": "toolu_status", "name": "Bash", "ms": 400,
        "input": { "command": format!("curl -s -H 'x-api-key: {key}' https://status.example.invalid/v1/health"), "timeout": 30000 },
        "output": format!("HTTP/1.1 200 OK\nstatus: green\nregion: eu-west-1\naws_access_key={aws}")
    }] }] });
    let f = Fermi::new(&c, &[account("c1", "claude", vec![step])], "").unwrap();
    let hub = f.hub();
    let id = hub.submit(f.task("Check the service status.", &c.work));
    wait_task(&hub, id, T).await.unwrap();
    wait_saved(&f.home, id, T).await;
    let rec = fermi_core::hub::read_task(&f.home.join("history"), id).unwrap();
    let export = trace::export(&rec);
    let op = ops(&export, "tool").into_iter().find(|o| o["id"] == "toolu_status").cloned().unwrap_or_default();
    c.json("invocation.json", &op);
    let cmd = op["input"]["command"].as_str().unwrap_or_default();
    c.check(
        "inputs",
        cmd.contains("curl -s") && cmd.contains("https://status.example.invalid/v1/health") && op["input"]["timeout"] == 30000,
        format!("input {}", op["input"]),
    );
    let out = op["output"].as_str().unwrap_or_default();
    c.check("output", out.contains("status: green") && out.contains("region: eu-west-1"), format!("output {out:?}"));
    c.check("status", op["status"] == "ok" && op["code"].is_null(), format!("status {} code {}", op["status"], op["code"]));
    let ms = op["ms"].as_u64().unwrap_or(0);
    c.check("duration", (300..5000).contains(&ms), format!("{ms} ms measured for a tool scripted to take 400 ms"));
    let saved = std::fs::read_to_string(f.home.join("history").join(format!("{id}.json"))).unwrap_or_default();
    let leaked: Vec<&str> = [key, aws].into_iter().filter(|s| saved.contains(s) || export.to_string().contains(s)).collect();
    c.check(
        "sensitiveExcluded",
        leaked.is_empty() && cmd.contains("[REDACTED]") && out.contains("[REDACTED]"),
        format!("leaked: {leaked:?}"),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ob_03_usage_fidelity() {
    let mut c = Case::start("OB-03", "T0 T1");
    c.real(&["fermi engine", "Claude and Gemini CLI adapters", "usage log"]);
    c.mocked(&["provider usage metadata (fake-cli)"]);
    let a = json!({ "input_tokens": 1200, "cache_read_input_tokens": 800, "cache_creation_input_tokens": 300, "output_tokens": 90 });
    // Call B doesn't report cache writes: that stays unknown, not zero.
    let b = json!({ "input_tokens": 400, "cache_read_input_tokens": 1500, "output_tokens": 60 });
    let step = json!({ "calls": [
        { "id": "msg_A", "usage": a, "tools": [{ "id": "t1", "name": "Read", "input": { "file_path": "x" } }, { "id": "t2", "name": "Read", "input": { "file_path": "y" } }] },
        { "id": "msg_B", "usage": b, "text": "done" } ] });
    let f = Fermi::new(&c, &[account("c1", "claude", vec![step])], "").unwrap();
    let hub = f.hub();
    let id = hub.submit(f.task("Read two files.", &c.work));
    wait_task(&hub, id, T).await.unwrap();
    wait_saved(&f.home, id, T).await;
    let rec = fermi_core::hub::read_task(&f.home.join("history"), id).unwrap();
    let calls: Vec<Value> = rec
        .events
        .iter()
        .filter_map(|e| if let Event::Call { id, usage, .. } = e { Some(json!({ "id": id, "usage": usage })) } else { None })
        .collect();
    c.json("calls.json", &json!(calls));
    c.check(
        "perCallFields",
        calls.len() == 2 && calls[0]["usage"] == a && calls[1]["usage"] == b,
        format!("{} calls; {calls:?}", calls.len()),
    );
    c.check(
        "unknownStaysUnknown",
        calls.get(1).is_some_and(|x| x["usage"].get("cache_creation_input_tokens").is_none()),
        "call B has no cache_creation_input_tokens field",
    );
    let recs = usage_records(&f);
    let r = recs.last().cloned().unwrap_or_default();
    c.json("usage-record.json", &r);
    // Sum over the calls (each counted once, though call A arrived as two lines).
    let sum = |k: &str| a[k].as_u64().unwrap_or(0) + b[k].as_u64().unwrap_or(0);
    let all_input = sum("input_tokens") + sum("cache_read_input_tokens") + sum("cache_creation_input_tokens");
    c.check(
        "aggregateMatches",
        recs.len() == 1
            && r["input_tokens"] == all_input
            && r["cached_input_tokens"] == sum("cache_read_input_tokens")
            && r["cache_write_tokens"] == sum("cache_creation_input_tokens")
            && r["output_tokens"] == sum("output_tokens"),
        format!(
            "record {} in / {} cached / {} written / {} out; calls sum to {all_input} / {} / {} / {}",
            r["input_tokens"],
            r["cached_input_tokens"],
            r["cache_write_tokens"],
            r["output_tokens"],
            sum("cache_read_input_tokens"),
            sum("cache_creation_input_tokens"),
            sum("output_tokens")
        ),
    );

    // A provider that doesn't report cache counts: the record says null, not 0.
    let g =
        Fermi::at(&c, "gemini-home", &[account("g1", "gemini", vec![json!({ "text": "hi", "usage": { "input": 70, "output": 7 } })])], "")
            .unwrap();
    let hub = g.hub();
    let gid = hub.submit(g.task("Say hi.", &c.work));
    wait_task(&hub, gid, T).await.unwrap();
    let gr = usage_records(&g).last().cloned().unwrap_or_default();
    c.json("gemini-usage-record.json", &gr);
    c.check(
        "nullNotZero",
        gr["input_tokens"] == 70 && gr["output_tokens"] == 7 && gr["cache_write_tokens"].is_null() && gr["cached_input_tokens"].is_null(),
        format!(
            "gemini record: in {} out {} cached {} written {}",
            gr["input_tokens"], gr["output_tokens"], gr["cached_input_tokens"], gr["cache_write_tokens"]
        ),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ob_04_failure_diagnostics() {
    let mut c = Case::start("OB-04", "T1");
    c.real(&["fermi engine and hub", "Claude, Codex and Gemini CLI adapters", "trace export", "redaction"]);
    c.mocked(&["the failing tools (fake-cli)"]);
    let pw = "DbPassw0rd-OB04-canary";
    let diag = format!(
        "Exit code 101\nerror[E0425]: cannot find value `undefined_total` in this scope\n --> src/lib.rs:3:5\nDATABASE_PASSWORD={pw}"
    );
    let claude = json!({ "calls": [{ "id": "msg_1", "text": "The build fails.", "tools": [
        { "id": "toolu_build", "name": "Bash", "input": { "command": "cargo build" }, "error": true, "output": diag },
        { "id": "toolu_lost", "name": "Bash", "input": { "command": "cargo test" }, "no_result": true } ] }] });
    let codex = json!({ "text": "Tests fail.", "tools": [{ "id": "item_7", "name": "shell", "input": { "command": "npm test" }, "error": true, "exit": 2, "output": "1 failing\n  AssertionError: expected 3 to equal 4" }] });
    let gemini = json!({ "text": "Missing file.", "tools": [{ "id": "read-1", "name": "read_file", "input": { "absolute_path": "/w/missing.txt" }, "error": true, "code": "FILE_NOT_FOUND", "output": "File not found: /w/missing.txt" }] });
    let mut found = vec![];
    for (name, provider, step, tool, code, context) in [
        ("claude", "claude", claude, "toolu_build", "exit 101", "error[E0425]"),
        ("codex", "openai", codex, "item_7", "exit 2", "AssertionError"),
        ("gemini", "gemini", gemini, "read-1", "FILE_NOT_FOUND", "File not found"),
    ] {
        let f = Fermi::at(&c, name, &[account("a", provider, vec![step])], "").unwrap();
        let hub = f.hub();
        let id = hub.submit(f.task("Build and test.", &c.work));
        let rec = wait_task(&hub, id, T).await.unwrap();
        let export = trace::export(&rec);
        c.json(&format!("{name}-trace.json"), &export);
        let op = ops(&export, "tool").into_iter().find(|o| o["id"] == tool).cloned().unwrap_or_default();
        let out = op["output"].as_str().unwrap_or_default();
        c.check(
            &format!("{name}Failure"),
            op["status"] == "error" && op["code"] == code && out.contains(context),
            format!("{tool}: status {} code {} output {:?}", op["status"], op["code"], out.lines().take(2).collect::<Vec<_>>()),
        );
        // Correlation: the task, the attempt and (for Claude) the model call that asked for it.
        let correlated = op["task"] == id && op["attempt"] == 1 && (name != "claude" || op["call"] == "msg_1");
        let listed = export["failures"].as_array().into_iter().flatten().any(|o| o["id"] == tool);
        c.check(
            &format!("{name}Correlated"),
            correlated && listed,
            format!("task {} attempt {} call {}", op["task"], op["attempt"], op["call"]),
        );
        if name == "claude" {
            let lost = ops(&export, "tool").into_iter().find(|o| o["id"] == "toolu_lost").cloned().unwrap_or_default();
            c.check("noSuccessFromSilence", lost["status"] == "no_result", format!("a tool that never reported back: {}", lost["status"]));
            c.check(
                "contextKept",
                out.contains("src/lib.rs:3:5") && !out.contains(pw) && out.contains("[REDACTED]"),
                "location kept, the password redacted",
            );
            // The hub writes the finished record just after the run ends: wait for it on disk.
            let file = f.home.join("history").join(format!("{id}.json"));
            let started = std::time::Instant::now();
            let mut saved = String::new();
            while started.elapsed() < T {
                saved = std::fs::read_to_string(&file).unwrap_or_default();
                if saved.contains("\"status\":\"done\"") || saved.contains("\"status\":\"failed\"") {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            c.check(
                "parentTrace",
                saved.contains("\"tool_result\"") && saved.contains("toolu_build") && !saved.contains(pw),
                "the failure is in the task's saved history, redacted",
            );
        }
        found.push(op);
    }
    c.json("failures.json", &json!(found));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ob_05_trace_persistence() {
    let mut c = Case::start("OB-05", "T1");
    c.real(&["fermi hub", "history files on disk", "a second hub instance (restart)"]);
    c.mocked(&["model output (fake-cli)"]);
    let step = json!({ "text": "Traced.", "tools": [{ "name": "Read", "input": { "file_path": "a.txt" } }, { "name": "Bash", "input": { "command": "cargo test" } }] });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step])], "").unwrap();
    let id = {
        let hub = f.hub();
        let id = hub.submit(f.task("Run something traceable.", &c.work));
        wait_task(&hub, id, Duration::from_secs(60)).await.unwrap();
        // Let the final write land, then "close the app".
        wait_saved(&f.home, id, T).await;
        id
    };
    let before = std::fs::read(f.home.join("history").join(format!("{id}.json"))).unwrap();
    let before: serde_json::Value = serde_json::from_slice(&before).unwrap();
    let hub2 = Hub::new(f.engine.clone(), "acceptance-restarted");
    let after = hub2.get(id).map(|t| serde_json::to_value(&t).unwrap());
    c.json("trace-before-restart.json", &before);
    c.json("trace-after-restart.json", &after.clone().unwrap_or_default());
    let after = after.unwrap_or_default();
    c.check("traceReadable", !after.is_null(), format!("task {id} after restart"));
    c.check(
        "operationsComplete",
        before["events"] == after["events"] && before["stamps"] == after["stamps"],
        format!("{} events before and after, with their sequence numbers", before["events"].as_array().map(|a| a.len()).unwrap_or(0)),
    );
    let tools = after["events"].as_array().into_iter().flatten().filter(|e| e["type"] == "tool").count();
    let results = after["events"].as_array().into_iter().flatten().filter(|e| e["type"] == "tool_result").count();
    c.check(
        "noDuplicates",
        tools == 2 && results == 2 && gapless(&after),
        format!("{tools} tool events and {results} results (2 each expected), seq without gaps"),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ob_06_resume_correlation() {
    let mut c = Case::start("OB-06", "T1 T3");
    c.real(&[
        "fermi hub",
        "history files on disk",
        "a second hub instance (restart)",
        "Claude CLI adapter (--resume)",
        "process cancellation",
    ]);
    c.mocked(&["model output (fake-cli)"]);
    // Attempt 1 does one tool, then hangs until it is interrupted.
    let first = json!({ "calls": [{ "id": "msg_1", "tools": [{ "id": "toolu_1", "name": "Read", "input": { "file_path": "PLAN.md" }, "output": "1. migrate 2. test" }] }],
        "hold": "never-released" });
    let second = json!({ "calls": [{ "id": "msg_2", "text": "Migration done, tests pass.",
        "tools": [{ "id": "toolu_2", "name": "Bash", "input": { "command": "cargo test" }, "output": "test result: ok" }] }] });
    let f = Fermi::new(&c, &[account("c1", "claude", vec![first, second])], "").unwrap();
    let (id, checkpoint) = {
        let hub = f.hub();
        let id = hub.submit(f.task("Migrate the schema, then run the tests.", &c.work));
        // The durable checkpoint: toolu_1's result is on disk.
        let start = std::time::Instant::now();
        let mut cp = Value::Null;
        while start.elapsed() < T {
            cp = history(&f, id);
            if cp.to_string().contains("\"tool_result\"") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        hub.cancel(id);
        tokio::time::sleep(Duration::from_millis(300)).await;
        (id, cp)
    };
    c.json("checkpoint.json", &checkpoint);
    let k = checkpoint["events"].as_array().map_or(0, |e| e.len());
    // "Restart": a new hub resumes the interrupted task.
    let hub2 = Hub::new(f.engine.clone(), "acceptance-restarted");
    let interrupted = hub2.get(id).map(|t| t.status);
    c.check("interrupted", interrupted == Some(TaskStatus::Cancelled), format!("{interrupted:?} after the restart"));
    let resumed = hub2.resume(id);
    c.check("resumeAccepted", resumed.is_ok(), format!("{resumed:?}"));
    let rec = wait_task(&hub2, id, T).await.unwrap();
    wait_saved(&f.home, id, T).await;
    let saved = history(&f, id);
    c.json("trace-after-resume.json", &saved);
    c.check("sameTask", rec.status == TaskStatus::Done && saved["id"] == id, format!("task {id} {:?}", rec.status));
    let events = saved["events"].as_array().cloned().unwrap_or_default();
    let stamps = saved["stamps"].as_array().cloned().unwrap_or_default();
    c.check(
        "checkpointKept",
        k > 0
            && events.len() > k
            && events[..k] == checkpoint["events"].as_array().unwrap()[..]
            && stamps[..k] == checkpoint["stamps"].as_array().unwrap()[..k],
        format!("the first {k} events and their stamps are unchanged"),
    );
    let attempt = |i: usize| stamps.get(i).and_then(|s| s["attempt"].as_u64()).unwrap_or(0);
    let split = (0..stamps.len()).find(|&i| attempt(i) == 2).unwrap_or(0);
    let ordered = split > 0 && (0..split).all(|i| attempt(i) == 1) && (split..stamps.len()).all(|i| attempt(i) == 2);
    c.check(
        "attemptsDistinct",
        ordered && saved["attempt"] == 2,
        format!("events 1…{split} are attempt 1, {}…{} attempt 2", split + 1, stamps.len()),
    );
    c.check("noGapsOrDuplicates", gapless(&saved), format!("seq 1…{} without gaps or repeats", stamps.len()));
    let tool_ids: Vec<&str> = events.iter().filter(|e| e["type"] == "tool").filter_map(|e| e["id"].as_str()).collect();
    c.check("noReplayedEvents", tool_ids == ["toolu_1", "toolu_2"], format!("tool invocations {tool_ids:?}"));
    // The resumed run continued the same provider session.
    let calls = f.calls("c1");
    let arg = |n: usize, flag: &str| {
        let a: Vec<String> = calls.get(n).and_then(|c| serde_json::from_value(c["args"].clone()).ok()).unwrap_or_default();
        a.iter().position(|x| x == flag).and_then(|i| a.get(i + 1).cloned())
    };
    let session = arg(0, "--session-id");
    c.check(
        "sameSession",
        session.is_some() && arg(1, "--resume") == session,
        format!("first run {session:?}, resumed with {:?}", arg(1, "--resume")),
    );
    // The interrupted recording replays up to where it stopped, then the resumed attempt.
    let replay = trace::replay(&f.engine.cfg, &f.home, id, &c.work.join("replay")).await;
    let detail = match &replay {
        Ok(r) => format!("{} events compared, differences {:?}", r.compared, r.differences),
        Err(e) => format!("{e:#}"),
    };
    c.check("replaysAcrossResume", replay.is_ok_and(|r| r.matches), detail);
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ob_07_secret_redaction() {
    let mut c = Case::start("OB-07", "T1 T3");
    c.real(&["fermi engine and hub", "redaction before storage", "tapes", "trace export", "the live event stream"]);
    c.mocked(&["model output, tool output and a provider error (fake-cli)"]);
    let canaries = [
        "ghp_OB07canaryPersonalAccessToken0000000",   // a GitHub token in tool output
        "sk-ant-api03-OB07canaryInStructuredField00", // an API key in a structured tool input
        "ACME-7f3a9c2e5b1d4f60",                      // the project's own format, from [trace] redact
        "Canary-Passw0rd-OB07",                       // a password=… value in an error
        "sk-proj-OB07canaryInProviderError000000",    // in the provider's error message
    ];
    let ok = json!({ "calls": [{ "id": "msg_1", "text": format!("Deployed with license {}.", canaries[2]), "tools": [
        { "id": "t1", "name": "Bash", "input": { "command": "gh auth status" }, "output": format!("Logged in to github.com\n  Token: {}\n  Scopes: repo", canaries[0]) },
        { "id": "t2", "name": "WebFetch", "input": { "url": "https://api.example.invalid/v1", "headers": { "x-api-key": canaries[1] } }, "output": "200 OK" },
        { "id": "t3", "name": "Bash", "input": { "command": "psql -c 'select 1'" }, "error": true,
          "output": format!("Exit code 2\npsql: error: connection to server failed: FATAL: password authentication failed\nconnection string: password={}", canaries[3]) } ] }] });
    let failing = json!({ "error": format!("API Error: 401 invalid x-api-key {}", canaries[4]) });
    // `[trace]` opens its own table after the profile the harness writes.
    let f = Fermi::new(&c, &[account("c1", "claude", vec![ok, failing])], "[trace]\nredact = [\"ACME-[0-9a-f]{16}\"]\n").unwrap();
    let hub = f.hub();
    let mut bus = hub.subscribe();
    let a = hub.submit(f.task("Deploy and check the database.", &c.work));
    wait_task(&hub, a, T).await.unwrap();
    let b = hub.submit(f.task("Try again.", &c.work));
    wait_task(&hub, b, T).await.unwrap();
    wait_saved(&f.home, a, T).await;
    wait_saved(&f.home, b, T).await;
    let mut live = String::new();
    while let Ok(m) = bus.try_recv() {
        live += &m.to_string();
    }
    let ra = fermi_core::hub::read_task(&f.home.join("history"), a).unwrap();
    let rb = fermi_core::hub::read_task(&f.home.join("history"), b).unwrap();
    let exports = format!("{}{}", trace::export(&ra), trace::export(&rb));
    c.text("trace-exports.json", &exports);
    let mut scanned: Vec<(String, String)> = files_text(&f.home.join("history"));
    scanned.extend(files_text(&f.home.join("tapes")));
    scanned.push(("usage.jsonl".into(), std::fs::read_to_string(f.home.join("usage.jsonl")).unwrap_or_default()));
    scanned.push(("trace export".into(), exports.clone()));
    scanned.push(("live event stream".into(), live.clone()));
    let tapes = files_text(&f.home.join("tapes")).len();
    let mut hits = vec![];
    for (where_, text) in &scanned {
        for k in canaries {
            if text.contains(k) {
                hits.push(format!("{k} in {where_}"));
            }
        }
    }
    c.json("canary-scan.json", &json!({ "canaries": canaries.len(), "places": scanned.iter().map(|(w, t)| json!({ "where": w, "bytes": t.len() })).collect::<Vec<_>>(), "hits": hits }));
    c.check(
        "scannedEverywhere",
        tapes >= 2 && !live.is_empty() && scanned.len() >= 6,
        format!("{} places scanned, {tapes} tape files", scanned.len()),
    );
    c.check("noCanarySurvives", hits.is_empty(), format!("{hits:?}"));
    c.check(
        "redactedBeforeStorage",
        scanned.iter().filter(|(w, _)| w.contains("tapes") && w.ends_with(".jsonl")).all(|(_, t)| t.contains("[REDACTED]")),
        "the raw provider recordings are redacted as they are written",
    );
    let history_a = std::fs::read_to_string(f.home.join("history").join(format!("{a}.json"))).unwrap_or_default();
    let useful = [
        "Logged in to github.com",
        "Scopes: repo",
        "password authentication failed",
        "https://api.example.invalid/v1",
        "exit 2",
        "Deployed with license",
    ];
    let missing: Vec<&str> = useful.iter().copied().filter(|u| !history_a.contains(u)).collect();
    c.check("diagnosticsRemain", missing.is_empty(), format!("missing: {missing:?}"));
    let error = rb
        .events
        .iter()
        .find_map(|e| if let Event::Done { ok: false, summary } = e { Some(summary.clone()) } else { None })
        .unwrap_or_default();
    c.check("errorExportRedacted", error.contains("invalid x-api-key") && error.contains("[REDACTED]"), format!("error: {error}"));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ob_08_deterministic_replay() {
    let mut c = Case::start("OB-08", "T1");
    c.real(&["fermi engine and hub", "the CLI adapters' parsing (fed from tapes)", "trace::replay", "normalization and hashing"]);
    c.mocked(&["model output (fake-cli, recorded to tapes on the original run)"]);
    let first = five_ops();
    let second = json!({ "calls": [{ "id": "msg_C", "usage": { "input_tokens": 90, "output_tokens": 12 }, "text": "Fixed and verified.", "tools": [
        { "id": "toolu_4", "name": "Edit", "input": { "file_path": "src/lib.rs" }, "output": "edited" },
        { "id": "toolu_5", "name": "Bash", "input": { "command": "cargo test" }, "error": true, "output": "Exit code 101\ntest add ... FAILED" } ] }] });
    let f = Fermi::new(&c, &[account("c1", "claude", vec![first, second])], "").unwrap();
    let hub = f.hub();
    let id = hub.submit(f.task("Check add, then fix it.", &c.work));
    wait_task(&hub, id, T).await.unwrap();
    hub.follow_up(id, "Now fix the failing case.", Default::default()).unwrap();
    wait_task(&hub, id, T).await.unwrap();
    wait_saved(&f.home, id, T).await;
    let calls_before = f.calls("c1").len();
    // The replay can't start a CLI even by mistake: the binaries point nowhere.
    let mut cfg = f.engine.cfg.clone();
    let nowhere = c.work.join("no-such-cli").display().to_string();
    cfg.bins.claude = Some(nowhere.clone());
    cfg.bins.codex = Some(nowhere.clone());
    cfg.bins.gemini = Some(nowhere);
    let r1 = trace::replay(&cfg, &f.home, id, &c.work.join("replay-1")).await;
    let r2 = trace::replay(&cfg, &f.home, id, &c.work.join("replay-2")).await;
    let (r1, r2) = match (r1, r2) {
        (Ok(a), Ok(b)) => (a, b),
        (a, b) => {
            c.check("replayRan", false, format!("{:?} / {:?}", a.err(), b.err()));
            return c.finish();
        }
    };
    c.json("replay-report.json", &serde_json::to_value(&r1).unwrap());
    c.check(
        "matches",
        r1.matches && r1.differences.is_empty(),
        format!("{} events compared; differences {:?}", r1.compared, r1.differences),
    );
    c.check("stateHash", r1.original_hash == r1.replay_hash, format!("recorded {} replayed {}", r1.original_hash, r1.replay_hash));
    c.check("bothAttempts", r1.attempts == [1, 2] && r1.compared >= 10, format!("attempts {:?}, {} events", r1.attempts, r1.compared));
    let same = serde_json::to_string(&r1.replayed).unwrap() == serde_json::to_string(&r2.replayed).unwrap();
    c.check("fixedClock", same, "two replays produce byte-identical records (clock and session ids fixed)");
    c.check("noCliStarted", f.calls("c1").len() == calls_before, format!("{calls_before} CLI invocations before and after"));
    let tape_ok = r1
        .tapes
        .iter()
        .all(|(name, hash)| std::fs::read(trace::tapes_dir(&f.home, id).join(name)).map(|b| sha256(&b) == *hash).unwrap_or(false));
    c.check("cassetteHashes", r1.tapes.len() == 2 && tape_ok, format!("{:?}", r1.tapes));
    c.check("noLiveClaim", r1.text_source == "tape", "the report says the text came from the recording");

    // Control: a tampered recording is caught.
    let tampered = c.work.join("tampered-home");
    for (from, to) in [("history", "history"), ("tapes", "tapes")] {
        copy(&f.home.join(from), &tampered.join(to));
    }
    let tape = trace::tapes_dir(&tampered, id).join("2-1.jsonl");
    let text = std::fs::read_to_string(&tape).unwrap_or_default().replace("test add ... FAILED", "test add ... ok");
    std::fs::write(&tape, text).unwrap();
    let r3 = trace::replay(&cfg, &tampered, id, &c.work.join("replay-3")).await.ok();
    let caught = r3.as_ref().is_some_and(|r| !r.matches && r.original_hash != r.replay_hash);
    c.check("tamperDetected", caught, format!("{:?}", r3.map(|r| r.differences)));
    c.finish();
}

fn copy(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).into_iter().flatten().filter_map(|e| e.ok()) {
        if e.path().is_dir() {
            copy(&e.path(), &to.join(e.file_name()));
        } else {
            std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}
