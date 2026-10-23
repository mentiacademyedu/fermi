//! System 2: multi-agent orchestration. Real hub and team runs, real git
//! worktrees and processes; model output comes from the fake CLI.

use acceptance_tests::{account, alive, fixture, git, kill, tree_hash, wait_file, wait_task, Case, Fermi};
use fermi_core::hub::TaskStatus;
use fermi_core::team::{NewRun, RunStatus, TaskState, TeamRun, Teams};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

const T: Duration = Duration::from_secs(90);

/// The architect's reply: a plan in a JSON block.
fn planner(tasks: Value) -> Value {
    let plan = json!({ "summary": "Plan for the acceptance case.", "tasks": tasks });
    json!({ "match": "You are the architect", "text": format!("Here is the plan.\n```json\n{plan}\n```") })
}

fn new_run(repo: &Path, parallel: usize) -> NewRun {
    serde_json::from_value(json!({
        "goal": "Acceptance case goal", "repo": repo, "profile": "p", "parallel": parallel,
        "checks": { "tests": false, "security": false },
    }))
    .unwrap()
}

async fn wait_run(teams: &Teams, id: u64, done: &[RunStatus]) -> TeamRun {
    let start = Instant::now();
    loop {
        let r = teams.get(id).unwrap();
        if done.contains(&r.status) {
            return r;
        }
        assert!(start.elapsed() < T, "run stuck in {:?}: {:?}", r.status, r.log.last());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Plans (with the fake architect), approves and waits for a terminal state.
async fn team(f: &Fermi, repo: &Path, parallel: usize) -> (Arc<Teams>, u64) {
    let hub = f.hub();
    let teams = Teams::new(hub);
    let id = teams.create(new_run(repo, parallel)).unwrap();
    wait_run(&teams, id, &[RunStatus::Review, RunStatus::Attention]).await;
    teams.approve(id, None).unwrap();
    (teams, id)
}

fn ms(f: &Fermi, file: &str) -> u128 {
    std::fs::read_to_string(f.account_dir("cl").join(file)).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_01_concurrent_workers() {
    let mut c = Case::start("MA-01", "T1");
    c.real(&["fermi hub", "engine", "OS processes"]);
    c.mocked(&["model output (fake-cli)"]);
    let step = |n: &str| json!({ "match": format!("[[{n}]]"), "touch": format!("{n}.start"), "wait_for": "release", "done_touch": format!("{n}.done"), "text": format!("result {n}") });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step("A"), step("B"), step("C")])], "").unwrap();
    let hub = f.hub();
    let mut ids = vec![];
    for n in ["A", "B", "C"] {
        let ws = c.work.join(format!("ws-{n}"));
        std::fs::create_dir_all(&ws).unwrap();
        ids.push(hub.submit(f.task(&format!("Job [[{n}]]"), &ws)));
    }
    // Each worker holds at its checkpoint until all three have started.
    let all_started = async {
        for n in ["A", "B", "C"] {
            wait_file(&f.account_dir("cl").join(format!("{n}.start")), Duration::from_secs(30)).await?;
        }
        anyhow::Ok(())
    }
    .await;
    c.check("allThreeStartedBeforeAnyFinished", all_started.is_ok(), "three workers reached their checkpoint together");
    f.release("cl", "release");
    let mut outs = vec![];
    for id in &ids {
        outs.push(wait_task(&hub, *id, T).await.unwrap());
    }
    let calls = f.calls("cl");
    c.json(
        "worker-timeline.json",
        &json!({ "calls": calls, "done_ms": { "A": ms(&f, "A.done"), "B": ms(&f, "B.done"), "C": ms(&f, "C.done") } }),
    );
    let starts: Vec<u128> = calls.iter().map(|x| x["started_ms"].as_u64().unwrap() as u128).collect();
    let first_done = ["A", "B", "C"].iter().map(|n| ms(&f, &format!("{n}.done"))).min().unwrap();
    c.check("intervalsOverlap", starts.iter().all(|s| *s <= first_done), format!("starts {starts:?}, first finish {first_done}"));
    for (n, rec) in ["A", "B", "C"].iter().zip(&outs) {
        let text = rec.events.iter().rev().find_map(|e| if let fermi_core::Event::Text { text } = e { Some(text.clone()) } else { None });
        c.check(
            &format!("ownResult:{n}"),
            rec.status == TaskStatus::Done && text.as_deref() == Some(&format!("result {n}")),
            format!("{:?} {text:?}", rec.status),
        );
    }
    let cwds: Vec<String> = calls.iter().map(|x| x["cwd"].as_str().unwrap_or("").to_string()).collect();
    c.check("separateWorkspaces", cwds.iter().collect::<std::collections::BTreeSet<_>>().len() == 3, format!("{cwds:?}"));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_02_dependency_ordering() {
    let mut c = Case::start("MA-02", "T1");
    c.real(&["fermi team runs (scheduler)", "hub", "git worktrees"]);
    c.mocked(&["model output (fake-cli)"]);
    let plan = || {
        planner(json!([
            { "id": "a", "role": "developer", "title": "Step A", "description": "Do [[A]]." },
            { "id": "b", "role": "developer", "title": "Step B", "description": "Do [[B]].", "depends_on": ["a"] },
            { "id": "c", "role": "developer", "title": "Step C", "description": "Do [[C]].", "depends_on": ["b"] }
        ]))
    };
    let work = |n: &str| json!({ "match": format!("[[{n}]]"), "touch": format!("{n}.start"), "done_touch": format!("{n}.done"), "text": format!("{n} finished") });

    // 1. A, then B, then C.
    let f = Fermi::at(&c, "ok", &[account("cl", "claude", vec![plan(), work("A"), work("B"), work("C")])], "").unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo-ok")).unwrap();
    let (teams, id) = team(&f, &fx.path, 3).await;
    let run = wait_run(&teams, id, &[RunStatus::Ready, RunStatus::Attention]).await;
    let calls = f.calls("cl");
    let start_of = |n: &str| {
        calls
            .iter()
            .find(|x| x["prompt"].as_str().unwrap_or("").contains(&format!("[[{n}]]")))
            .and_then(|x| x["started_ms"].as_u64())
            .unwrap_or(0) as u128
    };
    c.json("dag-success.json", &json!({ "run": run, "calls": calls }));
    c.check(
        "bAfterA",
        start_of("B") >= ms(&f, "A.done") && ms(&f, "A.done") > 0,
        format!("B started {} after A finished {}", start_of("B"), ms(&f, "A.done")),
    );
    c.check(
        "cAfterB",
        start_of("C") >= ms(&f, "B.done") && ms(&f, "B.done") > 0,
        format!("C started {} after B finished {}", start_of("C"), ms(&f, "B.done")),
    );
    c.check("allDone", run.status == RunStatus::Ready, format!("{:?}", run.status));

    // 2. A fails: B and C must never run.
    let fail = json!({ "match": "[[A]]", "error": "the build failed" });
    let f2 = Fermi::at(&c, "fail", &[account("cl", "claude", vec![plan(), fail, work("B"), work("C")])], "").unwrap();
    let fx2 = fixture("F-CODE", &c.work.join("repo-fail")).unwrap();
    let (teams2, id2) = team(&f2, &fx2.path, 3).await;
    let run2 = wait_run(&teams2, id2, &[RunStatus::Attention, RunStatus::Ready]).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let calls2 = f2.calls("cl");
    let ran = |n: &str| calls2.iter().any(|x| x["prompt"].as_str().unwrap_or("").contains(&format!("[[{n}]]")));
    c.json("dag-failure.json", &json!({ "run": teams2.get(id2), "calls": calls2 }));
    let st = |r: &TeamRun, k: &str| r.tasks.iter().find(|t| t.key == k).map(|t| t.state);
    c.check("failureBlocksDescendants", ran("A") && !ran("B") && !ran("C"), "A ran; B and C never started");
    c.check(
        "failureVisible",
        run2.status == RunStatus::Attention && st(&run2, "a") == Some(TaskState::Failed),
        format!("{:?} a={:?}", run2.status, st(&run2, "a")),
    );

    // 3. Cancel while B waits on A.
    let slow = json!({ "match": "[[A]]", "touch": "A.start", "wait_for": "A.go", "text": "A finished late" });
    let f3 = Fermi::at(&c, "cancel", &[account("cl", "claude", vec![plan(), slow, work("B"), work("C")])], "").unwrap();
    let fx3 = fixture("F-CODE", &c.work.join("repo-cancel")).unwrap();
    let (teams3, id3) = team(&f3, &fx3.path, 3).await;
    wait_file(&f3.account_dir("cl").join("A.start"), T).await.unwrap();
    teams3.cancel(id3).unwrap();
    f3.release("cl", "A.go");
    tokio::time::sleep(Duration::from_secs(2)).await;
    let calls3 = f3.calls("cl");
    let run3 = teams3.get(id3).unwrap();
    c.json("dag-cancel.json", &json!({ "run": run3, "calls": calls3 }));
    let ran3 = |n: &str| calls3.iter().any(|x| x["prompt"].as_str().unwrap_or("").contains(&format!("[[{n}]]")));
    c.check("cancelBlocksDescendants", !ran3("B") && !ran3("C") && run3.status == RunStatus::Cancelled, format!("{:?}", run3.status));
    c.finish();
}

/// Two developers in parallel; returns the run, teams and fixture.
async fn two_devs(
    c: &Case,
    name: &str,
    a: Value,
    b: Value,
    debugger: Option<Value>,
) -> (Fermi, Arc<Teams>, u64, acceptance_tests::Fixture) {
    let plan = planner(json!([
        { "id": "one", "role": "developer", "title": "Change one", "description": "Do [[ONE]]." },
        { "id": "two", "role": "developer", "title": "Change two", "description": "Do [[TWO]]." }
    ]));
    let mut steps = vec![plan, a, b];
    steps.extend(debugger);
    let f = Fermi::at(c, name, &[account("cl", "claude", steps)], "").unwrap();
    let fx = fixture("F-CODE", &c.work.join(format!("repo-{name}"))).unwrap();
    let (teams, id) = team(&f, &fx.path, 2).await;
    (f, teams, id, fx)
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_03_independent_edits() {
    let mut c = Case::start("MA-03", "T1");
    c.real(&["fermi team runs", "git worktrees, merges and the final merge"]);
    c.mocked(&["model output (fake-cli applies scripted edits)"]);
    let num = "/// The number helper.\npub fn helper() -> u32 {\n    20\n}\n";
    let main =
        "use engine::describe;\nuse util::num::helper;\n\nfn main() {\n    println!(\"{} {}\", describe(\" Fermi \"), helper());\n}\n";
    let a = json!({ "match": "[[ONE]]", "edits": [{ "path": "util/src/num.rs", "content": num }], "text": "one done" });
    let b = json!({ "match": "[[TWO]]", "edits": [{ "path": "app/src/main.rs", "content": main }], "text": "two done" });
    let (_f, teams, id, fx) = two_devs(&c, "ind", a, b, None).await;
    let run = wait_run(&teams, id, &[RunStatus::Ready, RunStatus::Attention]).await;
    // The user's own uncommitted file must survive.
    std::fs::write(fx.path.join("USER_SENTINEL.txt"), "mine").unwrap();
    let merged = teams.merge(id);
    // Expected: the base plus exactly the two patches.
    let expect = c.work.join("expected");
    let fx_e = fixture("F-CODE", &expect).unwrap();
    std::fs::write(fx_e.path.join("util/src/num.rs"), num).unwrap();
    std::fs::write(fx_e.path.join("app/src/main.rs"), main).unwrap();
    std::fs::write(fx_e.path.join("USER_SENTINEL.txt"), "mine").unwrap();
    let (got, want) = (tree_hash(&fx.path), tree_hash(&fx_e.path));
    c.text("base-commit.txt", &fx.commit);
    c.text("integrated.diff", &git(&fx.path, &["diff", &fx.commit, "HEAD"]).unwrap_or_default());
    c.json("run.json", &serde_json::to_value(&run).unwrap());
    c.check(
        "integrated",
        run.status == RunStatus::Ready && merged.is_ok(),
        format!("{:?} merge {:?}", run.status, merged.as_ref().err().map(|e| e.to_string())),
    );
    c.check("bothEditsSurviveExactly", got == want, format!("tree {got} vs expected {want}"));
    c.check(
        "sentinelIntact",
        std::fs::read_to_string(fx.path.join("USER_SENTINEL.txt")).ok().as_deref() == Some("mine"),
        "user's file unchanged",
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_04_conflicting_edits() {
    let mut c = Case::start("MA-04", "T1");
    c.real(&["fermi team runs", "git merge"]);
    c.mocked(&["model output (fake-cli); the debugger is given no resolution"]);
    let base = std::fs::read_to_string(acceptance_tests::repo_root().join("tests/fixtures/F-CODE/repo/engine/src/lib.rs")).unwrap();
    let v1 = base.replace("(normalize(name), helper())", "(normalize(name), helper() + 1)");
    let v2 = base.replace("(normalize(name), helper())", "(normalize(name).to_uppercase(), helper())");
    let a = json!({ "match": "[[ONE]]", "edits": [{ "path": "engine/src/lib.rs", "content": v1 }], "text": "one" });
    let b = json!({ "match": "[[TWO]]", "edits": [{ "path": "engine/src/lib.rs", "content": v2 }], "text": "two" });
    let dbg = json!({ "match": "Resolve merge conflict", "text": "I can't decide which change is right." });
    let (_f, teams, id, fx) = two_devs(&c, "conf", a, b, Some(dbg)).await;
    let run = wait_run(&teams, id, &[RunStatus::Attention, RunStatus::Ready]).await;
    let branch = |k: &str| run.tasks.iter().find(|t| t.key == k).and_then(|t| t.branch.clone()).unwrap_or_default();
    let show = |b: &str| git(&fx.path, &["show", &format!("{b}:engine/src/lib.rs")]).unwrap_or_default();
    let (b1, b2) = (branch("one"), branch("two"));
    c.text("diff-one.patch", &git(&fx.path, &["diff", &fx.commit, &b1]).unwrap_or_default());
    c.text("diff-two.patch", &git(&fx.path, &["diff", &fx.commit, &b2]).unwrap_or_default());
    c.json("run.json", &serde_json::to_value(&run).unwrap());
    let log = run.log.iter().map(|l| l.1.clone()).collect::<Vec<_>>().join("\n");
    c.check("conflictDetected", log.contains("conflicts in") && log.contains("engine/src/lib.rs"), "the log names the conflicting file");
    c.check("escalated", run.status == RunStatus::Attention && run.error.is_some(), format!("{:?}: {:?}", run.status, run.error));
    c.check(
        "bothPatchesKept",
        show(&b1).contains("helper() + 1") && show(&b2).contains("to_uppercase()"),
        "each branch still has its own change",
    );
    let integ = run.integration.as_ref().map(|i| i.worktree.clone()).unwrap();
    let unmerged = git(&integ, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
    c.check(
        "noSilentOverwrite",
        unmerged.contains("engine/src/lib.rs"),
        format!("still unresolved in the integration worktree: {unmerged:?}"),
    );
    c.check("userBranchUntouched", git(&fx.path, &["rev-parse", "HEAD"]).unwrap() == fx.commit, "main is still at the base commit");
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_06_worker_crash_isolation() {
    let mut c = Case::start("MA-06", "T1");
    c.real(&["fermi hub", "OS processes (one killed)"]);
    c.mocked(&["model output (fake-cli)"]);
    let step = |n: &str| json!({ "match": format!("[[{n}]]"), "touch": format!("{n}.pid"), "wait_for": format!("{n}.go"), "text": format!("result {n}") });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![step("A"), step("B"), step("C")])], "").unwrap();
    let hub = f.hub();
    let ids: Vec<u64> = ["A", "B", "C"].iter().map(|n| hub.submit(f.task(&format!("Job [[{n}]]"), &c.work))).collect();
    for n in ["A", "B", "C"] {
        wait_file(&f.account_dir("cl").join(format!("{n}.pid")), T).await.unwrap();
    }
    let pid: u32 = std::fs::read_to_string(f.account_dir("cl").join("B.pid")).unwrap().trim().parse().unwrap();
    kill(pid);
    f.release("cl", "A.go");
    f.release("cl", "C.go");
    let recs: Vec<_> = futures_wait(&hub, &ids).await;
    c.json("results.json", &serde_json::to_value(&recs).unwrap());
    c.check("killedTaskFailed", recs[1].status == TaskStatus::Failed, format!("{:?}", recs[1].status));
    for (i, n) in [(0, "A"), (2, "C")] {
        let text =
            recs[i].events.iter().rev().find_map(|e| if let fermi_core::Event::Text { text } = e { Some(text.clone()) } else { None });
        c.check(
            &format!("survivor:{n}"),
            recs[i].status == TaskStatus::Done && text.as_deref() == Some(&format!("result {n}")),
            format!("{:?}", recs[i].status),
        );
    }
    c.check("killedProcessGone", !alive(pid), format!("pid {pid}"));
    c.finish();
}

async fn futures_wait(hub: &fermi_core::hub::Hub, ids: &[u64]) -> Vec<fermi_core::hub::TaskRec> {
    let mut out = vec![];
    for id in ids {
        out.push(wait_task(hub, *id, T).await.unwrap());
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn ma_07_parent_cancellation() {
    let mut c = Case::start("MA-07", "T1");
    c.real(&["fermi team runs", "hub cancellation", "OS processes and a grandchild process"]);
    c.mocked(&["model output (fake-cli)"]);
    let plan = planner(json!([
        { "id": "a", "role": "developer", "title": "Active", "description": "Do [[A]]." },
        { "id": "b", "role": "developer", "title": "Queued", "description": "Do [[B]].", "depends_on": ["a"] }
    ]));
    let active = json!({ "match": "[[A]]", "touch": "A.pid", "child": "A.child", "wait_for": "never", "text": "unreachable" });
    let queued = json!({ "match": "[[B]]", "text": "B ran" });
    let f = Fermi::new(&c, &[account("cl", "claude", vec![plan, active, queued])], "").unwrap();
    let fx = fixture("F-CODE", &c.work.join("repo")).unwrap();
    let (teams, id) = team(&f, &fx.path, 2).await;
    let dir = f.account_dir("cl");
    wait_file(&dir.join("A.child"), T).await.unwrap();
    let pid: u32 = std::fs::read_to_string(dir.join("A.pid")).unwrap().trim().parse().unwrap();
    let child: u32 = std::fs::read_to_string(dir.join("A.child")).unwrap().trim().parse().unwrap();
    let before = json!({ "cli": alive(pid), "grandchild": alive(child) });
    teams.cancel(id).unwrap();
    // Declared cancellation deadline: 5 seconds.
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && (alive(pid) || alive(child)) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let after = json!({ "cli": alive(pid), "grandchild": alive(child) });
    let run = teams.get(id).unwrap();
    c.json("process-inventory.json", &json!({ "pids": { "cli": pid, "grandchild": child }, "before": before, "after": after }));
    c.json("run.json", &serde_json::to_value(&run).unwrap());
    c.check(
        "queuedNeverStarts",
        !f.calls("cl").iter().any(|x| x["prompt"].as_str().unwrap_or("").contains("[[B]]")),
        "B was never started",
    );
    c.check("activeStopped", !alive(pid), format!("agent CLI pid {pid} alive: {}", alive(pid)));
    c.check("noOrphan", !alive(child), format!("grandchild pid {child} alive: {}", alive(child)));
    let a = run.tasks.iter().find(|t| t.key == "a").map(|t| t.state);
    c.check(
        "noFalseCompletion",
        run.status == RunStatus::Cancelled && a == Some(TaskState::Interrupted),
        format!("{:?}, a = {a:?}", run.status),
    );
    if alive(child) {
        kill(child); // clean up after a failure
    }
    c.finish();
}
