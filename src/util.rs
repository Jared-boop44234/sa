use regex::Regex;
use std::sync::OnceLock;

fn pats() -> &'static Vec<Regex> {
    static PATS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATS.get_or_init(|| {
        vec![
            Regex::new(r"sk-[A-Za-z0-9_\-]{16,}").unwrap(),
            Regex::new(r"eyJ[A-Za-z0-9_\-]{20,}\.[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,}").unwrap(),
            Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9._\-]{16,}").unwrap(),
            Regex::new(
                r#"(?i)((?:api[_-]?key|secret|token|password|passwd|pwd)\s*[=:]\s*)["']?[^\s"',}]{8,}"#,
            )
            .unwrap(),
            Regex::new(r"\b[0-9a-f]{32,}\b").unwrap(),
        ]
    })
}

/// Nothing secret may reach the model, the screen or the transcript.
pub fn redact(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let mut out = s.to_string();
    for (i, re) in pats().iter().enumerate() {
        out = if i == 3 {
            re.replace_all(&out, "${1}<redacted>").to_string()
        } else {
            re.replace_all(&out, "<redacted>").to_string()
        };
    }
    out
}

pub fn truncate(s: &str, limit: usize) -> String {
    let s = redact(s);
    if s.chars().count() <= limit {
        return s;
    }
    let half = limit / 2;
    let head: String = s.chars().take(half).collect();
    let tail: String = s.chars().rev().take(half).collect::<String>()
        .chars().rev().collect();
    format!("{}\n... [{} chars elided] ...\n{}", head, s.chars().count() - limit, tail)
}

pub fn first_line(s: &str, width: usize) -> String {
    let line = s.trim().lines().next().unwrap_or("(empty)");
    let mut out: String = line.chars().take(width).collect();
    if line.chars().count() > width {
        out.push('…');
    }
    out
}

/// tool(primary argument) - reads like Claude Code's `⏺ Bash(git status)`.
pub fn call_brief(tool: &str, v: &serde_json::Value) -> String {
    let pick = |keys: &[&str]| -> Option<String> {
        for k in keys {
            if let Some(s) = v.get(*k).and_then(|x| x.as_str()) {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
        None
    };
    let primary = match tool {
        "sh" => pick(&["cmd"]),
        "read" | "write" | "patch" => pick(&["path"]),
        "graph" => pick(&["question"]),
        "graph_build" => pick(&["root"]),
        "brain" => pick(&["text"]),
        "agent" => pick(&["prompt"]),
        "job" => pick(&["name"]),
        _ => None,
    };
    match primary {
        Some(text) => {
            let short: String = text.chars().take(72).collect();
            format!("{}({}{})", tool, short, if text.chars().count() > 72 { "…" } else { "" })
        }
        None => format!("{}", tool),
    }
}

pub fn brief(v: &serde_json::Value) -> String {
    let s = match v {
        serde_json::Value::Object(m) => m
            .iter()
            .map(|(k, val)| {
                let text = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                let short: String = text.chars().take(40).collect();
                format!("{}={}", k, short)
            })
            .collect::<Vec<_>>()
            .join(" "),
        other => other.to_string(),
    };
    let short: String = s.chars().take(70).collect();
    format!("({}{})", short, if s.chars().count() > 70 { "…" } else { "" })
}

pub fn kfmt(n: u64) -> String {
    if n >= 1000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        n.to_string()
    }
}

pub fn now_iso() -> String {
    // seconds since epoch -> readable local stamp without extra crates
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}", secs)
}

pub fn stamp() -> String {
    let out = std::process::Command::new("/bin/date")
        .arg("+%Y%m%d-%H%M%S")
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => now_iso(),
    }
}
