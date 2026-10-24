//! A deterministic stand-in for the `claude`, `codex` and `gemini` CLIs.
//!
//! fermi spawns it through `[bins]` exactly as it spawns the real CLIs. It
//! tells the three apart by their arguments, reads the prompt from stdin and
//! plays the next step of `scenario.json` in the account's config folder
//! (`CLAUDE_CONFIG_DIR`, `CODEX_HOME` or `GEMINI_CLI_HOME`). Every invocation
//! is appended to `calls.jsonl` there, as evidence of what fermi sent.
//!
//! A step (all fields optional):
//! `match` (choose this step when the prompt contains it), `text`, `tools`
//! ([{name, input}]), `usage` ({input, cached, output, cost}), `edits` ([{path,
//! content}] relative to the working folder), `denials` ([{tool, input}]),
//! `error` (a provider error message), `limit` (a usage-limit rejection),
//! `touch` / `wait_for` (barrier files, relative to the account folder),
//! `delay_ms`, `child` (spawn a long-lived child process and record its pid),
//! `crash` (exit with code 101 after partial output), `exit` (exit code),
//! `done_touch` (a file written with the time when the step finishes),
//! `read` (an absolute path the agent reads: its content joins the reply,
//! unless a `--disallowedTools "Read(//…)"` rule denies it, as Claude Code does).

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(PartialEq, Clone, Copy)]
enum Flavor {
    Claude,
    Codex,
    Gemini,
}

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

fn out(v: Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `fake-cli sleep <secs>`: the long-lived child some steps spawn.
    if args.first().map(String::as_str) == Some("sleep") {
        let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(60);
        std::thread::sleep(Duration::from_secs(secs));
        return;
    }
    let flavor = if args.first().map(String::as_str) == Some("exec") {
        Flavor::Codex
    } else if args.iter().any(|a| a == "--approval-mode") {
        Flavor::Gemini
    } else {
        Flavor::Claude
    };
    let home_var = match flavor {
        Flavor::Claude => "CLAUDE_CONFIG_DIR",
        Flavor::Codex => "CODEX_HOME",
        Flavor::Gemini => "GEMINI_CLI_HOME",
    };
    let Some(home) = std::env::var_os(home_var).map(PathBuf::from) else {
        eprintln!("fake-cli: {home_var} is not set; give the account its own home");
        std::process::exit(2);
    };
    let mut prompt = String::new();
    let _ = std::io::stdin().read_to_string(&mut prompt);

    let scenario: Value = std::fs::read(home.join("scenario.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(json!({}));
    let steps = scenario["steps"].as_array().cloned().unwrap_or_default();
    let log = home.join("calls.jsonl");
    // One call at a time picks its step and writes its record (a lock file guards the counter).
    let n = with_lock(&home, || {
        let n = std::fs::read_to_string(&log).map(|t| t.lines().count()).unwrap_or(0);
        let rec = json!({
            "n": n, "flavor": match flavor { Flavor::Claude => "claude", Flavor::Codex => "codex", Flavor::Gemini => "gemini" },
            "args": args, "prompt": prompt, "cwd": std::env::current_dir().ok(), "pid": std::process::id(), "started_ms": now_ms() as u64,
            "env": { "FERMI_DEPTH": std::env::var("FERMI_DEPTH").ok() },
        });
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&log).expect("calls.jsonl");
        let _ = writeln!(f, "{rec}");
        n
    });
    let step = steps
        .iter()
        .find(|s| s["match"].as_str().is_some_and(|m| prompt.contains(m)))
        .cloned()
        .or_else(|| {
            let unmatched: Vec<&Value> = steps.iter().filter(|s| s["match"].is_null()).collect();
            unmatched.get(n.min(unmatched.len().saturating_sub(1))).map(|s| (*s).clone())
        })
        .unwrap_or_else(|| json!({ "text": "ok" }));
    run(flavor, &home, &step, n);
}

fn with_lock<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let lock = dir.join("calls.lock");
    let start = Instant::now();
    while std::fs::OpenOptions::new().write(true).create_new(true).open(&lock).is_err() {
        if start.elapsed() > Duration::from_secs(10) {
            let _ = std::fs::remove_file(&lock); // a crashed holder
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let r = f();
    let _ = std::fs::remove_file(&lock);
    r
}

/// The `Read(//…)` patterns given with `--disallowedTools`.
fn read_rules() -> Vec<String> {
    let args: Vec<String> = std::env::args().collect();
    let Some(i) = args.iter().position(|a| a == "--disallowedTools") else { return vec![] };
    args[i + 1..]
        .iter()
        .take_while(|a| !a.starts_with("--"))
        .filter_map(|r| r.strip_prefix("Read(//")?.strip_suffix(')').map(|p| format!("/{p}")))
        .collect()
}

/// Claude's matching, as far as the tests need it: paths in POSIX form
/// (`C:\x` is `/c/x`); a trailing `/**` covers everything below.
fn rule_matches(rule: &str, path: &str) -> bool {
    let p = path.replace('\\', "/");
    let p = if p.len() > 2 && p.as_bytes()[1] == b':' { format!("/{}{}", p[..1].to_ascii_lowercase(), &p[2..]) } else { p };
    match rule.strip_suffix("/**") {
        Some(dir) => p.to_lowercase().starts_with(&format!("{}/", dir.to_lowercase())),
        None => p.eq_ignore_ascii_case(rule),
    }
}

fn run(flavor: Flavor, home: &Path, step: &Value, n: usize) {
    if let Some(t) = step["touch"].as_str() {
        let _ = std::fs::write(home.join(t), std::process::id().to_string());
    }
    if let Some(file) = step["child"].as_str() {
        let me = std::env::current_exe().expect("own path");
        let mut cmd = std::process::Command::new(me);
        cmd.args(["sleep", "120"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if let Ok(child) = cmd.spawn() {
            let _ = std::fs::write(home.join(file), child.id().to_string());
        }
    }
    if let Some(w) = step["wait_for"].as_str() {
        let start = Instant::now();
        while !home.join(w).exists() {
            if start.elapsed() > Duration::from_secs(120) {
                eprintln!("fake-cli: gave up waiting for {w}");
                std::process::exit(3);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    if let Some(ms) = step["delay_ms"].as_u64() {
        std::thread::sleep(Duration::from_millis(ms));
    }
    for e in step["edits"].as_array().into_iter().flatten() {
        if let (Some(p), Some(c)) = (e["path"].as_str(), e["content"].as_str()) {
            let path = Path::new(p);
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, c);
        }
    }
    let mut text = step["text"].as_str().unwrap_or("").to_string();
    let mut read_denied: Option<String> = None;
    if let Some(path) = step["read"].as_str() {
        if read_rules().iter().any(|r| rule_matches(r, path)) {
            read_denied = Some(path.to_string());
        } else {
            text += &std::fs::read_to_string(path).unwrap_or_default();
        }
    }
    let tools: Vec<Value> = step["tools"].as_array().cloned().unwrap_or_default();
    let usage = &step["usage"];
    let (input, output) = (usage["input"].as_u64().unwrap_or(10), usage["output"].as_u64().unwrap_or(5));
    // Input read from the prompt cache, on top of `input`.
    let cached = usage["cached"].as_u64().unwrap_or(0);
    let cost = usage["cost"].as_f64();
    let session = format!("fake-session-{n}");

    match flavor {
        Flavor::Claude => {
            out(json!({ "type": "system", "subtype": "init", "session_id": session }));
            for t in &tools {
                out(
                    json!({ "type": "assistant", "parent_tool_use_id": null, "message": { "content": [{ "type": "tool_use", "name": t["name"], "input": t["input"] }] } }),
                );
            }
            if step["crash"] == true {
                std::process::exit(101);
            }
            if step["limit"] == true {
                out(
                    json!({ "type": "rate_limit_event", "rate_limit_info": { "status": "rejected", "resetsAt": 4_102_444_800u64, "unifiedWindows": { "five_hour": { "utilization": 1.0 } } } }),
                );
                out(
                    json!({ "type": "result", "is_error": true, "result": "Claude AI usage limit reached", "usage": { "input_tokens": 0, "output_tokens": 0 } }),
                );
                std::process::exit(1);
            }
            if let Some(err) = step["error"].as_str() {
                out(
                    json!({ "type": "result", "subtype": "error_during_execution", "is_error": true, "result": err, "usage": { "input_tokens": input, "output_tokens": 0 } }),
                );
                std::process::exit(step["exit"].as_i64().unwrap_or(1) as i32);
            }
            if !text.is_empty() {
                out(
                    json!({ "type": "stream_event", "parent_tool_use_id": null, "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": text } } }),
                );
                out(
                    json!({ "type": "assistant", "parent_tool_use_id": null, "message": { "content": [{ "type": "text", "text": text }] } }),
                );
            }
            let mut denials: Vec<Value> = step["denials"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(i, d)| json!({ "tool_name": d["tool"], "tool_use_id": format!("toolu_{i}"), "tool_input": d["input"] }))
                .collect();
            if let Some(p) = &read_denied {
                denials.push(json!({ "tool_name": "Read", "tool_use_id": "toolu_read", "tool_input": { "file_path": p } }));
            }
            let mut result = json!({ "type": "result", "subtype": "success", "is_error": false, "result": text, "session_id": session,
                "usage": { "input_tokens": input, "cache_read_input_tokens": cached, "output_tokens": output }, "permission_denials": denials });
            if let Some(c) = cost {
                result["total_cost_usd"] = json!(c);
            }
            out(result);
        }
        Flavor::Codex => {
            out(json!({ "type": "thread.started", "thread_id": session }));
            out(json!({ "type": "turn.started" }));
            for t in &tools {
                out(
                    json!({ "type": "item.completed", "item": { "type": "command_execution", "command": t["input"]["command"].as_str().unwrap_or(t["name"].as_str().unwrap_or("tool")) } }),
                );
            }
            if step["crash"] == true {
                std::process::exit(101);
            }
            if step["limit"] == true || step["error"].is_string() {
                let msg = if step["limit"] == true { "You've hit your usage limit." } else { step["error"].as_str().unwrap_or("error") };
                out(json!({ "type": "turn.failed", "error": { "message": msg } }));
                std::process::exit(1);
            }
            if !text.is_empty() {
                out(json!({ "type": "item.completed", "item": { "type": "agent_message", "text": text } }));
            }
            out(
                json!({ "type": "turn.completed", "usage": { "input_tokens": input + cached, "cached_input_tokens": cached, "output_tokens": output } }),
            );
        }
        Flavor::Gemini => {
            out(json!({ "type": "init", "session_id": session }));
            for t in &tools {
                out(json!({ "type": "tool_use", "tool_name": t["name"], "parameters": t["input"] }));
            }
            if step["crash"] == true {
                std::process::exit(101);
            }
            if step["limit"] == true || step["error"].is_string() {
                let msg = if step["limit"] == true { "Resource exhausted: quota" } else { step["error"].as_str().unwrap_or("error") };
                out(
                    json!({ "type": "result", "status": "error", "error": { "message": msg }, "stats": { "input_tokens": 0, "output_tokens": 0 } }),
                );
                std::process::exit(1);
            }
            if !text.is_empty() {
                out(json!({ "type": "message", "role": "assistant", "content": text }));
            }
            out(json!({ "type": "result", "status": "success", "stats": { "input_tokens": input, "output_tokens": output } }));
        }
    }
    if let Some(d) = step["done_touch"].as_str() {
        let _ = std::fs::write(home.join(d), now_ms().to_string());
    }
    std::process::exit(step["exit"].as_i64().unwrap_or(0) as i32);
}
