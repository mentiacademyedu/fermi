//! Workers for the execution cases (LR): `fermi worker` as a separate process
//! or in a Docker container, and a TCP proxy that can cut the connection to
//! one (refuse connections, or let requests through and lose the answers).

use serde_json::{json, Value};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const TOKEN: &str = "acceptance-worker-token-9c41";

/// `fermi worker` as its own process, on a free local port.
pub struct WorkerProc {
    pub name: String,
    pub url: String,
    pub dir: PathBuf,
    child: std::process::Child,
}

impl WorkerProc {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Kills the worker and everything it started (a crash, as far as clients can tell).
    pub fn kill(&mut self) {
        crate::kill_tree(self.child.id());
        let _ = self.child.wait();
    }
}

impl Drop for WorkerProc {
    fn drop(&mut self) {
        crate::kill_tree(self.child.id());
        let _ = self.child.wait();
    }
}

/// Starts a worker on the fermi home `home` (its accounts run the fake CLI).
/// `protocol` overrides the version it speaks, to test handshakes.
pub fn start_worker(home: &Path, name: &str, protocol: Option<u32>) -> WorkerProc {
    let dir = home.join("worker-jobs");
    let mut cmd = std::process::Command::new(crate::sibling("fermi-worker"));
    cmd.args(["--listen", "127.0.0.1:0", "--token", TOKEN, "--name", name, "--dir"])
        .arg(&dir)
        .env("FERMI_HOME", home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    if let Some(p) = protocol {
        cmd.env("FERMI_WORKER_PROTOCOL", p.to_string());
    }
    let mut child = cmd.spawn().expect("starting fermi-worker");
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    out.read_line(&mut line).unwrap();
    let addr = line.trim().strip_prefix("listening ").unwrap_or_else(|| panic!("fermi-worker said {line:?}")).to_string();
    WorkerProc { name: name.into(), url: format!("http://{addr}"), dir, child }
}

/// `[[workers]]` and `[remote]` for a controller's config.
pub fn workers_toml(workers: &[(&str, &str)], heartbeat_secs: u64, retries: u32) -> String {
    let mut s = String::new();
    for (name, url) in workers {
        s += &format!("\n[[workers]]\nname = \"{name}\"\nurl = \"{url}\"\ntoken = \"{TOKEN}\"\n");
    }
    s + &format!("\n[remote]\nheartbeat_secs = {heartbeat_secs}\nretries = {retries}\n")
}

/// A worker's job states, read from its folder.
pub fn jobs(dir: &Path) -> Vec<Value> {
    std::fs::read_dir(dir.join("jobs"))
        .into_iter()
        .flatten()
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.ok()?.path().join("job.json")).ok()?).ok())
        .collect()
}

/// Waits until `f` holds (or the time runs out); returns whether it did.
pub async fn until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    f()
}

// ── a proxy that can lose the connection ────────────────────────────────────

pub const PASS: u8 = 0;
/// Accepts and closes at once: the worker looks unreachable.
pub const REFUSE: u8 = 1;
/// Forwards the request, then closes without the answer: a lost acknowledgement.
pub const DROP_ANSWERS: u8 = 2;

pub struct Proxy {
    pub url: String,
    pub mode: Arc<AtomicU8>,
    /// Requests seen per "METHOD /path" prefix, e.g. how often a job was submitted.
    pub submits: Arc<AtomicUsize>,
    pub cancels: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Proxy {
    pub fn set(&self, mode: u8) {
        self.mode.store(mode, Ordering::SeqCst);
    }
}

pub async fn proxy(target: &str) -> Proxy {
    let target = target.trim_start_matches("http://").to_string();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let mode = Arc::new(AtomicU8::new(PASS));
    let (submits, cancels) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (m, s, c) = (mode.clone(), submits.clone(), cancels.clone());
    let task = tokio::spawn(async move {
        while let Ok((mut client, _)) = listener.accept().await {
            let (m, s, c, target) = (m.clone(), s.clone(), c.clone(), target.clone());
            tokio::spawn(async move {
                let mode = m.load(Ordering::SeqCst);
                if mode == REFUSE {
                    return; // dropped: the client sees the connection close
                }
                let Ok(mut server) = TcpStream::connect(&target).await else { return };
                // One request per connection is enough for reqwest's short calls; forward both ways.
                let (mut cr, mut cw) = client.split();
                let (mut sr, mut sw) = server.split();
                let up = async {
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        let n = match cr.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        // A connection may carry several requests: count each request line.
                        let text = String::from_utf8_lossy(&buf[..n]);
                        for line in text.lines().filter(|l| l.starts_with("POST /v1/jobs") && l.ends_with("HTTP/1.1")) {
                            if line.starts_with("POST /v1/jobs HTTP") {
                                s.fetch_add(1, Ordering::SeqCst);
                            } else if line.contains("/cancel ") {
                                c.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                        if sw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                };
                let down = async {
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        let n = match sr.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => n,
                        };
                        if m.load(Ordering::SeqCst) == DROP_ANSWERS {
                            break; // the worker answered; the client never hears it
                        }
                        if cw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                };
                tokio::select! { _ = up => {}, _ = down => {} }
            });
        }
    });
    Proxy { url, mode, submits, cancels, task }
}

// ── a worker in a container ─────────────────────────────────────────────────

/// Whether Docker can run Linux containers here.
pub fn docker() -> bool {
    std::process::Command::new("docker")
        .args(["info", "--format", "{{.OSType}}"])
        .output()
        .is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "linux")
        && cfg!(target_os = "linux")
}

pub struct DockerWorker {
    pub name: String,
    pub url: String,
    pub image: String,
    pub digest: String,
    pub container: String,
}

impl DockerWorker {
    pub fn kill(&self) {
        let _ = std::process::Command::new("docker").args(["kill", &self.container]).output();
    }
}

impl Drop for DockerWorker {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker").args(["rm", "-f", &self.container]).output();
    }
}

fn run(cmd: &mut std::process::Command) -> Result<String, String> {
    let o = cmd.output().map_err(|e| e.to_string())?;
    if !o.status.success() {
        return Err(format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)));
    }
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Builds an image with `fermi-worker`, the fake CLI and a fermi home whose
/// Claude account `c1` plays `steps`, and starts it with a published port.
pub fn docker_worker(work: &Path, name: &str, steps: Vec<Value>) -> Result<DockerWorker, String> {
    let ctx = work.join(format!("docker-{name}"));
    let _ = std::fs::remove_dir_all(&ctx);
    let acct = ctx.join("home/accounts/c1");
    std::fs::create_dir_all(&acct).map_err(|e| e.to_string())?;
    std::fs::write(acct.join("scenario.json"), serde_json::to_vec_pretty(&json!({ "steps": steps })).unwrap())
        .map_err(|e| e.to_string())?;
    std::fs::write(acct.join(".credentials.json"), "{}").map_err(|e| e.to_string())?;
    let cfg = "default_profile = \"p\"\n\n[bins]\nclaude = \"/opt/fermi/fake-cli\"\ncodex = \"/opt/fermi/fake-cli\"\ngemini = \"/opt/fermi/fake-cli\"\n\n\
               [[accounts]]\nid = \"c1\"\nprovider = \"claude\"\nauth = \"cli\"\nhome = \"/fermi-home/accounts/c1\"\n\n\
               [[profiles]]\nname = \"p\"\naccounts = [\"c1\"]\nmode = \"auto\"\n";
    std::fs::write(ctx.join("home/config.toml"), cfg).map_err(|e| e.to_string())?;
    for bin in ["fermi-worker", "fake-cli"] {
        std::fs::copy(crate::sibling(bin), ctx.join(bin)).map_err(|e| e.to_string())?;
    }
    std::fs::write(
        ctx.join("Dockerfile"),
        "FROM ubuntu:24.04\n\
         RUN apt-get update && apt-get install -y --no-install-recommends git ca-certificates && rm -rf /var/lib/apt/lists/*\n\
         COPY fermi-worker fake-cli /opt/fermi/\n\
         COPY home /fermi-home\n\
         ENV FERMI_HOME=/fermi-home\n\
         EXPOSE 8787\n\
         ENTRYPOINT [\"/opt/fermi/fermi-worker\"]\n",
    )
    .map_err(|e| e.to_string())?;
    let image = format!("fermi-acceptance-worker:{name}");
    run(std::process::Command::new("docker").args(["build", "-q", "-t", &image]).arg(&ctx))?;
    let digest = run(std::process::Command::new("docker").args(["image", "inspect", "--format", "{{.Id}}", &image]))?;
    let container = format!("fermi-worker-{name}-{}", std::process::id());
    let _ = std::process::Command::new("docker").args(["rm", "-f", &container]).output();
    run(std::process::Command::new("docker").args([
        "run",
        "-d",
        "--name",
        &container,
        "-p",
        "127.0.0.1::8787",
        &image,
        "--listen",
        "0.0.0.0:8787",
        "--token",
        TOKEN,
        "--name",
        name,
        "--dir",
        "/jobs",
    ]))?;
    let port = run(std::process::Command::new("docker").args(["port", &container, "8787"]))?;
    let addr = port.lines().find(|l| l.starts_with("127.0.0.1")).unwrap_or(&port).to_string();
    let url = format!("http://{addr}");
    // Ready once it answers (any answer: it wants a token).
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        if std::net::TcpStream::connect(addr.trim_start_matches("http://")).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(DockerWorker { name: name.into(), url, image, digest, container })
}
