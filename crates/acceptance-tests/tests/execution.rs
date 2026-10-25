//! System 11: local and remote execution.

use acceptance_tests::{account, fixture, git, repo_root, wait_task, Case, Fermi};
use fermi_core::events::Event;
use fermi_core::hub::TaskStatus;
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread")]
async fn lr_01_local_execution() {
    let mut c = Case::start("LR-01", "T1");
    c.real(&["fermi hub (task API)", "engine", "local process", "cargo test on the fixture"]);
    c.mocked(&["model output (fake-cli applies the scripted edit)"]);
    let expected = std::fs::read_to_string(repo_root().join("tests/fixtures/F-CODE/expected/engine-lib-greet.rs")).unwrap();
    let step = json!({ "text": "Added greet.", "tools": [{ "name": "Edit", "input": { "file_path": "engine/src/lib.rs" } }], "edits": [{ "path": "engine/src/lib.rs", "content": expected }] });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step])], "").unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let hub = f.hub();
    let id = hub.submit(f.task("Add a greet(name) function to the engine crate, with a test.", &fx.path));
    let rec = wait_task(&hub, id, Duration::from_secs(60)).await.unwrap();
    let patch = git(&fx.path, &["diff"]).unwrap();
    let test = std::process::Command::new("cargo")
        .args(["test", "--quiet", "--offline"])
        .current_dir(&fx.path)
        .env("CARGO_TARGET_DIR", c.work.join("target"))
        .output()
        .unwrap();
    c.text("task.patch", &patch);
    c.text("fixture-tests.txt", &format!("{}{}", String::from_utf8_lossy(&test.stdout), String::from_utf8_lossy(&test.stderr)));
    c.json("task-trace.json", &serde_json::to_value(&rec).unwrap());
    c.check("terminalState", rec.status == TaskStatus::Done, format!("{:?}", rec.status));
    c.check(
        "expectedPatch",
        std::fs::read_to_string(fx.path.join("engine/src/lib.rs")).unwrap() == expected
            && git(&fx.path, &["diff", "--name-only"]).unwrap() == "engine/src/lib.rs",
        "only engine/src/lib.rs changed, as expected",
    );
    c.check("fixtureTestsPass", test.status.success(), "cargo test on the fixture");
    let attributable = rec.events.iter().any(|e| matches!(e, Event::Routed { account, .. } if account == "cl"))
        && rec.events.iter().any(|e| matches!(e, Event::Tool { detail, .. } if detail.contains("engine/src/lib.rs")));
    c.check("attributable", attributable, "the trace names the account and the edited file");
    c.finish();
}

// ── workers ─────────────────────────────────────────────────────────────────

use acceptance_tests::workers::{
    self, docker, docker_worker, jobs, proxy, start_worker, until, workers_toml, DROP_ANSWERS, PASS, REFUSE, TOKEN,
};
use fermi_core::hub::Hub;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Instant;

const T: Duration = Duration::from_secs(60);

fn greet() -> String {
    std::fs::read_to_string(repo_root().join("tests/fixtures/F-CODE/expected/engine-lib-greet.rs")).unwrap()
}

/// The job: add greet() (the same scripted edit LR-01 makes locally).
fn greet_step() -> Value {
    json!({ "text": "Added greet.", "tools": [{ "name": "Edit", "input": { "file_path": "engine/src/lib.rs" } }],
        "edits": [{ "path": "engine/src/lib.rs", "content": greet() }] })
}

/// The same edit, but the run keeps going until `go` appears (a long job).
fn held_step(release: &str) -> Value {
    json!({ "calls": [{ "id": "msg_1", "tools": [{ "id": "t_edit", "name": "Edit", "input": { "file_path": "engine/src/lib.rs" }, "output": "edited" }] }],
        "edits": [{ "path": "engine/src/lib.rs", "content": greet() }], "hold": release, "text": "Finished on the worker." })
}

fn remote_task(f: &Fermi, prompt: &str, cwd: &Path, worker: Option<&str>) -> fermi_core::engine::Task {
    let mut t = f.task(prompt, cwd);
    t.worker = worker.map(str::to_string);
    t
}

fn ledger(f: &Fermi, id: u64) -> Vec<Value> {
    std::fs::read_to_string(fermi_core::remote::dir(&f.home, id).join("ledger.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn count(l: &[Value], event: &str) -> usize {
    l.iter().filter(|e| e["event"] == event).count()
}

fn cargo_test(dir: &Path, target: &Path) -> (bool, String) {
    let o = std::process::Command::new("cargo")
        .args(["test", "--quiet", "--offline"])
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target)
        .output()
        .unwrap();
    (o.status.success(), format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)))
}

/// The remote run of LR-02 and LR-03: greet() on a worker in a container.
async fn container_run(c: &mut Case, label: &str) -> Option<(workers::DockerWorker, Fermi, PathBuf, fermi_core::hub::TaskRec)> {
    let w = match docker_worker(&c.work, label, vec![greet_step()]) {
        Ok(w) => w,
        Err(e) => {
            c.check("containerStarted", false, e);
            return None;
        }
    };
    let f = Fermi::at(c, "controller", &[], &workers_toml(&[(label, &w.url)], 30, 0)).unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo-container")).unwrap();
    let hub = f.hub();
    let id = hub.submit(remote_task(&f, "Add a greet(name) function to the engine crate, with a test.", &fx.path, Some(label)));
    let rec = wait_task(&hub, id, T).await.unwrap();
    Some((w, f, fx.path, rec))
}

#[tokio::test(flavor = "multi_thread")]
async fn lr_02_container_equivalence() {
    let mut c = Case::start("LR-02", "T1 T3");
    c.real(&[
        "fermi worker in a Docker container (ubuntu:24.04)",
        "the controller (hub, bundle, dispatch, artifacts, patch)",
        "git and cargo test",
    ]);
    c.mocked(&["model output (fake-cli, inside the container too)"]);
    if !docker() {
        return c.block("no Docker able to run Linux containers on this machine (the container cases run on Linux CI)");
    }
    // Local, as LR-01 does it.
    let local = Fermi::at(&c, "local", &[account("cl", "claude", vec![greet_step()])], "").unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo-local")).unwrap();
    let hub = local.hub();
    let lid = hub.submit(local.task("Add a greet(name) function to the engine crate, with a test.", &fx.path));
    let lrec = wait_task(&hub, lid, T).await.unwrap();
    let Some((w, _f, cpath, crec)) = container_run(&mut c, "box").await else { return c.finish() };
    let norm = |p: &Path| git(p, &["diff"]).unwrap().replace("\r\n", "\n");
    let (lp, cp) = (norm(&fx.path), norm(&cpath));
    c.text("local.patch", &lp);
    c.text("container.patch", &cp);
    let (lt, lout) = cargo_test(&fx.path, &c.work.join("target-local"));
    let (ct, cout) = cargo_test(&cpath, &c.work.join("target-container"));
    c.text("local-tests.txt", &lout);
    c.text("container-tests.txt", &cout);
    c.json("image.json", &json!({ "image": w.image, "digest": w.digest }));
    c.check(
        "bothAccepted",
        lrec.status == TaskStatus::Done && crec.status == TaskStatus::Done,
        format!("local {:?}, container {:?}", lrec.status, crec.status),
    );
    c.check("samePatch", !lp.is_empty() && lp == cp, "the same change, byte for byte (line endings normalized)");
    c.check("fixtureAssertionsAgree", lt && ct, format!("cargo test: local {lt}, container result {ct}"));
    c.check("imageRecorded", w.digest.starts_with("sha256:"), w.digest.clone());
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn lr_03_remote_dispatch() {
    let mut c = Case::start("LR-03", "T4");
    c.real(&[
        "fermi worker in a Docker container, reached over the network with its token",
        "dispatch, artifact download and checksums",
        "cargo test",
    ]);
    c.mocked(&["model output (fake-cli inside the container); no provider is called"]);
    if !docker() {
        return c.block("no Docker able to run Linux containers on this machine (the remote worker runs in one on Linux CI)");
    }
    let Some((w, f, cpath, rec)) = container_run(&mut c, "remote").await else { return c.finish() };
    let l = ledger(&f, rec.id);
    c.json("ledger.json", &json!(l));
    let d = l.iter().find(|e| e["event"] == "dispatched").cloned().unwrap_or_default();
    c.check("dispatchAcknowledged", d["worker"] == "remote" && d["runtime"]["os"] == "linux" && d["worker_protocol"] == 1, format!("{d}"));
    c.check(
        "executedThere",
        rec.status == TaskStatus::Done && rec.remote.as_ref().is_some_and(|r| r.worker == "remote"),
        format!("{:?} via {:?}", rec.status, rec.remote),
    );
    let job = rec.remote.as_ref().map(|r| r.job.clone()).unwrap_or_default();
    let dir = fermi_core::remote::dir(&f.home, rec.id).join(&job);
    let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap_or_default()).unwrap_or_default();
    c.json("manifest.json", &manifest);
    let sums_ok = manifest["files"].as_array().is_some_and(|fs| {
        !fs.is_empty()
            && fs.iter().all(|x| {
                std::fs::read(dir.join(x["path"].as_str().unwrap_or("-")))
                    .is_ok_and(|b| acceptance_tests::sha256(&b) == x["sha256"].as_str().unwrap_or(""))
            })
    });
    c.check("checksumsMatch", sums_ok, format!("{} artifacts", manifest["files"].as_array().map_or(0, |a| a.len())));
    let (ok, out) = cargo_test(&cpath, &c.work.join("target"));
    c.text("fixture-tests.txt", &out);
    c.check(
        "acceptedResult",
        std::fs::read_to_string(cpath.join("engine/src/lib.rs")).unwrap_or_default() == greet() && ok,
        "the fixture's expected change, and its tests pass",
    );
    drop(w);
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn lr_04_desktop_independent_jobs() {
    let mut c = Case::start("LR-04", "T2 T4");
    c.real(&[
        "fermi worker as its own process (the execution service)",
        "two hub instances (the window closed, then reopened)",
        "event log, artifacts and patch",
    ]);
    c.mocked(&["model output (fake-cli on the worker)"]);
    let wh = Fermi::at(&c, "worker-home", &[account("c1", "claude", vec![held_step("go")])], "").unwrap();
    let w = start_worker(&wh.home, "svc", None);
    let f = Fermi::at(&c, "controller", &[], &workers_toml(&[("svc", &w.url)], 30, 0)).unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let hub1 = f.hub();
    let id = hub1.submit(remote_task(&f, "Add greet, then wait for the go-ahead.", &fx.path, Some("svc")));
    // A durable checkpoint: the edit's events are in the saved history.
    let file = f.home.join("history").join(format!("{id}.json"));
    let cp = until(T, || std::fs::read_to_string(&file).is_ok_and(|t| t.contains("t_edit") && t.contains("\"remote\""))).await;
    let checkpoint: Value = serde_json::from_slice(&std::fs::read(&file).unwrap_or_default()).unwrap_or_default();
    c.json("checkpoint.json", &checkpoint);
    c.check("checkpoint", cp, format!("remote {}", checkpoint["remote"]));
    // Close the window.
    hub1.shutdown();
    drop(hub1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let job = checkpoint["remote"]["job"].as_str().unwrap_or_default().to_string();
    let running = jobs(&w.dir).iter().any(|j| j["id"] == job.as_str() && j["status"] == "running");
    c.check("notCancelled", running, format!("job {job} still running on the worker after the window closed"));
    // The job finishes with no window open.
    wh.release("c1", "go");
    let finished = until(T, || jobs(&w.dir).iter().any(|j| j["id"] == job.as_str() && j["status"] == "done")).await;
    c.check("continuedIndependently", finished, "the worker finished it alone");
    // Reopen: the task picks up the job's state and its result.
    let hub2 = Hub::new(f.engine.clone(), "reopened");
    let rec = wait_task(&hub2, id, T).await.unwrap();
    acceptance_tests::wait_saved(&f.home, id, T).await;
    c.json("reconnected-task.json", &serde_json::to_value(&rec).unwrap());
    let texts: Vec<String> =
        rec.events.iter().filter_map(|e| if let Event::Text { text } = e { Some(text.clone()) } else { None }).collect();
    let tools = rec.events.iter().filter(|e| matches!(e, Event::Tool { .. })).count();
    c.check(
        "stateAfterReconnect",
        rec.status == TaskStatus::Done && texts.iter().any(|t| t == "Finished on the worker.") && tools == 1,
        format!("{:?}; texts {texts:?}; {tools} tool event", rec.status),
    );
    let l = ledger(&f, id);
    c.json("ledger.json", &json!(l));
    c.check(
        "artifactsVisible",
        std::fs::read_to_string(fx.path.join("engine/src/lib.rs")).unwrap_or_default() == greet()
            && count(&l, "applied") == 1
            && count(&l, "dispatched") == 1,
        "the worker's change is applied here once; one dispatch",
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn lr_05_connection_recovery() {
    let mut c = Case::start("LR-05", "T1 T4");
    c.real(&["fermi worker process", "the controller's event following", "a TCP proxy that cuts the connection"]);
    c.mocked(&["model output (fake-cli on the worker)"]);
    let wh = Fermi::at(&c, "worker-home", &[account("c1", "claude", vec![held_step("go")])], "").unwrap();
    let w = start_worker(&wh.home, "svc", None);
    let p = proxy(&w.url).await;
    let f = Fermi::at(&c, "controller", &[], &workers_toml(&[("svc", &p.url)], 20, 0)).unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let hub = f.hub();
    let id = hub.submit(remote_task(&f, "Add greet, then wait.", &fx.path, Some("svc")));
    let seq_at = |hub: &Hub| hub.get(id).and_then(|t| t.remote.map(|r| r.seq)).unwrap_or(0);
    c.check("checkpoint", until(T, || seq_at(&hub) >= 3).await, format!("followed up to event {}", seq_at(&hub)));
    let before = seq_at(&hub);
    // Cut only the client's connection; the job goes on and ends meanwhile.
    p.set(REFUSE);
    let cut = Instant::now();
    wh.release("c1", "go");
    let job_done = until(T, || jobs(&w.dir).iter().any(|j| j["status"] == "done")).await;
    let during = seq_at(&hub);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    p.set(PASS);
    let rec = wait_task(&hub, id, T).await.unwrap();
    acceptance_tests::wait_saved(&f.home, id, T).await;
    let worker_seq = jobs(&w.dir).first().and_then(|j| j["seq"].as_u64()).unwrap_or(0);
    c.json(
        "timeline.json",
        &json!({ "followed_before_cut": before, "job_done_during_outage": job_done, "followed_during_outage": during, "outage_ms": cut.elapsed().as_millis() as u64,
            "worker_seq": worker_seq, "final_remote": rec.remote, "submits": p.submits.load(std::sync::atomic::Ordering::SeqCst) }),
    );
    c.json("restored-task.json", &serde_json::to_value(&rec).unwrap());
    c.check(
        "progressedDuringOutage",
        job_done && during == before,
        format!("the job finished on the worker while the client saw nothing new (stuck at {during})"),
    );
    c.check(
        "stateRestored",
        rec.status == TaskStatus::Done && rec.remote.as_ref().is_some_and(|r| r.seq == worker_seq),
        format!("{:?}; caught up to event {:?} of {worker_seq}", rec.status, rec.remote.as_ref().map(|r| r.seq)),
    );
    let tools = rec.events.iter().filter(|e| matches!(e, Event::Tool { .. })).count();
    let texts = rec.events.iter().filter(|e| matches!(e, Event::Text { .. })).count();
    let submits = p.submits.load(std::sync::atomic::Ordering::SeqCst);
    c.check(
        "noDuplicates",
        tools == 1 && texts == 1 && submits == 1 && jobs(&w.dir).len() == 1,
        format!("{tools} tool, {texts} text events; {submits} submission; {} job", jobs(&w.dir).len()),
    );
    let l = ledger(&f, id);
    c.check(
        "artifactReferences",
        count(&l, "applied") == 1 && std::fs::read_to_string(fx.path.join("engine/src/lib.rs")).unwrap_or_default() == greet(),
        "the result arrived after reconnecting and was applied once",
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn lr_06_remote_worker_death() {
    let mut c = Case::start("LR-06", "T3 T4");
    c.real(&["two fermi worker processes, one killed mid-job", "heartbeat timeout and retry policy", "ledger"]);
    c.mocked(&["model output (fake-cli on the workers)"]);
    let h1 = Fermi::at(&c, "worker-1", &[account("c1", "claude", vec![held_step("never")])], "").unwrap();
    let h2 = Fermi::at(&c, "worker-2", &[account("c1", "claude", vec![greet_step()])], "").unwrap();
    let mut w1 = start_worker(&h1.home, "w1", None);
    let w2 = start_worker(&h2.home, "w2", None);
    let extra = format!("workers = [\"w1\", \"w2\"]\n{}", workers_toml(&[("w1", &w1.url), ("w2", &w2.url)], 3, 1));
    let f = Fermi::at(&c, "controller", &[], &extra).unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let hub = f.hub();
    let id = hub.submit(remote_task(&f, "Add greet.", &fx.path, None));
    c.check("running", until(T, || hub.get(id).is_some_and(|t| t.remote.as_ref().is_some_and(|r| r.seq >= 3))).await, "the job runs on w1");
    w1.kill();
    let killed_at = fermi_core::util::now_ms();
    let rec = wait_task(&hub, id, T).await.unwrap();
    acceptance_tests::wait_saved(&f.home, id, T).await;
    let l = ledger(&f, id);
    c.json("ledger.json", &json!(l));
    c.json("task.json", &serde_json::to_value(&rec).unwrap());
    let lost_at = l.iter().find(|e| e["event"] == "lost").and_then(|e| e["at"].as_u64()).unwrap_or(0);
    let lost = l.iter().find(|e| e["event"] == "lost").cloned().unwrap_or_default();
    let detect_ms = lost_at.saturating_sub(killed_at);
    c.check(
        "detectedWithinPolicy",
        lost["worker"] == "w1" && detect_ms <= 3000 + 4000,
        format!("lost after {detect_ms} ms (heartbeat 3 s): {}", lost["reason"]),
    );
    c.check(
        "retriedOnce",
        count(&l, "dispatched") == 2
            && l.iter().filter(|e| e["event"] == "dispatched").map(|e| e["worker"].clone()).collect::<Vec<_>>()
                == [json!("w1"), json!("w2")],
        "dispatched on w1, then once on w2",
    );
    c.check(
        "sideEffectsOnce",
        rec.status == TaskStatus::Done
            && count(&l, "applied") == 1
            && l.iter().filter(|e| e["event"] == "finished").all(|e| e["worker"] == "w2"),
        format!("{:?}; applied {} time(s), only from w2", rec.status, count(&l, "applied")),
    );
    // The lost job can't pass for finished: back up, its worker marks it interrupted.
    let w1b = start_worker(&h1.home, "w1", None);
    let interrupted = jobs(&w1b.dir).iter().all(|j| j["status"] == "interrupted");
    c.check(
        "unresolvedNotComplete",
        interrupted && !jobs(&w1b.dir).is_empty(),
        format!("{:?}", jobs(&w1b.dir).iter().map(|j| j["status"].clone()).collect::<Vec<_>>()),
    );
    drop(w1b);
    // Bounded: with every worker gone, the task fails after retries + 1 attempts.
    let h3 = Fermi::at(&c, "worker-3", &[account("c1", "claude", vec![held_step("never")])], "").unwrap();
    let mut w3 = start_worker(&h3.home, "w3", None);
    let gone = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let extra =
        format!("workers = [\"w3\", \"gone\"]\n{}", workers_toml(&[("w3", &w3.url), ("gone", &format!("http://127.0.0.1:{gone}"))], 2, 1));
    let g = Fermi::at(&c, "controller-2", &[], &extra).unwrap();
    let fx2 = fixture("F-CODE", &c.work.join("repo-2")).unwrap();
    let hub2 = g.hub();
    let id2 = hub2.submit(remote_task(&g, "Add greet.", &fx2.path, None));
    let _ = until(T, || hub2.get(id2).is_some_and(|t| t.remote.is_some())).await;
    w3.kill();
    let rec2 = wait_task(&hub2, id2, T).await.unwrap();
    let summary = rec2
        .events
        .iter()
        .rev()
        .find_map(|e| if let Event::Done { summary, .. } = e { Some(summary.clone()) } else { None })
        .unwrap_or_default();
    c.check(
        "bounded",
        rec2.status == TaskStatus::Failed && summary.contains("2 tried") && git(&fx2.path, &["diff"]).unwrap().is_empty(),
        format!("{:?}: {summary}", rec2.status),
    );
    drop(w2);
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn lr_07_remote_cancellation() {
    let mut c = Case::start("LR-07", "T3 T4");
    c.real(&[
        "fermi worker process",
        "cancellation through the controller",
        "a proxy that loses the first acknowledgements",
        "process inventory",
    ]);
    c.mocked(&["model output (fake-cli on the worker; it starts a long-lived child)"]);
    let step = json!({ "calls": [{ "id": "msg_1", "tools": [{ "id": "t_build", "name": "Bash", "input": { "command": "make long-build" }, "output": "started" }] }],
        "child": "child.pid", "hold": "never" });
    let wh = Fermi::at(&c, "worker-home", &[account("c1", "claude", vec![step])], "").unwrap();
    let w = start_worker(&wh.home, "svc", None);
    let p = proxy(&w.url).await;
    let f = Fermi::at(&c, "controller", &[], &workers_toml(&[("svc", &p.url)], 30, 0)).unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let hub = f.hub();
    let id = hub.submit(remote_task(&f, "Run the long build.", &fx.path, Some("svc")));
    let pid_file = wh.account_dir("c1").join("child.pid");
    c.check(
        "started",
        until(T, || pid_file.exists() && hub.get(id).is_some_and(|t| t.remote.is_some())).await,
        "the job and its child process run",
    );
    let child: u32 = std::fs::read_to_string(&pid_file).unwrap_or_default().trim().parse().unwrap_or(0);
    let job = hub.get(id).and_then(|t| t.remote.map(|r| r.job)).unwrap_or_default();
    // The first answers get lost on the way back.
    p.set(DROP_ANSWERS);
    let cancelled = hub.cancel(id);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    p.set(PASS);
    let confirmed = until(Duration::from_secs(20), || count(&ledger(&f, id), "cancelled") == 1).await;
    let deadline = Duration::from_secs(10);
    let stopped = until(deadline, || !acceptance_tests::alive(child)).await;
    let work = w.dir.join("jobs").join(&job).join("work");
    let released = until(deadline, || !work.exists()).await;
    let state = jobs(&w.dir).into_iter().find(|j| j["id"] == job.as_str()).unwrap_or_default();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let rec = hub.get(id).unwrap();
    c.json(
        "cleanup.json",
        &json!({ "job": state, "child_pid": child, "child_alive": acceptance_tests::alive(child), "work_dir_exists": work.exists(),
        "cancel_requests": p.cancels.load(std::sync::atomic::Ordering::SeqCst), "ledger": ledger(&f, id) }),
    );
    c.check("cancelRequested", cancelled, "the controller cancelled the task");
    c.check("processesStopped", stopped, format!("child {child} gone within {} s", deadline.as_secs()));
    c.check("resourcesReleased", released, "the job's working folder was removed");
    c.check(
        "staysCancelled",
        state["status"] == "cancelled" && rec.status == TaskStatus::Cancelled,
        format!("worker {}, task {:?}", state["status"], rec.status),
    );
    let requests = p.cancels.load(std::sync::atomic::Ordering::SeqCst);
    c.check(
        "lostAckReconciled",
        confirmed && requests >= 2 && state["cancel"]["at"].is_u64(),
        format!("{requests} cancel requests (the first answers were lost); confirmed once in the ledger"),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn lr_08_runtime_compatibility() {
    let mut c = Case::start("LR-08", "T0 T1");
    c.real(&["three fermi worker processes speaking protocol 0, 1 and 2", "the handshake on both sides"]);
    c.mocked(&["model output (fake-cli on the compatible worker)"]);
    let mut results = vec![];
    let mut keep = vec![];
    for (name, version) in [("old", 0u32), ("ok", 1), ("new", 2)] {
        let wh = Fermi::at(&c, &format!("worker-{name}"), &[account("c1", "claude", vec![greet_step()])], "").unwrap();
        let w = start_worker(&wh.home, name, Some(version));
        let f = Fermi::at(&c, &format!("controller-{name}"), &[], &workers_toml(&[(name, &w.url)], 10, 0)).unwrap();
        let fx = fixture("F-CODE", &c.work.join(format!("repo-{name}"))).unwrap();
        let hub = f.hub();
        let id = hub.submit(remote_task(&f, "Add greet.", &fx.path, Some(name)));
        let rec = wait_task(&hub, id, T).await.unwrap();
        let summary = rec
            .events
            .iter()
            .rev()
            .find_map(|e| if let Event::Done { summary, .. } = e { Some(summary.clone()) } else { None })
            .unwrap_or_default();
        results.push(json!({ "worker": name, "protocol": version, "status": format!("{:?}", rec.status), "summary": summary, "jobs": jobs(&w.dir).len() }));
        keep.push((wh, w));
    }
    c.json("handshakes.json", &json!(results));
    for r in &results {
        let name = r["worker"].as_str().unwrap();
        if name == "ok" {
            c.check("compatibleAdmitted", r["status"] == "Done" && r["jobs"] == 1, format!("{r}"));
        } else {
            c.check(
                &format!("{name}Rejected"),
                r["status"] == "Failed"
                    && r["jobs"] == 0
                    && r["summary"]
                        .as_str()
                        .is_some_and(|s| s.contains(&format!("speaks protocol {}", r["protocol"])) && s.contains("supports protocol 1")),
                format!("{r}"),
            );
        }
    }
    // The worker refuses clients too: a newer client, and one without the token.
    let ok = &keep[1].1;
    let http = reqwest::Client::new();
    let newer = http.get(format!("{}/v1/hello", ok.url)).header("x-fermi-protocol", "2").bearer_auth(TOKEN).send().await.unwrap();
    let (ns, nb) = (newer.status().as_u16(), newer.json::<Value>().await.unwrap_or_default());
    let anon = http.get(format!("{}/v1/hello", ok.url)).header("x-fermi-protocol", "1").send().await.unwrap().status().as_u16();
    c.json("worker-side.json", &json!({ "newer_client": { "status": ns, "body": nb }, "no_token": anon }));
    c.check(
        "workerRejectsClient",
        ns == 426 && nb["error"].as_str().is_some_and(|e| e.contains("protocol")) && anon == 401,
        format!("newer client {ns}, no token {anon}"),
    );
    c.finish();
}
