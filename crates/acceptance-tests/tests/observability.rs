//! System 8: agent observability.

use acceptance_tests::{account, wait_task, Case, Fermi};
use fermi_core::hub::Hub;
use serde_json::json;
use std::time::Duration;

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
        tokio::time::sleep(Duration::from_millis(300)).await;
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
        before["events"] == after["events"],
        format!("{} events before and after", before["events"].as_array().map(|a| a.len()).unwrap_or(0)),
    );
    let tools = after["events"].as_array().into_iter().flatten().filter(|e| e["type"] == "tool").count();
    c.check("noDuplicates", tools == 2, format!("{tools} tool events (2 expected)"));
    c.finish();
}
