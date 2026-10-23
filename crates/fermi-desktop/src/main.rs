//! Fermi desktop app: a native window around the dashboard. The dashboard
//! server runs inside this process on a random local port, so the app and
//! `fermi ui` share one interface.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use fermi_core::hub::Hub;
use fermi_core::Engine;
use std::sync::Arc;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

fn main() {
    // Claude sessions launch this same executable as their MCP bridge
    // (`<exe> mcp --profile <name>`), so answer that before starting a GUI.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("mcp") {
        std::process::exit(if run_mcp(&args).is_ok() { 0 } else { 1 });
    }

    let app = tauri::Builder::default()
        .setup(|app| {
            let url = match start_dashboard() {
                Ok((url, hub)) => {
                    app.manage(hub);
                    WebviewUrl::External(url.parse()?)
                }
                Err(e) => WebviewUrl::App(format!("index.html#{}", percent_encode(&format!("{e:#}"))).into()),
            };
            WebviewWindowBuilder::new(app, "main", url).title("Fermi").inner_size(1280.0, 840.0).min_inner_size(860.0, 560.0).build()?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start Fermi");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            // Stop running tasks (and their CLI processes) with the window.
            if let Some(hub) = handle.try_state::<Arc<Hub>>() {
                hub.cancel_all();
            }
        }
    });
}

fn start_dashboard() -> anyhow::Result<(String, Arc<Hub>)> {
    fermi::setup::ensure_config()?;
    let engine = Engine::load()?;
    // Started from the Start menu there is no meaningful working directory.
    let cwd = dirs::home_dir().unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let server = tauri::async_runtime::block_on(fermi::server::start(engine, 0, cwd, "desktop"))?;
    let (url, hub) = (server.url.clone(), server.hub.clone());
    tauri::async_runtime::spawn(async move {
        let _ = server.serve(std::future::pending()).await;
    });
    Ok((url, hub))
}

fn run_mcp(args: &[String]) -> anyhow::Result<()> {
    let profile =
        args.windows(2).find(|w| w[0] == "--profile").map(|w| w[1].clone()).ok_or_else(|| anyhow::anyhow!("missing --profile"))?;
    tokio::runtime::Runtime::new()?.block_on(fermi_core::mcp::serve(Engine::load()?, profile))
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
