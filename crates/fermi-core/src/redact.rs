//! Removes secrets from everything fermi keeps about a task (history, tapes,
//! trace exports, the live stream) before it is written: common credential
//! formats, the values of this process's secret-looking environment
//! variables, and the patterns in `[trace] redact`.

use crate::events::Event;
use anyhow::{Context, Result};
use regex::{Captures, Regex};
use serde_json::Value;
use std::borrow::Cow;

pub const MASK: &str = "[REDACTED]";

/// Credential formats. When a pattern has a group, only the group is removed
/// (`password=` stays, so the diagnostic still says what was there).
const BUILTIN: &[&str] = &[
    r"\bsk-ant-[A-Za-z0-9_\-]{16,}",
    r"\bsk-(?:proj-)?[A-Za-z0-9_\-]{20,}",
    r"\bgh[pousr]_[A-Za-z0-9]{30,}",
    r"\bgithub_pat_[A-Za-z0-9_]{30,}",
    r"\bAKIA[0-9A-Z]{16}\b",
    r"\bAIza[0-9A-Za-z_\-]{35}",
    r"\bxox[abprs]-[A-Za-z0-9\-]{10,}",
    r"\beyJ[A-Za-z0-9_\-]{8,}\.eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}",
    r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
    r"(?i)\bbearer\s+([A-Za-z0-9_\-\.=]{16,})",
    // `password=…`, `DATABASE_PASSWORD: …`, `aws_access_key=…`, `GITHUB_TOKEN=…`
    r#"(?i)\b(?:[a-z0-9]+[_-])*(?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key)\b["']?\s*[:=]\s*["']?([^\s"',;]{8,})"#,
];

/// Environment variable names whose values are treated as secrets.
const SECRET_NAMES: &[&str] = &["KEY", "TOKEN", "SECRET", "PASSWORD", "PASSWD", "CREDENTIAL", "PRIVATE"];

pub struct Redactor {
    patterns: Vec<Regex>,
    /// Exact secret values (from the environment).
    values: Vec<String>,
}

impl Redactor {
    /// The built-in patterns plus `extra` (checked: a bad pattern is a config error).
    pub fn new(extra: &[String]) -> Result<Redactor> {
        let mut patterns: Vec<Regex> = BUILTIN.iter().map(|p| Regex::new(p).expect("built-in pattern")).collect();
        for p in extra {
            patterns.push(Regex::new(p).with_context(|| format!("[trace] redact: bad pattern `{p}`"))?);
        }
        Ok(Redactor { patterns, values: vec![] })
    }

    /// Also removes the values of secret-looking environment variables
    /// (`*_TOKEN`, `*_API_KEY`…) of this process.
    pub fn with_env(mut self) -> Redactor {
        for (k, v) in std::env::vars() {
            let k = k.to_ascii_uppercase();
            let path_like = v.contains(['/', '\\', ' ', ';']) || v.as_bytes().get(1) == Some(&b':');
            if v.len() >= 12 && !path_like && SECRET_NAMES.iter().any(|n| k.contains(n)) {
                self.values.push(v);
            }
        }
        // Longest first, so one value inside another is removed whole.
        self.values.sort_by_key(|v| std::cmp::Reverse(v.len()));
        self.values.dedup();
        self
    }

    pub fn text<'a>(&self, s: &'a str) -> Cow<'a, str> {
        let mut out: Cow<str> = Cow::Borrowed(s);
        for v in &self.values {
            if out.contains(v.as_str()) {
                out = Cow::Owned(out.replace(v.as_str(), MASK));
            }
        }
        for re in &self.patterns {
            if !re.is_match(&out) {
                continue;
            }
            let replaced = re
                .replace_all(&out, |c: &Captures| {
                    let all = c.get(0).expect("match");
                    match c.get(1) {
                        Some(g) => {
                            let s = all.as_str();
                            format!("{}{MASK}{}", &s[..g.start() - all.start()], &s[g.end() - all.start()..])
                        }
                        None => MASK.to_string(),
                    }
                })
                .into_owned();
            out = Cow::Owned(replaced);
        }
        out
    }

    /// Redacts every string in `v`; true if anything changed.
    pub fn value(&self, v: &mut Value) -> bool {
        match v {
            Value::String(s) => match self.text(s) {
                Cow::Owned(t) => {
                    *s = t;
                    true
                }
                Cow::Borrowed(_) => false,
            },
            Value::Array(a) => a.iter_mut().fold(false, |acc, x| self.value(x) | acc),
            Value::Object(o) => o.values_mut().fold(false, |acc, x| self.value(x) | acc),
            _ => false,
        }
    }

    pub fn event(&self, ev: &Event) -> Event {
        let Ok(mut v) = serde_json::to_value(ev) else { return ev.clone() };
        if !self.value(&mut v) {
            return ev.clone();
        }
        serde_json::from_value(v).unwrap_or_else(|_| Event::Log { line: "[an event was withheld: it couldn't be redacted]".into() })
    }

    /// A JSON line (a CLI's output): redacted inside its strings, so the line stays valid JSON.
    pub fn line(&self, line: &str) -> String {
        match serde_json::from_str::<Value>(line) {
            Ok(mut v) => {
                if self.value(&mut v) {
                    v.to_string()
                } else {
                    line.to_string()
                }
            }
            Err(_) => self.text(line).into_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_go_and_diagnostics_stay() {
        let r = Redactor::new(&["ACME-[0-9a-f]{16}".into()]).unwrap();
        let s = "error[E0425] at src/lib.rs:3 key sk-ant-api03-abcdefghijklmnopqrstu and ghp_abcdefghijklmnopqrstuvwxyz0123456789 \
                 password=hunter2hunter2 DATABASE_PASSWORD=Sup3rS3cretValue Authorization: Bearer abcdefghijklmnopqrstuvwx ACME-0123456789abcdef";
        let t = r.text(s);
        for secret in ["sk-ant-api03", "ghp_abc", "hunter2hunter2", "Sup3rS3cretValue", "abcdefghijklmnopqrstuvwx", "ACME-0123456789abcdef"]
        {
            assert!(!t.contains(secret), "{secret} survived: {t}");
        }
        assert!(t.contains("error[E0425] at src/lib.rs:3"), "{t}");
        assert!(t.contains("password=[REDACTED]"), "{t}");
        // JSON lines stay valid JSON.
        let line = r#"{"type":"user","content":"token: \"sk-proj-abcdefghijklmnopqrstuvwxyz\""}"#;
        let v: Value = serde_json::from_str(&r.line(line)).unwrap();
        assert!(!v.to_string().contains("abcdefghijklmnop"));
        assert!(Redactor::new(&["(".into()]).is_err());
    }
}
