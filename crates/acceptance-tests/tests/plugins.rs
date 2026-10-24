//! System 7: composable harness and plugin system.

use acceptance_tests::{account, fake_plugin, plugin_fixture, wait_task, Case, Fermi};
use fermi_core::events::Event;
use fermi_core::hub::TaskStatus;
use fermi_core::plugins::{self, Kind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

const T: Duration = Duration::from_secs(60);

fn lines(p: &Path) -> Vec<Value> {
    std::fs::read_to_string(p).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn events(f: &Fermi) -> Vec<Value> {
    lines(&plugins::dir(&f.home).join("events.jsonl"))
}

/// The registry as plain JSON, for snapshots and comparisons.
fn snapshot(f: &Fermi) -> Value {
    serde_json::to_value(plugins::registry(&f.engine)).unwrap()
}

fn ids(f: &Fermi) -> Vec<String> {
    plugins::registry(&f.engine).entries.iter().map(|e| format!("{} {}", e.kind.as_str(), e.id)).collect()
}

/// The tools fermi's bridge offers a Claude agent in profile `p`.
fn bridge_tools(f: &Fermi, cwd: &Path) -> String {
    let profile = f.engine.cfg.resolve_profile(Some("p"), cwd).unwrap();
    fermi_core::mcp::bridge_for(&f.engine, &profile).unwrap().map(|b| b.allowed_tools).unwrap_or_default()
}

/// Where a test plugin may write what it records (its manifest's `read_write`).
fn probe(c: &Case) -> PathBuf {
    let p = c.work.join("probe");
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn project(c: &Case) -> PathBuf {
    let p = c.work.join("project");
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[tokio::test(flavor = "multi_thread")]
async fn pl_01_plugin_registration() {
    let mut c = Case::start("PL-01", "T1");
    c.real(&["plugin manager (install, enable)", "MCP client over stdio", "sandboxed plugin process", "fermi's MCP bridge tool list"]);
    c.mocked(&["the plugin program (fake-plugin, a real MCP server)"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let pr = probe(&c);
    let marker = pr.join("sample-starts.txt");
    let src = plugin_fixture("sample", &c.work.join("src/sample"), &[("MARKER", &marker), ("PROBE", &pr)]).unwrap();
    let cwd = project(&c);
    let installed = plugins::install(&f.engine, &src).await;
    c.check("installed", installed.is_ok(), format!("{:?}", installed.as_ref().err()));
    let reg = ids(&f);
    c.json("registry.json", &snapshot(&f));
    c.check(
        "onlyDeclared",
        reg == ["tool sample.echo", "command sample.greet"],
        format!("{reg:?} (the plugin also offers `add`, undeclared)"),
    );
    let echo = plugins::call(&f.engine, Kind::Tool, "echo", &json!({ "text": "ping" }), &cwd).await;
    let greet = plugins::call(&f.engine, Kind::Command, "greet", &json!({ "name": "Ada" }), &cwd).await;
    let undeclared = plugins::call(&f.engine, Kind::Tool, "add", &json!({ "a": 1, "b": 2 }), &cwd).await;
    c.json(
        "invocations.json",
        &json!({ "echo": echo.as_ref().map_err(|e| e.to_string()).ok(), "greet": greet.as_ref().map_err(|e| e.to_string()).ok(),
            "add": undeclared.as_ref().err().map(|e| e.to_string()) }),
    );
    let attributed = |o: &plugins::Outcome| o.ok && o.plugin == "sample" && o.version == "1.2.0";
    c.check(
        "bothCallable",
        echo.as_ref().is_ok_and(|o| attributed(o) && o.text == "ping")
            && greet.as_ref().is_ok_and(|o| attributed(o) && o.text.starts_with("Hello, Ada!")),
        "echo and greet answered, attributed to sample 1.2.0",
    );
    c.check("undeclaredUnavailable", undeclared.is_err(), format!("add: {:?}", undeclared.err().map(|e| e.to_string())));
    let tools = bridge_tools(&f, &cwd);
    c.check(
        "offeredToAgents",
        tools.contains("mcp__fermi__plugin__sample__echo") && !tools.contains("sample__greet") && !tools.contains("sample__add"),
        "the tool is on the bridge; the command and the undeclared tool aren't",
    );
    let ev = events(&f);
    c.json("plugin-events.json", &json!(ev));
    let enabled = ev.iter().find(|e| e["event"] == "enabled").cloned().unwrap_or_default();
    c.check(
        "registrationEvents",
        ev.iter().any(|e| e["event"] == "installed" && e["version"] == "1.2.0")
            && enabled["ignored_undeclared"] == json!(["add"])
            && enabled["registered"].as_array().map_or(0, |a| a.len()) == 2,
        format!("enabled: {enabled}"),
    );
    let starts = std::fs::read_to_string(&marker).unwrap_or_default().lines().count();
    c.check("processPerCall", starts == 3, format!("{starts} starts: one to register, one per call (the refused call started nothing)"));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn pl_02_plugin_disabling() {
    let mut c = Case::start("PL-02", "T1");
    c.real(&["plugin manager", "registry", "fermi's MCP bridge tool list"]);
    c.mocked(&["the plugin programs (fake-plugin)"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let cwd = project(&c);
    let pr = probe(&c);
    let m = pr.join("m.txt");
    plugins::install(&f.engine, &plugin_fixture("sample", &c.work.join("src/sample"), &[("MARKER", &m), ("PROBE", &pr)]).unwrap())
        .await
        .unwrap();
    plugins::install(&f.engine, &plugin_fixture("other", &c.work.join("src/other"), &[]).unwrap()).await.unwrap();
    let before = snapshot(&f);
    c.json("registry-before.json", &before);
    let r = plugins::disable(&f.engine, "sample");
    c.check("disabled", r.is_ok(), format!("{r:?}"));
    let after = snapshot(&f);
    c.json("registry-after.json", &after);
    let gone = plugins::call(&f.engine, Kind::Tool, "sample.echo", &json!({ "text": "x" }), &cwd).await;
    let other = plugins::call(&f.engine, Kind::Command, "sum", &json!({ "a": 2, "b": 3 }), &cwd).await;
    c.json("outcomes.json", &json!({ "sample.echo": gone.as_ref().err().map(|e| e.to_string()), "sum": other.as_ref().ok() }));
    c.check(
        "unavailable",
        gone.as_ref().is_err_and(|e| e.to_string().contains("no enabled plugin")),
        format!("{:?}", gone.err().map(|e| e.to_string())),
    );
    c.check(
        "otherWorks",
        other.as_ref().is_ok_and(|o| o.ok && o.text == "5" && o.plugin == "other"),
        format!("{:?}", other.map(|o| o.text)),
    );
    let stale = after["entries"].as_array().unwrap().iter().any(|e| e["plugin"] == "sample");
    c.check(
        "noStaleRegistration",
        !stale
            && before["entries"].as_array().unwrap().len() == 3
            && ids(&f) == ["command other.sum"]
            && !bridge_tools(&f, &cwd).contains("plugin__sample"),
        format!(
            "before {} entries, after {:?}; nothing of sample left on the bridge",
            before["entries"].as_array().unwrap().len(),
            ids(&f)
        ),
    );
    // Enabling it again brings its capabilities back.
    let back = plugins::enable(&f.engine, "sample").await;
    c.check("reenabled", back.is_ok() && ids(&f).len() == 3, format!("{:?}", ids(&f)));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn pl_03_command_collisions() {
    let mut c = Case::start("PL-03", "T0 T1");
    c.real(&["plugin manager", "registry and its collision policy", "plugin calls"]);
    c.mocked(&["the plugin programs (fake-plugin)"]);
    let cwd = project(&c);
    let mut snaps = vec![];
    for (policy, extra) in [("namespace", ""), ("reject", "[plugins]\ncollisions = \"reject\"\n")] {
        let mut per_order = vec![];
        for order in [["alpha", "beta"], ["beta", "alpha"]] {
            let f = Fermi::at(&c, &format!("{policy}-{}", order[0]), &[], extra).unwrap();
            for p in order {
                let _ =
                    plugins::install(&f.engine, &plugin_fixture(p, &c.work.join(format!("src-{policy}-{}/{p}", order[0])), &[]).unwrap())
                        .await;
            }
            let snap = snapshot(&f);
            c.json(&format!("{policy}-{}-first.json", order[0]), &snap);
            let bare = plugins::call(&f.engine, Kind::Command, "greet", &json!({ "name": "Ada" }), &cwd).await;
            let a = plugins::call(&f.engine, Kind::Command, "alpha.greet", &json!({ "name": "Ada" }), &cwd).await;
            let b = plugins::call(&f.engine, Kind::Command, "beta.greet", &json!({ "name": "Ada" }), &cwd).await;
            per_order.push((
                snap,
                bare.map(|o| o.text).map_err(|e| e.to_string()),
                a.map(|o| o.text).map_err(|e| e.to_string()),
                b.map(|o| o.text).map_err(|e| e.to_string()),
            ));
        }
        let (s1, bare, a, b) = &per_order[0];
        let same = per_order[0].0 == per_order[1].0 && per_order[0].1 == per_order[1].1;
        c.check(&format!("{policy}OrderIndependent"), same, "the same registry and the same answer to `greet` in both install orders");
        let diag = s1["collisions"][0]["outcome"].as_str().unwrap_or_default().to_string();
        match policy {
            "namespace" => {
                c.check(
                    "namespaced",
                    a.as_ref().is_ok_and(|t| t.contains("(alpha)")) && b.as_ref().is_ok_and(|t| t.contains("(beta)")),
                    format!("alpha.greet → {a:?}; beta.greet → {b:?}"),
                );
                c.check(
                    "bareNameAmbiguous",
                    bare.as_ref().is_err_and(|e| e.contains("alpha.greet") && e.contains("beta.greet")) && diag.contains("ambiguous"),
                    format!("greet → {bare:?}; diagnostic: {diag}"),
                );
            }
            _ => {
                let none = s1["entries"].as_array().unwrap().is_empty();
                c.check(
                    "rejected",
                    none && a.is_err() && b.is_err() && bare.as_ref().is_err_and(|e| e.contains("registered to neither")),
                    format!("registry empty: {none}; greet → {bare:?}; diagnostic: {diag}"),
                );
            }
        }
        snaps.push(json!({ "policy": policy, "registry": s1 }));
    }
    c.json("collisions.json", &json!(snaps));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn pl_04_plugin_fault_isolation() {
    let mut c = Case::start("PL-04", "T1 T3");
    c.real(&["plugin host (one process per call)", "timeouts", "fermi hub and engine"]);
    c.mocked(&["the plugin programs (fake-plugin)", "model output (fake-cli)"]);
    let f = Fermi::new(&c, &[account("c1", "claude", vec![json!({ "text": "still here" })])], "").unwrap();
    let cwd = project(&c);
    plugins::install(&f.engine, &plugin_fixture("faulty", &c.work.join("src/faulty"), &[]).unwrap()).await.unwrap();
    plugins::install(&f.engine, &plugin_fixture("other", &c.work.join("src/other"), &[]).unwrap()).await.unwrap();
    let boom = plugins::call(&f.engine, Kind::Tool, "boom", &json!({}), &cwd).await.map_err(|e| format!("{e:#}"));
    let started = std::time::Instant::now();
    let hang = plugins::call(&f.engine, Kind::Tool, "hang", &json!({}), &cwd).await.map_err(|e| format!("{e:#}"));
    let hang_secs = started.elapsed().as_secs_f64();
    let fail = plugins::call(&f.engine, Kind::Tool, "fail", &json!({}), &cwd).await;
    let healthy = plugins::call(&f.engine, Kind::Command, "sum", &json!({ "a": 20, "b": 22 }), &cwd).await;
    c.json(
        "diagnostics.json",
        &json!({ "boom": boom.as_ref().err(), "hang": hang.as_ref().err(), "hang_secs": hang_secs,
        "fail": fail.as_ref().ok(), "sum": healthy.as_ref().ok() }),
    );
    let b = boom.as_ref().err().cloned().unwrap_or_default();
    c.check("crashAttributed", b.contains("plugin `faulty` 0.9.0 failed in `boom`") && b.contains("distinctive failure 7f3a"), b.clone());
    let h = hang.as_ref().err().cloned().unwrap_or_default();
    c.check(
        "hangBounded",
        h.contains("faulty") && h.contains("no answer within 3 s") && hang_secs < 15.0,
        format!("{h} after {hang_secs:.1} s"),
    );
    c.check(
        "toolErrorReported",
        fail.as_ref().is_ok_and(|o| !o.ok && o.text.contains("fail says no")),
        "a tool error comes back as a failed outcome, not a crash",
    );
    c.check("unrelatedWorks", healthy.as_ref().is_ok_and(|o| o.ok && o.text == "42"), format!("{:?}", healthy.map(|o| o.text)));
    // The host is fine: tasks still run, and the faulty plugin still answers its next call.
    let hub = f.hub();
    let id = hub.submit(f.task("Say something.", &cwd));
    let rec = wait_task(&hub, id, T).await.unwrap();
    let again = plugins::call(&f.engine, Kind::Tool, "fail", &json!({}), &cwd).await;
    c.check(
        "hostHealthy",
        rec.status == TaskStatus::Done && again.is_ok(),
        format!("task {:?}; faulty answers again: {}", rec.status, again.is_ok()),
    );
    let faults: Vec<Value> = events(&f).into_iter().filter(|e| e["event"] == "fault").collect();
    c.json("fault-events.json", &json!(faults));
    c.check(
        "faultsLogged",
        faults.len() == 2 && faults.iter().all(|e| e["plugin"] == "faulty"),
        format!("{} fault events, all for faulty", faults.len()),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn pl_05_mcp_interoperability() {
    let mut c = Case::start("PL-05", "T1");
    c.real(&["MCP client (server/discover for current servers, initialize for older ones)", "schema validation", "fermi plugin add-mcp"]);
    c.mocked(&["the MCP servers (fake-plugin, both protocol eras)"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let cwd = project(&c);
    let exe = fake_plugin().display().to_string();
    let mut results = vec![];
    for (name, legacy) in [("modern-echo", false), ("legacy-echo", true)] {
        let log = probe(&c).join(format!("{name}.log"));
        let mut cmd = vec![
            exe.clone(),
            "--label".into(),
            name.into(),
            "--tools".into(),
            "echo,add".into(),
            "--log".into(),
            log.display().to_string(),
        ];
        if legacy {
            cmd.push("--legacy".into());
        }
        let added =
            plugins::add_mcp(&f.engine, name, cmd, plugins::Access { write: vec![probe(&c).display().to_string()], ..Default::default() })
                .await;
        c.check(
            &format!("{name}Discovered"),
            added.as_ref().is_ok_and(|e| e.len() == 2),
            format!("{:?}", added.as_ref().map(|e| e.iter().map(|x| x.id.clone()).collect::<Vec<_>>()).map_err(|e| e.to_string())),
        );
        let reg = plugins::registry(&f.engine);
        let echo = reg.resolve(Kind::Tool, &format!("{name}.echo")).cloned();
        let want = json!({ "type": "object", "properties": { "text": { "type": "string", "minLength": 1 } }, "required": ["text"], "additionalProperties": false });
        c.check(
            &format!("{name}SchemaPreserved"),
            echo.as_ref().is_ok_and(|e| e.schema == want),
            format!("{:?}", echo.as_ref().map(|e| e.schema.clone()).ok()),
        );
        let id = format!("{name}.echo");
        let ok = plugins::call(&f.engine, Kind::Tool, &id, &json!({ "text": "hello" }), &cwd).await;
        let mut invalid = vec![];
        for bad in [json!({}), json!({ "text": 5 }), json!({ "text": "a", "loud": true })] {
            invalid.push(plugins::call(&f.engine, Kind::Tool, &id, &bad, &cwd).await.err().map(|e| e.to_string()).unwrap_or_default());
        }
        let reqs = lines(&log);
        let calls = reqs.iter().filter(|r| r["method"] == "tools/call").count();
        let handshake: Vec<&str> =
            reqs.iter().filter_map(|r| r["method"].as_str()).filter(|m| *m == "server/discover" || *m == "initialize").collect();
        c.check(
            &format!("{name}Valid"),
            ok.as_ref().is_ok_and(|o| o.ok && o.text == "hello" && o.structured == Some(json!({ "echo": "hello" }))),
            format!("{:?}", ok.as_ref().map(|o| (&o.text, &o.structured))),
        );
        c.check(
            &format!("{name}InvalidRejected"),
            invalid[0].contains("arguments.text is required")
                && invalid[1].contains("should be string")
                && invalid[2].contains("isn't a known argument")
                && calls == 1,
            format!("{invalid:?}; the server received {calls} tools/call (only the valid one)"),
        );
        let era_ok = if legacy {
            handshake.contains(&"initialize")
        } else {
            !handshake.contains(&"initialize")
                && reqs
                    .iter()
                    .any(|r| r["method"] == "tools/call" && r["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] == "2026-07-28")
        };
        c.check(&format!("{name}Protocol"), era_ok, format!("handshake requests: {handshake:?}"));
        results.push(json!({ "server": name, "schema": echo.ok().map(|e| e.schema), "valid": ok.ok().map(|o| o.text), "invalid": invalid, "requests": reqs }));
    }
    c.json("mcp-interop.json", &json!(results));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn pl_06_adapter_substitution() {
    let mut c = Case::start("PL-06", "T1");
    c.real(&["fermi hub and engine (the standard task service)", "plugin adapter path", "failover", "usage log"]);
    c.mocked(&["the adapter plugin's replies (fake-plugin, scripted)", "the fallback account's CLI (fake-cli)"]);
    let pr = probe(&c);
    let script = pr.join("replies.json");
    let reply = json!({ "text": "Added the test.", "session": "adapter-session-1",
        "usage": { "input_tokens": 120, "cache_read_input_tokens": 30, "output_tokens": 40 },
        "calls": [{ "id": "a-call-1", "model": "scripted-model", "usage": { "input_tokens": 120, "output_tokens": 40 } }],
        "tools": [{ "id": "a-tool-1", "name": "Edit", "input": { "file_path": "src/lib.rs" }, "output": "edited", "status": "ok" }] });
    std::fs::write(&script, serde_json::to_vec(&json!([reply, { "error": "Claude AI usage limit reached" }])).unwrap()).unwrap();
    let log = pr.join("adapter.log");
    let fallback = json!({ "text": "Done by the fallback.", "tools": [{ "name": "Edit", "input": { "file_path": "src/lib.rs" } }] });
    let f = Fermi::new(
        &c,
        &[account("ad", "claude", vec![]).adapter("adapter.scripted"), account("fb", "claude", vec![fallback.clone(), fallback])],
        "",
    )
    .unwrap();
    let cwd = project(&c);
    plugins::install(
        &f.engine,
        &plugin_fixture("adapter", &c.work.join("src/adapter"), &[("SCRIPT", &script), ("LOG", &log), ("PROBE", &pr)]).unwrap(),
    )
    .await
    .unwrap();
    let hub = f.hub();
    let a = hub.submit(f.task("Add a test for add().", &cwd));
    let ra = wait_task(&hub, a, T).await.unwrap();
    let b = hub.submit(f.task("Add another test.", &cwd));
    let rb = wait_task(&hub, b, T).await.unwrap();
    // The same exchange through the built-in adapter, for comparison.
    let reference = Fermi::at(
        &c,
        "builtin",
        &[account(
            "fb",
            "claude",
            vec![json!({ "text": "Added the test.", "tools": [{ "name": "Edit", "input": { "file_path": "src/lib.rs" } }] })],
        )],
        "",
    )
    .unwrap();
    let rh = reference.hub();
    let r = rh.submit(reference.task("Add a test for add().", &cwd));
    let rr = wait_task(&rh, r, T).await.unwrap();
    let kinds = |rec: &fermi_core::hub::TaskRec| -> Vec<String> {
        rec.events.iter().map(|e| serde_json::to_value(e).unwrap()["type"].as_str().unwrap().to_string()).filter(|k| k != "call").collect()
    };
    c.json("adapter-task.json", &serde_json::to_value(&ra).unwrap());
    c.json("failover-task.json", &serde_json::to_value(&rb).unwrap());
    c.json("builtin-task.json", &serde_json::to_value(&rr).unwrap());
    let routed = |rec: &fermi_core::hub::TaskRec| {
        rec.events
            .iter()
            .filter_map(|e| if let Event::Routed { account, .. } = e { Some(account.clone()) } else { None })
            .collect::<Vec<_>>()
    };
    c.check(
        "adapterUsed",
        ra.status == TaskStatus::Done && routed(&ra) == ["ad"] && lines(&log).iter().any(|l| l["method"] == "tools/call"),
        format!("{:?} via {:?}", ra.status, routed(&ra)),
    );
    c.check(
        "sameLifecycle",
        kinds(&ra) == kinds(&rr),
        format!("adapter {:?} / built-in {:?} (per-call usage events aside)", kinds(&ra), kinds(&rr)),
    );
    let tool = ra.events.iter().any(|e| matches!(e, Event::Tool { id: Some(i), .. } if i == "a-tool-1"))
        && ra.events.iter().any(|e| matches!(e, Event::ToolResult { id, status, .. } if id == "a-tool-1" && status == "ok"))
        && ra.events.iter().any(|e| matches!(e, Event::Call { id, .. } if id == "a-call-1"));
    c.check(
        "normalizedEvents",
        tool && ra.session.as_deref() == Some("adapter-session-1"),
        "tool, result, model call and session in the usual events",
    );
    let usage: Vec<Value> = lines(&f.home.join("usage.jsonl"));
    c.json("usage.json", &json!(usage));
    let first = usage.iter().find(|u| u["account"] == "ad" && u["status"] == "ok").cloned().unwrap_or_default();
    c.check(
        "normalizedUsage",
        first["input_tokens"] == 150
            && first["cached_input_tokens"] == 30
            && first["output_tokens"] == 40
            && first["cache_write_tokens"].is_null()
            && first["task"] == a,
        format!("{first}"),
    );
    let failover =
        rb.events.iter().any(|e| matches!(e, Event::Failover { account, reason } if account == "ad" && reason.contains("limit")));
    c.check(
        "normalizedErrors",
        rb.status == TaskStatus::Done && failover && routed(&rb) == ["ad", "fb"],
        format!("{:?}; routed {:?}; the adapter's limit error became a failover", rb.status, routed(&rb)),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn pl_07_plugin_boundary_enforcement() {
    let mut c = Case::start("PL-07", "T3");
    c.real(&[
        "plugin sandbox (environment allowlist everywhere; Landlock files on Linux)",
        "argument scope and deny_read checks",
        "fermi's MCP bridge",
    ]);
    c.mocked(&["the adversarial plugin (fake-plugin)"]);
    let canary = "PL07-SENTINEL-c4n4ry-0d9e";
    let zone = c.work.join("secret-zone");
    std::fs::create_dir_all(&zone).unwrap();
    let sentinel = zone.join("sentinel.txt");
    std::fs::write(&sentinel, canary).unwrap();
    let cwd = project(&c);
    std::fs::write(cwd.join(".env"), canary).unwrap();
    let deny = format!("[security]\ndeny_read = [\"{}/.env\"]\n", cwd.display().to_string().replace('\\', "/"));
    let f = Fermi::new(&c, &[], &deny).unwrap();
    let pr = probe(&c);
    let log = pr.join("adversarial.log");
    plugins::install(
        &f.engine,
        &plugin_fixture("adversarial", &c.work.join("src/adversarial"), &[("SENTINEL", &sentinel), ("LOG", &log), ("PROBE", &pr)]).unwrap(),
    )
    .await
    .unwrap();
    let call = |name: &'static str, args: Value| {
        let (f, cwd) = (&f, &cwd);
        async move { plugins::call(&f.engine, Kind::Tool, name, &args, cwd).await.map(|o| (o.ok, o.text)).map_err(|e| format!("{e:#}")) }
    };
    // Through the hook: a path argument outside the scope, and one deny_read forbids.
    let hook = call("read_file", json!({ "path": sentinel.display().to_string() })).await;
    let traversal = call("read_file", json!({ "path": "../secret-zone/sentinel.txt" })).await;
    let denied = call("read_file", json!({ "path": ".env" })).await;
    let profile = f.engine.cfg.resolve_profile(Some("p"), &cwd).unwrap();
    let bridge = fermi_core::mcp::call_tool(
        &f.engine,
        &profile,
        &cwd,
        "plugin__adversarial__read_file",
        &json!({ "path": sentinel.display().to_string() }),
    )
    .await
    .map_err(|e| format!("{e:#}"));
    // Directly: the plugin reads the sentinel on its own.
    let direct = call("read_sentinel", json!({})).await;
    // Its environment: none of this process's variables beyond the system ones.
    let env = call("env", json!({})).await;
    let reached = lines(&log).iter().filter(|r| r["method"] == "tools/call" && r["params"]["name"] == "read_file").count();
    let outputs = json!({ "hook": hook, "traversal": traversal, "deny_read": denied, "bridge": bridge, "direct": direct, "env": env });
    c.json("attempts.json", &outputs);
    let refused = |r: &Result<(bool, String), String>| {
        r.as_ref().is_err_and(|e| e.contains("refused") && (e.contains("outside the plugin's scope") || e.contains("deny_read")))
    };
    c.check(
        "hookBlocked",
        refused(&hook)
            && refused(&traversal)
            && refused(&denied)
            && bridge.as_ref().is_err_and(|e| e.contains("outside the plugin's scope")),
        "path arguments outside the scope or under deny_read are refused, also from an agent through the bridge",
    );
    c.check("neverReachedPlugin", reached == 0, format!("{reached} read_file calls reached the plugin"));
    let names: Vec<String> = env.as_ref().ok().and_then(|(_, t)| serde_json::from_str(t).ok()).unwrap_or_default();
    let leaked: Vec<&String> =
        names.iter().filter(|n| n.starts_with("CARGO") || n.starts_with("RUST") || n.starts_with("ACCEPTANCE")).collect();
    c.check(
        "environmentFiltered",
        !names.is_empty() && leaked.is_empty(),
        format!("{} variables; none of the test process's own: {leaked:?}", names.len()),
    );
    let mut scanned = vec![outputs.to_string()];
    for e in std::fs::read_dir(plugins::dir(&f.home)).unwrap().flatten().filter(|e| e.path().is_file()) {
        scanned.push(std::fs::read_to_string(e.path()).unwrap_or_default());
    }
    let (files, net) = fermi_core::sandbox::os_enforced();
    let direct_blocked = direct.as_ref().is_ok_and(|(ok, t)| !ok && !t.contains(canary));
    if files && net {
        c.check("directBlocked", direct_blocked, format!("{direct:?}"));
        c.check(
            "canaryAbsent",
            scanned.iter().all(|s| !s.contains(canary)),
            "no sentinel bytes in outputs, the plugin log or fermi's plugin files",
        );
        c.finish();
    } else {
        // Without an OS sandbox the plugin's own read succeeds: say so, don't pass.
        c.check(
            "hookCanaryAbsent",
            [&hook, &traversal, &denied].iter().all(|r| !format!("{r:?}").contains(canary)),
            "the hook refusals carry no sentinel bytes",
        );
        c.block("the OS file sandbox for plugins needs Linux (Landlock); the argument scope, deny_read and environment checks were verified on this platform");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pl_08_api_incompatibility() {
    let mut c = Case::start("PL-08", "T0 T1");
    c.real(&["plugin manager (manifest and API version checks)", "registry"]);
    c.mocked(&["the plugin program (fake-plugin; it must never start)"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let marker = c.work.join("future-starts.txt");
    let src = plugin_fixture("future", &c.work.join("src/future"), &[("MARKER", &marker)]).unwrap();
    let r = plugins::install(&f.engine, &src).await;
    let msg = r.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default();
    c.text("error.txt", &msg);
    c.check("rejected", r.is_err(), "installation refused");
    c.check("explains", msg.contains("needs plugin API 2") && msg.contains("supports plugin API 1") && msg.contains("future"), msg.clone());
    let installed = plugins::installed(&f.engine.home);
    let starts = std::fs::read_to_string(&marker).unwrap_or_default().lines().count();
    c.json("state.json", &json!({ "installed": installed, "registry": snapshot(&f), "starts": starts }));
    c.check(
        "noSideEffects",
        installed.is_empty()
            && plugins::registry(&f.engine).entries.is_empty()
            && !plugins::dir(&f.home).join("future").exists()
            && starts == 0,
        format!(
            "{} installed, {} registered, folder copied: {}, program started {starts} times",
            installed.len(),
            plugins::registry(&f.engine).entries.len(),
            plugins::dir(&f.home).join("future").exists()
        ),
    );
    c.check(
        "logged",
        events(&f).iter().any(|e| e["event"] == "rejected" && e["reason"].as_str().is_some_and(|r| r.contains("API 2"))),
        "the rejection is in the plugin log",
    );
    let enable = plugins::enable(&f.engine, "future").await;
    c.check("notEnableable", enable.is_err(), format!("{:?}", enable.err().map(|e| e.to_string())));
    c.finish();
}
