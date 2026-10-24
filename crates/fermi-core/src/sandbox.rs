//! Sandboxed agent processes (`[security] sandbox = true`).
//!
//! What a sandboxed agent CLI (and everything it starts) gets:
//!
//! - **Environment**: only an allowlist of variables (system ones a program
//!   needs to run, plus `env_allow`); everything else, such as API keys in
//!   fermi's own environment, is left out. All platforms.
//! - **Files** (Linux, Landlock): read and write in the task's folder, its
//!   account's config folder and a private temp folder; read-only access to
//!   system folders, the CLI's install folder and `read_only` extras; no
//!   other paths, including other tasks' folders, fermi's history and other
//!   accounts. `/proc` is limited to the process itself.
//! - **Network**: requests go through fermi's egress proxy, which only lets
//!   through hosts in `net_allow` (plus the providers' API hosts) and logs every
//!   decision to `~/.fermi/egress.jsonl`. On Linux, Landlock also limits TCP
//!   connections to the proxy's port (kernel 6.7+, Landlock ABI 4), so a
//!   program can't go around it. UDP (DNS) isn't covered.
//!
//! Elsewhere (Windows, macOS) only the environment allowlist and the proxy
//! settings apply; file and network boundaries aren't enforced by the OS.

use crate::util::now;
use anyhow::Result;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// Variables a program needs to run, kept in a sandboxed environment.
const ENV_BASE: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TERM",
    "TZ",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "SYSTEMROOT",
    "SystemRoot",
    "WINDIR",
    "windir",
    "COMSPEC",
    "ComSpec",
    "PATHEXT",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "ProgramData",
    "CommonProgramFiles",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "NUMBER_OF_PROCESSORS",
    "HOMEDRIVE",
    "HOMEPATH",
    "USERNAME",
    "COMPUTERNAME",
    "SystemDrive",
];

/// API hosts the agent CLIs need, allowed through the proxy by default.
pub const PROVIDER_HOSTS: &[&str] = &[
    "api.anthropic.com",
    "*.anthropic.com",
    "claude.ai",
    "*.claude.ai",
    "api.openai.com",
    "*.openai.com",
    "chatgpt.com",
    "*.chatgpt.com",
    "generativelanguage.googleapis.com",
    "*.googleapis.com",
    "accounts.google.com",
];

/// How a sandboxed process is set up.
#[derive(Debug, Clone)]
pub struct Jail {
    /// Read and write.
    pub read_write: Vec<PathBuf>,
    /// Read and execute.
    pub read_only: Vec<PathBuf>,
    /// Variables passed through (by name).
    pub env_allow: Vec<String>,
    /// The egress proxy for this process, if network is restricted.
    pub proxy: Option<u16>,
    /// The private temp folder.
    pub tmp: PathBuf,
}

impl Jail {
    /// The environment for the child: the allowlisted variables of this
    /// process, then the proxy and temp settings.
    pub fn env(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| ENV_BASE.contains(&k.as_str()) || self.env_allow.iter().any(|a| a.eq_ignore_ascii_case(k)))
            .collect();
        let tmp = self.tmp.to_string_lossy().to_string();
        for k in ["TMPDIR", "TEMP", "TMP"] {
            out.push((k.into(), tmp.clone()));
        }
        if let Some(p) = self.proxy {
            let url = format!("http://127.0.0.1:{p}");
            for k in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy"] {
                out.push((k.into(), url.clone()));
            }
            out.push(("NO_PROXY".into(), String::new()));
        }
        out.push(("FERMI_SANDBOXED".into(), "1".into()));
        out
    }
}

/// Whether the OS enforces file and network boundaries here.
pub fn os_enforced() -> (bool, bool) {
    #[cfg(target_os = "linux")]
    {
        let abi = linux::abi();
        (abi >= 1, abi >= 4)
    }
    #[cfg(not(target_os = "linux"))]
    {
        (false, false)
    }
}

/// Builds the jail for one agent run.
pub fn jail(
    cfg: &crate::config::Config,
    home: &Path,
    cwd: &Path,
    account_home: Option<&Path>,
    bin: &Path,
    proxy: Option<u16>,
) -> Result<Jail> {
    let tmp = home.join("sandbox").join(format!("tmp-{}", &crate::util::uuid_v4()[..8]));
    std::fs::create_dir_all(&tmp)?;
    let mut read_write = vec![
        cwd.to_path_buf(),
        tmp.clone(),
        home.join("memory"),
        home.join("index"),
        home.join("locks"),
        home.join("approvals"),
        home.join("run"),
    ];
    for d in &read_write {
        let _ = std::fs::create_dir_all(d);
    }
    read_write.extend(account_home.map(Path::to_path_buf));
    read_write.extend(cfg.security.read_write.iter().map(|p| crate::util::expand(p)));
    let mut read_only = system_dirs();
    // The CLI and the folder it's installed in (node packages live next to it).
    if let Ok(real) = std::fs::canonicalize(bin) {
        read_only.extend(real.parent().map(Path::to_path_buf));
    }
    read_only.extend(std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)));
    read_only.push(home.join("config.toml"));
    read_only.push(home.join("ledger.jsonl"));
    read_only.extend(cfg.security.read_only.iter().map(|p| crate::util::expand(p)));
    let mut env_allow = cfg.security.env_allow.clone();
    env_allow.extend(cfg.accounts.iter().filter_map(|a| a.api_key_env.clone()));
    Ok(Jail { read_write, read_only, env_allow, proxy, tmp })
}

/// System folders every sandboxed program may read (and run programs from).
pub fn system_dirs() -> Vec<PathBuf> {
    ["/usr", "/lib", "/lib64", "/lib32", "/bin", "/sbin", "/etc", "/opt", "/nix", "/snap", "/dev", "/sys/devices/system/cpu"]
        .iter()
        .map(PathBuf::from)
        .collect()
}

/// Applies the jail to a command about to be spawned.
pub fn apply(cmd: &mut tokio::process::Command, j: &Jail) -> Result<()> {
    cmd.env_clear();
    cmd.envs(j.env());
    #[cfg(target_os = "linux")]
    linux::restrict(cmd, j)?;
    Ok(())
}

#[cfg(target_os = "linux")]
mod linux {
    use super::Jail;
    use anyhow::Result;
    use landlock::{
        path_beneath_rules, Access, AccessFs, AccessNet, CompatLevel, Compatible, NetPort, Ruleset, RulesetAttr, RulesetCreated,
        RulesetCreatedAttr, ABI,
    };
    use std::sync::Mutex;

    /// The kernel's Landlock ABI version (0 = unavailable).
    pub fn abi() -> i64 {
        // landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)
        let v = unsafe { libc::syscall(libc::SYS_landlock_create_ruleset, std::ptr::null::<libc::c_void>(), 0usize, 1u32) };
        if v < 0 {
            0
        } else {
            v
        }
    }

    fn ruleset(j: &Jail) -> Result<RulesetCreated> {
        let abi = ABI::V5;
        let mut r = Ruleset::default().set_compatibility(CompatLevel::BestEffort).handle_access(AccessFs::from_all(abi))?;
        if j.proxy.is_some() {
            r = r.handle_access(AccessNet::ConnectTcp)?;
        }
        let mut c = r
            .create()?
            .add_rules(path_beneath_rules(&j.read_only, AccessFs::from_read(abi)))?
            .add_rules(path_beneath_rules(&j.read_write, AccessFs::from_all(abi)))?
            // A few system files programs read; the process's own /proc entry is
            // added in the child (here /proc/self would be fermi's, environment included).
            .add_rules(path_beneath_rules(
                ["/proc/cpuinfo", "/proc/meminfo", "/proc/stat", "/proc/loadavg", "/proc/sys/kernel"],
                AccessFs::from_read(abi),
            ))?
            .add_rules(path_beneath_rules(["/dev/null", "/dev/tty", "/dev/pts"], AccessFs::from_all(abi)))?;
        if let Some(p) = j.proxy {
            c = c.add_rule(NetPort::new(p, AccessNet::ConnectTcp))?;
        }
        Ok(c)
    }

    pub fn restrict(cmd: &mut tokio::process::Command, j: &Jail) -> Result<()> {
        // Built here (files opened, rules added); only applied in the child.
        let rs = Mutex::new(Some(ruleset(j)?));
        unsafe {
            cmd.pre_exec(move || {
                let r = rs.lock().map_err(|_| std::io::Error::other("sandbox lock"))?.take();
                if let Some(r) = r {
                    let other = |e: landlock::RulesetError| std::io::Error::other(e.to_string());
                    // Opened in the child: /proc/self is now this process.
                    let own = landlock::PathFd::new("/proc/self").map_err(|e| std::io::Error::other(e.to_string()))?;
                    let r = r.add_rule(landlock::PathBeneath::new(own, AccessFs::from_read(ABI::V5))).map_err(other)?;
                    r.restrict_self().map_err(other)?;
                }
                Ok(())
            });
        }
        Ok(())
    }
}

// ── the egress proxy ────────────────────────────────────────────────────────

/// Whether `host:port` is allowed by `allow` (`host`, `host:port`, `*.domain`).
pub fn host_allowed(allow: &[String], host: &str, port: u16) -> bool {
    let host = host.trim_matches(['[', ']']).to_lowercase();
    allow.iter().any(|a| {
        let a = a.trim().to_lowercase();
        let (h, p) = match a.rsplit_once(':') {
            Some((h, p)) if p.parse::<u16>().is_ok() => (h.to_string(), p.parse::<u16>().ok()),
            _ => (a.clone(), None),
        };
        let host_ok = match h.strip_prefix("*.") {
            Some(d) => host.ends_with(&format!(".{d}")),
            None => host == h,
        };
        host_ok && p.is_none_or(|p| p == port)
    })
}

/// An HTTP proxy (CONNECT and plain requests) that only reaches allowed
/// hosts. It stops accepting connections when dropped.
pub struct Proxy {
    pub port: u16,
    accept: tokio::task::AbortHandle,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

impl Proxy {
    /// Starts on 127.0.0.1 (any free port). Decisions go to `log` as JSON lines.
    pub async fn start(allow: Vec<String>, log: PathBuf) -> Result<Proxy> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let allow = Arc::new(allow);
        let accept = tokio::spawn(async move {
            while let Ok((conn, _)) = listener.accept().await {
                let (allow, log) = (allow.clone(), log.clone());
                tokio::spawn(async move {
                    let _ = serve(conn, &allow, &log).await;
                });
            }
        })
        .abort_handle();
        Ok(Proxy { port, accept })
    }
}

fn record(log: &Path, method: &str, target: &str, allowed: bool) {
    if let Some(d) = log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = std::io::Write::write_all(
            &mut f,
            format!("{}\n", json!({ "ts": now(), "method": method, "target": target, "allowed": allowed })).as_bytes(),
        );
    }
}

async fn serve(conn: TcpStream, allow: &[String], log: &Path) -> Result<()> {
    let mut reader = BufReader::new(conn);
    let mut head = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
        head.push_str(&line);
        if head.len() > 64 * 1024 {
            anyhow::bail!("request head too long");
        }
    }
    let first = head.lines().next().unwrap_or("").to_string();
    let mut parts = first.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_string(), parts.next().unwrap_or("").to_string());
    // CONNECT host:port, or an absolute URL for a plain request.
    let (host, port, path) = if method.eq_ignore_ascii_case("CONNECT") {
        let (h, p) = target.rsplit_once(':').unwrap_or((&target, "443"));
        (h.to_string(), p.parse().unwrap_or(443), String::new())
    } else {
        let rest = target.strip_prefix("http://").unwrap_or(&target);
        let (authority, path) = rest.split_once('/').map(|(a, p)| (a, format!("/{p}"))).unwrap_or((rest, "/".into()));
        let (h, p) = authority.rsplit_once(':').map(|(h, p)| (h, p.parse().unwrap_or(80))).unwrap_or((authority, 80));
        (h.to_string(), p, path)
    };
    let allowed = !host.is_empty() && host_allowed(allow, &host, port);
    record(log, &method, &format!("{host}:{port}"), allowed);
    let mut client = reader.into_inner();
    if !allowed {
        let body = format!("fermi blocked {host}:{port}: not in the sandbox's network allowlist\n");
        client
            .write_all(format!("HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes())
            .await?;
        return Ok(());
    }
    let mut upstream = TcpStream::connect((host.as_str(), port)).await?;
    if method.eq_ignore_ascii_case("CONNECT") {
        client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    } else {
        let rest: String = head.lines().skip(1).filter(|l| !l.to_lowercase().starts_with("proxy-")).map(|l| format!("{l}\r\n")).collect();
        upstream.write_all(format!("{method} {path} HTTP/1.1\r\n{rest}\r\n").as_bytes()).await?;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    let _ = client.read(&mut [0u8; 0]).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_matching() {
        let allow = vec!["api.anthropic.com".to_string(), "*.example.com".to_string(), "127.0.0.1:8080".to_string()];
        assert!(host_allowed(&allow, "api.anthropic.com", 443));
        assert!(host_allowed(&allow, "x.example.com", 80));
        assert!(!host_allowed(&allow, "example.com", 80));
        assert!(host_allowed(&allow, "127.0.0.1", 8080));
        assert!(!host_allowed(&allow, "127.0.0.1", 9090));
        assert!(!host_allowed(&allow, "evil.test", 443));
    }
}
