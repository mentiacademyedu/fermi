//! System 6: browser and application control.

use acceptance_tests::{fermi_bridge, web_fixture, Case, Fermi};
use fermi_core::browser;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn project(c: &Case) -> PathBuf {
    let p = c.work.join("project");
    std::fs::create_dir_all(&p).unwrap();
    p
}

/// One browser tool through fermi's MCP bridge, as an agent calls it.
async fn tool(f: &Fermi, cwd: &Path, name: &str, args: Value) -> Result<Value, String> {
    let profile = f.engine.cfg.resolve_profile(Some("p"), cwd).map_err(|e| e.to_string())?;
    fermi_core::mcp::call_tool(&f.engine, &profile, cwd, name, &args)
        .await
        .map_err(|e| format!("{e:#}"))
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
}

/// Copies the session's trace and screenshots into the evidence.
fn evidence(c: &mut Case, f: &Fermi, cwd: &Path) -> Vec<Value> {
    let dir = browser::session_dir(&f.home, cwd);
    let trace = std::fs::read_to_string(dir.join("trace.jsonl")).unwrap_or_default();
    c.text("browser/trace.jsonl", &trace);
    for e in std::fs::read_dir(&dir).into_iter().flatten().filter_map(|e| e.ok()) {
        if e.path().extension().is_some_and(|x| x == "png") {
            let name = e.file_name().to_string_lossy().to_string();
            let _ = std::fs::copy(e.path(), c.dir.join("browser").join(&name));
            // Registered as an artifact through the manifest helper.
            c.text(&format!("browser/{name}.sha256"), &acceptance_tests::sha256(&std::fs::read(e.path()).unwrap_or_default()));
        }
    }
    trace.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Without a Chromium-based browser the case can't run here.
fn need_browser(c: Case, f: &Fermi) -> Option<Case> {
    if browser::find(&f.engine.cfg.browser).is_some() {
        return Some(c);
    }
    c.block("no Chrome, Edge or Chromium on this machine");
    None
}

fn actions<'a>(trace: &'a [Value], action: &str) -> Vec<&'a Value> {
    trace.iter().filter(|e| e["kind"] == "action" && e["action"] == action).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ba_01_local_site_launch() {
    let mut c = Case::start("BA-01", "T2");
    c.real(&[
        "fermi's browser tools on the MCP bridge",
        "a headless Chromium-based browser (DevTools protocol)",
        "the F-WEB site over HTTP",
    ]);
    c.mocked(&["the agent deciding what to call (the test calls the tools)"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let web = web_fixture().await;
    let cwd = project(&c);
    let opened = tool(&f, &cwd, "browser_open", json!({ "url": web.url("/") })).await;
    let shot = tool(&f, &cwd, "browser_screenshot", json!({ "name": "home" })).await;
    let missing = tool(&f, &cwd, "browser_open", json!({ "url": web.url("/missing") })).await;
    // A port nothing listens on (bound, then released).
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let refused = tool(&f, &cwd, "browser_open", json!({ "url": format!("http://127.0.0.1:{closed}/") })).await;
    let _ = tool(&f, &cwd, "browser_close", json!({})).await;
    let trace = evidence(&mut c, &f, &cwd);
    c.json("results.json", &json!({ "opened": opened, "screenshot": shot, "missing": missing, "refused": refused }));
    let o = opened.clone().unwrap_or_default();
    c.check(
        "openedExpectedPage",
        o["url"] == web.url("/") && o["page"] == "home" && o["title"] == "F-WEB Home" && o["status"] == 200,
        format!("{o}"),
    );
    c.check(
        "screenshot",
        shot.as_ref().is_ok_and(|s| Path::new(s["path"].as_str().unwrap_or("")).is_file()),
        format!("{:?}", shot.map(|s| s["path"].clone())),
    );
    c.check("httpErrorIsFailure", missing.as_ref().is_err_and(|e| e.contains("answered HTTP 404")), format!("{missing:?}"));
    c.check(
        "unreachableIsFailure",
        refused.as_ref().is_err_and(|e| e.contains("couldn't open") && e.contains("ERR_")),
        format!("{refused:?}"),
    );
    let opens = actions(&trace, "open");
    c.check(
        "traced",
        opens.len() == 3 && opens[0]["ok"] == true && opens[1]["ok"] == false && opens[2]["ok"] == false,
        format!("{} open actions in the trace: {:?}", opens.len(), opens.iter().map(|o| o["ok"].clone()).collect::<Vec<_>>()),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ba_02_form_submission() {
    let mut c = Case::start("BA-02", "T2");
    c.real(&["browser tools", "headless browser", "F-WEB form and its API"]);
    c.mocked(&["the agent"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let web = web_fixture().await;
    let cwd = project(&c);
    let steps = [
        ("browser_open", json!({ "url": web.url("/form") })),
        ("browser_fill", json!({ "selector": "#name", "value": "Ada Lovelace" })),
        ("browser_fill", json!({ "selector": "#email", "value": "ada@example.invalid" })),
        ("browser_click", json!({ "text": "Submit" })),
        ("browser_wait", json!({ "selector": "#result", "state": "visible" })),
        ("browser_read", json!({ "selector": "#result" })),
    ];
    let mut out = vec![];
    for (name, args) in steps {
        out.push(json!({ "tool": name, "result": tool(&f, &cwd, name, args).await }));
    }
    let _ = tool(&f, &cwd, "browser_screenshot", json!({ "name": "submitted" })).await;
    let _ = tool(&f, &cwd, "browser_close", json!({})).await;
    let trace = evidence(&mut c, &f, &cwd);
    let state = web.state.lock().unwrap();
    let posts: Vec<&Value> = state.requests.iter().filter(|r| r["method"] == "POST" && r["path"] == "/api/signup").collect();
    c.json("interactions.json", &json!(out));
    c.json("server-records.json", &json!({ "signups": state.signups, "signup_requests": posts }));
    let all_ok = out.iter().all(|o| o["result"].get("Ok").is_some());
    c.check("interactionsSucceeded", all_ok, format!("{} tool calls", out.len()));
    c.check(
        "storedOnce",
        state.signups.len() == 1
            && posts.len() == 1
            && state.signups[0]["name"] == "Ada Lovelace"
            && state.signups[0]["email"] == "ada@example.invalid",
        format!("{} records, {} POSTs: {}", state.signups.len(), posts.len(), json!(state.signups)),
    );
    let shown = out[5]["result"]["Ok"]["text"].as_str().unwrap_or_default();
    c.check("successReflectsRecord", shown == "Thanks, Ada Lovelace! Saved as record 1.", shown.to_string());
    c.check("traced", actions(&trace, "fill").len() == 2 && actions(&trace, "click").len() == 1, "two fills and one click in the trace");
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ba_03_navigation_chain() {
    let mut c = Case::start("BA-03", "T2");
    c.real(&["browser tools", "headless browser", "F-WEB pages"]);
    c.mocked(&["the agent"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let web = web_fixture().await;
    let cwd = project(&c);
    let start = tool(&f, &cwd, "browser_open", json!({ "url": web.url("/") })).await;
    let mut hops = vec![];
    for label in ["Start the tour", "Next page", "Next page"] {
        hops.push(tool(&f, &cwd, "browser_click", json!({ "text": label })).await);
    }
    let read = tool(&f, &cwd, "browser_read", json!({})).await;
    let state = tool(&f, &cwd, "browser_state", json!({})).await.unwrap_or_default();
    let _ = tool(&f, &cwd, "browser_screenshot", json!({ "name": "final" })).await;
    let _ = tool(&f, &cwd, "browser_close", json!({})).await;
    let trace = evidence(&mut c, &f, &cwd);
    c.json("hops.json", &json!({ "start": start, "hops": hops, "read": read, "state": state }));
    let pages: Vec<Value> = hops.iter().map(|h| h.as_ref().map(|v| v["page"].clone()).unwrap_or_default()).collect();
    c.check(
        "eachHopNavigated",
        hops.iter().all(|h| h.as_ref().is_ok_and(|v| v["navigated"] == true))
            && pages == [json!("tour-1"), json!("tour-2"), json!("tour-3")],
        format!("{pages:?}"),
    );
    let r = read.clone().unwrap_or_default();
    c.check(
        "finalRoute",
        r["url"] == web.url("/tour/3") && r["page"] == "tour-3" && r["text"].as_str().is_some_and(|t| t.contains("Tour complete")),
        format!("{} {}", r["url"], r["page"]),
    );
    let navs: Vec<String> =
        trace.iter().filter(|e| e["kind"] == "navigation").filter_map(|e| e["url"].as_str().map(str::to_string)).collect();
    let want: Vec<String> = ["/", "/tour/1", "/tour/2", "/tour/3"].iter().map(|p| web.url(p)).collect();
    c.check("routeEvents", navs.ends_with(&want), format!("{navs:?}"));
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ba_04_screenshot_artifact() {
    let mut c = Case::start("BA-04", "T2");
    c.real(&["browser tools", "headless browser", "PNG decoding of the saved file"]);
    c.mocked(&["the agent"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let web = web_fixture().await;
    let cwd = project(&c);
    let _ = tool(&f, &cwd, "browser_open", json!({ "url": web.url("/"), "width": 1280, "height": 800 })).await;
    let shot = tool(&f, &cwd, "browser_screenshot", json!({ "name": "home" })).await;
    let _ = tool(&f, &cwd, "browser_close", json!({})).await;
    let trace = evidence(&mut c, &f, &cwd);
    c.json("screenshot.json", &json!(shot));
    let s = shot.unwrap_or_default();
    let path = PathBuf::from(s["path"].as_str().unwrap_or_default());
    let bytes = std::fs::read(&path).unwrap_or_default();
    c.check("exists", !bytes.is_empty(), format!("{} ({} bytes)", path.display(), bytes.len()));
    // Reopen and decode the image.
    let decoded = (|| -> Result<(u32, u32, Vec<u8>, png::ColorType), String> {
        let mut r = png::Decoder::new(std::io::Cursor::new(&bytes)).read_info().map_err(|e| e.to_string())?;
        let mut buf = vec![0; r.output_buffer_size().ok_or("no size")?];
        let info = r.next_frame(&mut buf).map_err(|e| e.to_string())?;
        Ok((info.width, info.height, buf, info.color_type))
    })();
    let ok = decoded.as_ref().is_ok_and(|(w, h, ..)| *w == 1280 && *h == 800);
    c.check("decodes", ok, format!("{:?}", decoded.as_ref().map(|(w, h, _, t)| (w, h, t))));
    // The page state: the banner's colour at its middle, the body's white below.
    let pixel = |x: u32, y: u32| -> Option<(u8, u8, u8)> {
        let (w, _, buf, t) = decoded.as_ref().ok()?;
        let n = match t {
            png::ColorType::Rgba => 4,
            png::ColorType::Rgb => 3,
            _ => return None,
        };
        let i = ((y * w + x) as usize) * n;
        Some((buf[i], buf[i + 1], buf[i + 2]))
    };
    let close = |p: Option<(u8, u8, u8)>, want: (u8, u8, u8)| {
        p.is_some_and(|(r, g, b)| r.abs_diff(want.0) < 8 && g.abs_diff(want.1) < 8 && b.abs_diff(want.2) < 8)
    };
    let banner = pixel(1200, 20);
    let body = pixel(1200, 700);
    c.check(
        "expectedState",
        close(banner, (0x1d, 0x6f, 0xb8)) && close(body, (255, 255, 255)),
        format!("banner {banner:?}, body {body:?}"),
    );
    let t = actions(&trace, "screenshot");
    c.check(
        "traceReference",
        t.len() == 1 && t[0]["sha256"] == s["sha256"] && s["sha256"].as_str() == Some(&acceptance_tests::sha256(&bytes)),
        format!("trace sha {} = file sha", t.first().map(|x| x["sha256"].clone()).unwrap_or_default()),
    );
    c.finish();
}

/// A minimal MCP client for the bridge as its own process.
struct BridgeProc {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    out: std::io::BufReader<std::process::ChildStdout>,
    id: u64,
}

impl BridgeProc {
    fn start(home: &Path, cwd: &Path, task: &str) -> BridgeProc {
        let mut child = std::process::Command::new(fermi_bridge())
            .args(["--profile", "p"])
            .current_dir(cwd)
            .env("FERMI_HOME", home)
            .env("FERMI_TASK_ID", task)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut b = BridgeProc { child, stdin, out, id: 0 };
        b.call(
            "initialize",
            json!({ "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }),
        );
        let _ = writeln!(b.stdin, "{}", json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        b
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        self.id += 1;
        writeln!(self.stdin, "{}", json!({ "jsonrpc": "2.0", "id": self.id, "method": method, "params": params })).unwrap();
        loop {
            let mut line = String::new();
            if self.out.read_line(&mut line).unwrap_or(0) == 0 {
                return Value::Null;
            }
            let v: Value = serde_json::from_str(&line).unwrap_or_default();
            if v["id"] == self.id {
                return v["result"].clone();
            }
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.call("tools/call", json!({ "name": name, "arguments": args }));
        let text = r["content"][0]["text"].as_str().unwrap_or_default().to_string();
        json!({ "error": r["isError"] == true, "value": serde_json::from_str::<Value>(&text).unwrap_or(json!(text)) })
    }
}

impl Drop for BridgeProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ba_05_console_error_capture() {
    let mut c = Case::start("BA-05", "T2");
    c.real(&["fermi's MCP bridge as its own process (the agent's view)", "headless browser", "F-WEB error page"]);
    c.mocked(&["the agent (the test sends the MCP calls)"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let web = web_fixture().await;
    let cwd = project(&c);
    let (home, url) = (f.home.clone(), web.url("/error"));
    let cwd2 = cwd.clone();
    // The bridge is blocking I/O: run it off the async runtime (the web server stays responsive).
    let (listed, steps, state) = tokio::task::spawn_blocking(move || {
        let mut b = BridgeProc::start(&home, &cwd2, "4242");
        let listed: Vec<String> = b.call("tools/list", json!({}))["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        let open = b.tool("browser_open", json!({ "url": url }));
        let click = b.tool("browser_click", json!({ "text": "Break it" }));
        // Wait for the browser's event (polling what it reported), not a fixed sleep.
        let started = Instant::now();
        let mut state = Value::Null;
        while started.elapsed() < Duration::from_secs(10) {
            state = b.tool("browser_state", json!({}))["value"].clone();
            if state["console"].as_array().is_some_and(|c| !c.is_empty()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = b.tool("browser_close", json!({}));
        (listed, json!({ "open": open, "click": click }), state)
    })
    .await
    .unwrap();
    c.json("tool-calls.json", &steps);
    c.json("state.json", &state);
    c.check(
        "toolsOffered",
        listed.iter().any(|t| t == "browser_open") && listed.iter().any(|t| t == "browser_state"),
        format!("{} tools on the bridge", listed.len()),
    );
    let errors: Vec<Value> = state["console"].as_array().cloned().unwrap_or_default();
    let boom = errors.iter().find(|e| e["text"].as_str().is_some_and(|t| t.contains("fixture-boom-42"))).cloned().unwrap_or_default();
    c.check(
        "captured",
        boom["level"] == "exception" && boom["source"].as_str().is_some_and(|s| s.contains("/error")) && state["page"] == "error",
        format!("{boom}"),
    );
    // Linked to the task: the session's trace lives under the task's id.
    let trace_path = f.home.join("browser").join("4242").join("trace.jsonl");
    let trace = std::fs::read_to_string(&trace_path).unwrap_or_default();
    c.text("browser/trace.jsonl", &trace);
    c.check(
        "linkedToTask",
        trace.lines().any(|l| l.contains("fixture-boom-42") && l.contains("\"kind\":\"console\"")),
        format!("console error in {}", trace_path.display()),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ba_06_network_failure_detection() {
    let mut c = Case::start("BA-06", "T2");
    c.real(&["browser tools", "headless browser", "F-WEB API answering 500"]);
    c.mocked(&["the agent"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let web = web_fixture().await;
    let cwd = project(&c);
    let _ = tool(&f, &cwd, "browser_open", json!({ "url": web.url("/fail") })).await;
    let click = tool(&f, &cwd, "browser_click", json!({ "text": "Load data" })).await;
    let shown = tool(&f, &cwd, "browser_wait", json!({ "selector": "#status.bad", "state": "visible" })).await;
    let read = tool(&f, &cwd, "browser_read", json!({ "selector": "#status" })).await;
    let state = tool(&f, &cwd, "browser_state", json!({})).await.unwrap_or_default();
    let _ = tool(&f, &cwd, "browser_screenshot", json!({ "name": "error-state" })).await;
    let _ = tool(&f, &cwd, "browser_close", json!({})).await;
    let trace = evidence(&mut c, &f, &cwd);
    c.json("network.json", &json!({ "click": click, "shown": shown, "read": read, "state": state }));
    let failed = state["failed_requests"].as_array().cloned().unwrap_or_default();
    let api = failed.iter().find(|r| r["url"] == web.url("/api/fail")).cloned().unwrap_or_default();
    c.check("recorded", api["method"] == "GET" && api["status"] == 500, format!("{api}"));
    let text = read.as_ref().map(|r| r["text"].as_str().unwrap_or_default().to_string()).unwrap_or_default();
    c.check("noFalseSuccess", text == "Could not load data (500)." && !text.contains("loaded."), text.clone());
    c.check(
        "inTrace",
        trace.iter().any(|e| e["kind"] == "network" && e["status"] == 500 && e["url"] == web.url("/api/fail")),
        "the 500 is in the trace",
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ba_07_mobile_layout_assertions() {
    let mut c = Case::start("BA-07", "T2");
    c.real(&["browser tools", "headless browser with mobile emulation", "F-WEB responsive page"]);
    c.mocked(&["the agent"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let web = web_fixture().await;
    let cwd = project(&c);
    let opened = tool(&f, &cwd, "browser_open", json!({ "url": web.url("/mobile"), "width": 390, "height": 844, "mobile": true })).await;
    let layout = tool(&f, &cwd, "browser_state", json!({})).await.unwrap_or_default()["layout"].clone();
    let menu = tool(&f, &cwd, "browser_check", json!({ "selector": "#menu-button" })).await.unwrap_or_default();
    let link_before = tool(&f, &cwd, "browser_check", json!({ "selector": "#nav-home" })).await.unwrap_or_default();
    let _ = tool(&f, &cwd, "browser_screenshot", json!({ "name": "mobile-closed" })).await;
    let _ = tool(&f, &cwd, "browser_click", json!({ "text": "Menu" })).await;
    let link_after = tool(&f, &cwd, "browser_check", json!({ "selector": "#nav-home" })).await.unwrap_or_default();
    let _ = tool(&f, &cwd, "browser_screenshot", json!({ "name": "mobile-menu" })).await;
    let _ = tool(&f, &cwd, "browser_click", json!({ "text": "Menu" })).await;
    let fill = tool(&f, &cwd, "browser_fill", json!({ "selector": "#m-name", "value": "Ada" })).await;
    let send = tool(&f, &cwd, "browser_check", json!({ "selector": "#m-send" })).await.unwrap_or_default();
    let _ = tool(&f, &cwd, "browser_click", json!({ "text": "Send" })).await;
    let result = tool(&f, &cwd, "browser_read", json!({ "selector": "#m-result" })).await;
    let after = tool(&f, &cwd, "browser_state", json!({})).await.unwrap_or_default()["layout"].clone();
    // On a desktop viewport the menu button is meant to be hidden.
    let desktop = tool(&f, &cwd, "browser_viewport", json!({ "width": 1280, "height": 800 })).await;
    let menu_desktop = tool(&f, &cwd, "browser_check", json!({ "selector": "#menu-button" })).await.unwrap_or_default();
    let _ = tool(&f, &cwd, "browser_close", json!({})).await;
    let _ = evidence(&mut c, &f, &cwd);
    c.json(
        "layout.json",
        &json!({ "opened": opened, "viewport": layout, "menu": menu, "link_before": link_before, "link_after": link_after, "fill": fill, "send": send, "result": result, "after": after, "desktop": desktop, "menu_desktop": menu_desktop }),
    );
    c.check("viewport", layout["width"] == 390 && layout["height"] == 844, format!("{layout}"));
    c.check(
        "noHorizontalOverflow",
        layout["overflow_x"] == false && after["overflow_x"] == false,
        format!("scroll width {} in {} px", layout["scroll_width"], layout["width"]),
    );
    c.check(
        "menuBehaviour",
        menu["visible"] == true
            && menu["in_viewport"] == true
            && link_before["visible"] == false
            && link_after["visible"] == true
            && link_after["in_viewport"] == true,
        format!(
            "menu button visible {}, link hidden until opened ({} → {})",
            menu["visible"], link_before["visible"], link_after["visible"]
        ),
    );
    c.check(
        "formOperable",
        fill.is_ok() && send["visible"] == true && send["in_viewport"] == true && result.as_ref().is_ok_and(|r| r["text"] == "Sent: Ada"),
        format!("{:?}", result.map(|r| r["text"].clone())),
    );
    c.check(
        "desktopHidesMenuButton",
        menu_desktop["visible"] == false,
        format!("at 1280 px the menu button visible: {}", menu_desktop["visible"]),
    );
    c.finish();
}

#[tokio::test(flavor = "multi_thread")]
async fn ba_08_dynamic_readiness() {
    let mut c = Case::start("BA-08", "T2");
    c.real(&["browser tools (auto-waiting)", "headless browser", "F-WEB modal with a delayed response"]);
    c.mocked(&["the agent"]);
    let f = Fermi::new(&c, &[], "").unwrap();
    let Some(mut c) = need_browser(c, &f) else { return };
    let web = web_fixture().await;
    let cwd = project(&c);
    let _ = tool(&f, &cwd, "browser_open", json!({ "url": web.url("/modal") })).await;
    let open = tool(&f, &cwd, "browser_click", json!({ "text": "Open dialog" })).await;
    // Too early: Confirm is still disabled, so a short wait must fail, not click.
    let early = tool(&f, &cwd, "browser_click", json!({ "selector": "#confirm", "timeout_ms": 150 })).await;
    let confirms_after_early = web.state.lock().unwrap().confirms.len();
    let fill = tool(&f, &cwd, "browser_fill", json!({ "selector": "#code", "value": "code-7" })).await;
    let confirm = tool(&f, &cwd, "browser_click", json!({ "selector": "#confirm" })).await;
    let done = tool(&f, &cwd, "browser_wait", json!({ "selector": "#outcome", "state": "visible" })).await;
    let outcome = tool(&f, &cwd, "browser_read", json!({ "selector": "#outcome" })).await;
    let _ = tool(&f, &cwd, "browser_close", json!({})).await;
    let trace = evidence(&mut c, &f, &cwd);
    let state = web.state.lock().unwrap();
    c.json("readiness.json", &json!({ "open": open, "early": early, "fill": fill, "confirm": confirm, "done": done, "outcome": outcome, "confirms": state.confirms }));
    c.check(
        "prematureRefused",
        early.as_ref().is_err_and(|e| e.contains("not ready")) && confirms_after_early == 0,
        format!("{early:?}; {confirms_after_early} confirmations"),
    );
    // The fill had to wait, and the confirmation went out only after the delayed data arrived.
    let waited = fill.as_ref().map(|f| f["waited_ms"].as_u64().unwrap_or(0)).unwrap_or(0);
    let order: Vec<String> =
        state.requests.iter().filter_map(|r| r["path"].as_str().filter(|p| p.starts_with("/api/")).map(str::to_string)).collect();
    c.check(
        "waitedForReadiness",
        fill.is_ok() && waited > 0 && order == ["/api/slow", "/api/confirm"],
        format!("the fill waited {waited} ms for the field; API requests in order: {order:?}"),
    );
    let wait = actions(&trace, "wait").into_iter().find(|w| w["target"] == "`#code`").cloned().unwrap_or_default();
    let changes: Vec<Value> = wait["changes"].as_array().cloned().unwrap_or_default();
    let disabled_then_enabled =
        changes.first().is_some_and(|c| c["state"]["enabled"] == false) && changes.last().is_some_and(|c| c["state"]["enabled"] == true);
    c.check("timedStates", disabled_then_enabled, format!("{} state changes: {}", changes.len(), json!(changes)));
    c.check(
        "exactlyOnce",
        confirm.is_ok()
            && state.confirms.len() == 1
            && state.confirms[0]["code"] == "code-7"
            && outcome.as_ref().is_ok_and(|o| o["text"] == "Confirmed code-7 as record 1."),
        format!("{} confirmations: {}", state.confirms.len(), json!(state.confirms)),
    );
    c.finish();
}
