use serde_json::Value;
use std::fs;
use std::path::PathBuf;

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
}

/// Where the app keeps its own config: SA_HOME, else ~/.config/sa.
pub fn sa_home() -> PathBuf {
    std::env::var("SA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".config").join("sa"))
}

pub fn sg_base() -> PathBuf {
    std::env::var("SYSTEMGRAPH_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".systemgraph"))
}

pub fn sg_bin() -> String {
    if let Ok(p) = std::env::var("SG_BIN") {
        if !p.trim().is_empty() {
            return p;
        }
    }
    let own = home().join(".local/bin/sg");
    if own.exists() {
        return own.to_string_lossy().into();
    }
    "sg".into()
}

pub fn brain_bin() -> String {
    if let Ok(p) = std::env::var("BRAIN_BIN") {
        if !p.trim().is_empty() {
            return p;
        }
    }
    let own = home().join(".local/bin/brain");
    if own.exists() {
        return own.to_string_lossy().into();
    }
    "brain".into()
}

pub const BASE_URL: &str = "https://api.deepseek.com";

pub fn api_key() -> Option<String> {
    if let Ok(k) = std::env::var("DEEPSEEK_API_KEY") {
        let k = k.trim().to_string();
        if !k.is_empty() {
            return Some(k);
        }
    }
    let path = home().join(".pi/agent/auth.json");
    let text = fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("deepseek")?
        .get("key")?
        .as_str()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

#[derive(Clone, Debug)]
pub struct Root {
    pub name: String,
    pub path: String,
    pub graph: String,
    pub nodes: u64,
}

pub fn roots() -> Vec<Root> {
    let path = sg_base().join("registry.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(list) = v.get("roots").and_then(|r| r.as_array()) {
        for r in list {
            let name = r.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            let shard = sg_base().join("shards").join(format!("{}.json", name));
            let nodes = fs::read_to_string(&shard)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| v.get("nodes").and_then(|n| n.as_u64()))
                .or_else(|| r.get("nodes").and_then(|n| n.as_u64()))
                .unwrap_or(0);
            out.push(Root {
                name,
                path: r.get("path").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                graph: r.get("graph").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                nodes,
            });
        }
    }
    out
}

pub fn system_counts() -> (usize, usize, usize) {
    let path = sg_base().join("system.json");
    let Ok(text) = fs::read_to_string(path) else {
        return (0, 0, 0);
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return (0, 0, 0);
    };
    let nodes = v.get("nodes").and_then(|n| n.as_array()).map(|a| a.len()).unwrap_or(0);
    let edges = v.get("edges").and_then(|n| n.as_array()).map(|a| a.len()).unwrap_or(0);
    let jobs = v
        .get("nodes")
        .and_then(|n| n.as_array())
        .map(|a| {
            a.iter()
                .filter(|n| n.get("kind").and_then(|k| k.as_str()) == Some("job"))
                .count()
        })
        .unwrap_or(0);
    (nodes, edges, jobs)
}

pub struct AgentDef {
    pub name: &'static str,
    pub headless: bool,
    pub path: String,
}

pub fn agents() -> Vec<AgentDef> {
    let defs = [
        ("claude", true),
        ("codex", true),
        ("pi", true),
        ("grok", false),
        ("fusion", false),
        ("fusion5", false),
    ];
    defs.iter()
        .map(|(name, headless)| AgentDef {
            name,
            headless: *headless,
            path: which(name),
        })
        .collect()
}

pub fn which(name: &str) -> String {
    let own = home().join(".local/bin").join(name);
    if own.exists() {
        return own.to_string_lossy().into();
    }
    let path = std::env::var("PATH").unwrap_or_default();
    for dir in path.split(':') {
        let p = PathBuf::from(dir).join(name);
        if p.exists() {
            return p.to_string_lossy().into();
        }
    }
    String::new()
}

pub fn sessions_dir() -> PathBuf {
    let d = sg_base().join("sessions");
    let _ = fs::create_dir_all(&d);
    d
}
