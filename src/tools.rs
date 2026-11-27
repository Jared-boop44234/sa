use crate::config;
use crate::util;
use regex::Regex;
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const MODES: [&str; 4] = ["ask", "agent", "dispatch", "system"];
pub const MODE_BLURB: [&str; 4] = [
    "read-only: graph, files, vault search. No writes, no shell, no dispatch.",
    "full operator: shell, files, graph, vault, dispatch. Auto-approved.",
    "router: plan, hand work to claude/codex/pi, then verify.",
    "map keeper: system graph, roots, jobs, agent wiring.",
];

pub const TOOL_NAMES: [&str; 12] = [
    "graph", "system_map", "scan", "graph_build", "sh", "read", "write", "patch", "brain",
    "agent", "job", "plan",
];

fn mode_tools(mode: usize) -> Vec<&'static str> {
    match MODES.get(mode).copied().unwrap_or("agent") {
        "ask" => vec!["graph", "system_map", "read", "job", "plan", "brain"],
        "dispatch" => vec!["agent", "plan", "graph", "system_map", "read", "job", "brain"],
        "system" => vec!["graph", "system_map", "scan", "graph_build", "job", "plan", "read", "brain"],
        _ => TOOL_NAMES.to_vec(),
    }
}

pub fn allowed(name: &str, mode: usize) -> bool {
    mode_tools(mode).contains(&name)
}

#[derive(Clone, Debug)]
pub struct AgentStatus {
    pub name: &'static str,
    pub present: bool,
    pub headless: bool,
    pub status: &'static str,
    pub last: String,
    pub runs: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub steps: u32,
    pub input: u64,
    pub output: u64,
    pub cost: f64,
    pub model: String,
}

pub struct Ask {
    pub question: Mutex<String>,
    pub answer: Mutex<Option<bool>>,
    pub cv: Condvar,
    pub active: AtomicBool,
}

pub struct Shared {
    pub mode: AtomicU8,
    pub approver: Mutex<String>,
    pub statuses: Mutex<Vec<AgentStatus>>,
    pub stats: Mutex<Stats>,
    pub ask: Ask,
    pub cwd: Mutex<String>,
    pub plan: Mutex<Vec<String>>,
    pub cancel: AtomicBool,
}

impl Shared {
    pub fn new(model: &str, mode: usize, approver: &str, cwd: &str) -> Shared {
        let defs = config::agents();
        let statuses = defs
            .iter()
            .map(|d| AgentStatus {
                name: d.name,
                present: !d.path.is_empty(),
                headless: d.headless,
                status: "idle",
                last: String::new(),
                runs: 0,
            })
            .collect();
        Shared {
            mode: AtomicU8::new(mode as u8),
            approver: Mutex::new(approver.to_string()),
            statuses: Mutex::new(statuses),
            stats: Mutex::new(Stats { model: model.to_string(), ..Default::default() }),
            ask: Ask {
                question: Mutex::new(String::new()),
                answer: Mutex::new(None),
                cv: Condvar::new(),
                active: AtomicBool::new(false),
            },
            cwd: Mutex::new(cwd.to_string()),
            plan: Mutex::new(Vec::new()),
            cancel: AtomicBool::new(false),
        }
    }

    pub fn mode(&self) -> usize {
        self.mode.load(Ordering::SeqCst) as usize
    }

    pub fn set_mode(&self, mode: usize) {
        self.mode.store(mode as u8, Ordering::SeqCst);
        let mut a = self.approver.lock().unwrap();
        *a = if mode == 0 { "auto".into() } else { "yolo".into() };
    }

    pub fn approver(&self) -> String {
        self.approver.lock().unwrap().clone()
    }

    /// Ok(()) = run it. Err(msg) = refuse with a reason the model can read.
    pub fn approve(&self, tool: &str, brief: &str, why: &str) -> Result<(), String> {
        if why.starts_with("HARD:") {
            return Err(format!("blocked: {} (hard rule)", why.trim_start_matches("HARD: ")));
        }
        let mode = self.approver();
        if mode == "yolo" {
            return Ok(());
        }
        if mode == "auto" {
            return Err(format!("{} needs approval in ASK mode ({})", tool, brief));
        }
        // ask: hand the question to the UI and wait
        *self.ask.question.lock().unwrap() = format!("{} {}{}", tool, brief, if why.is_empty() {
            String::new()
        } else {
            format!(" · {}", why)
        });
        *self.ask.answer.lock().unwrap() = None;
        self.ask.active.store(true, Ordering::SeqCst);
        let mut guard = self.ask.answer.lock().unwrap();
        while guard.is_none() {
            guard = self.ask.cv.wait(guard).unwrap();
        }
        self.ask.active.store(false, Ordering::SeqCst);
        match *guard {
            Some(true) => Ok(()),
            _ => Err(format!("{} declined by operator", tool)),
        }
    }

    pub fn answer_ask(&self, yes: bool) {
        *self.ask.answer.lock().unwrap() = Some(yes);
        self.ask.cv.notify_all();
    }

    pub fn agents_snapshot(&self) -> Vec<AgentStatus> {
        self.statuses.lock().unwrap().clone()
    }

    pub fn stats_snapshot(&self) -> Stats {
        self.stats.lock().unwrap().clone()
    }

    fn set_agent(&self, name: &str, status: &'static str, last: Option<&str>) {
        let mut s = self.statuses.lock().unwrap();
        for a in s.iter_mut() {
            if a.name == name {
                a.status = status;
                if let Some(l) = last {
                    a.last = l.to_string();
                }
                if status == "working" {
                    a.runs += 1;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// guardrails
// ---------------------------------------------------------------------------
fn hard_blocks() -> &'static Vec<(Regex, &'static str)> {
    static H: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    H.get_or_init(|| {
        vec![
            (
                Regex::new(r"(?i)(security\s+find-|keychain|id_rsa|\.ssh/id_|auth\.json|credentials\.json|\*password\*)").unwrap(),
                "credential access",
            ),
            (
                Regex::new(r"\brm\s+(-[a-zA-Z]*[rf][a-zA-Z]*\s+)+(/|~|\$HOME)\s*$").unwrap(),
                "deleting home or root",
            ),
            (Regex::new(r"\bsudo\b").unwrap(), "sudo"),
            (
                Regex::new(r"\bmkfs|\bdd\s+if=|>\s*/dev/[sh]d").unwrap(),
                "raw disk write",
            ),
            (Regex::new(r":\(\)\s*\{.*\}\s*;?\s*:").unwrap(), "fork bomb"),
        ]
    })
}

fn soft_blocks() -> &'static Vec<(Regex, &'static str)> {
    static S: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    S.get_or_init(|| {
        vec![
            (Regex::new(r"\b(shutdown|reboot|halt)\b").unwrap(), "power state"),
            (
                Regex::new(r"launchctl\s+(bootout|unload|disable)").unwrap(),
                "launchd removal",
            ),
            (Regex::new(r"git\s+push\s+.*(--force|-f)\b").unwrap(), "force push"),
            (Regex::new(r"curl[^|]*\|\s*(ba)?sh").unwrap(), "piping a download into a shell"),
            (Regex::new(r"\bchmod\s+-R\s+777\s+/").unwrap(), "world-writable root"),
        ]
    })
}

// ---------------------------------------------------------------------------
// command execution with a real timeout
// ---------------------------------------------------------------------------
pub fn run(cmd: &str, args: &[&str], cwd: &str, timeout: Duration) -> Result<(i32, String, String), String> {
    let mut child = Command::new(cmd)
        .args(args)
        .current_dir(if Path::new(cwd).is_dir() { cwd } else { "/" })
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn {}: {}", cmd, e))?;

    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).to_string()
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).to_string()
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timed out after {}s", timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(format!("wait failed: {}", e)),
        }
    };
    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();
    Ok((status.code().unwrap_or(-1), stdout, stderr))
}

pub fn bash(cmd: &str, cwd: &str, timeout: Duration) -> Result<(i32, String, String), String> {
    run("/bin/bash", &["-lc", cmd], cwd, timeout)
}

// ---------------------------------------------------------------------------
// tool schemas
// ---------------------------------------------------------------------------
pub fn schema() -> Value {
    json!([
        {"type":"function","function":{
            "name":"graph","description":"Ask a question across the whole system: components in the machine system graph AND code symbols in every registered repo, with relations.",
            "parameters":{"type":"object","properties":{
                "question":{"type":"string"},
                "root":{"type":"string","description":"search one registered root only"},
                "deep":{"type":"boolean","description":"also run a graphify BFS inside that root"}},
                "required":["question"]}}},
        {"type":"function","function":{
            "name":"system_map","description":"List machine components by kind, or print the full wiring.",
            "parameters":{"type":"object","properties":{
                "kind":{"type":"string"},"wiring":{"type":"boolean"}}}}},
        {"type":"function","function":{
            "name":"scan","description":"Re-scan the machine and rebuild the system graph.",
            "parameters":{"type":"object","properties":{}}}},
        {"type":"function","function":{
            "name":"graph_build","description":"Rebuild the code graph of a registered root, or all of them.",
            "parameters":{"type":"object","properties":{
                "root":{"type":"string"},"all":{"type":"boolean"}}}}},
        {"type":"function","function":{
            "name":"sh","description":"Run a bash command. Returns stdout+stderr and the exit code.",
            "parameters":{"type":"object","properties":{
                "cmd":{"type":"string"},
                "cwd":{"type":"string"},
                "timeout":{"type":"integer","description":"seconds, default 120, max 1800"}},
                "required":["cmd"]}}},
        {"type":"function","function":{
            "name":"read","description":"Read a text file, optionally a line range.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},"start":{"type":"integer"},"limit":{"type":"integer"}},
                "required":["path"]}}},
        {"type":"function","function":{
            "name":"write","description":"Create or overwrite a text file.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},"content":{"type":"string"}},
                "required":["path","content"]}}},
        {"type":"function","function":{
            "name":"patch","description":"Replace one exact unique string in a file.",
            "parameters":{"type":"object","properties":{
                "path":{"type":"string"},"old":{"type":"string"},"new":{"type":"string"}},
                "required":["path","old","new"]}}},
        {"type":"function","function":{
            "name":"brain","description":"Read or write the second-brain vault. action=search|remember.",
            "parameters":{"type":"object","properties":{
                "action":{"type":"string","enum":["search","remember"]},
                "text":{"type":"string"},"kind":{"type":"string"},"limit":{"type":"integer"}},
                "required":["action","text"]}}},
        {"type":"function","function":{
            "name":"agent","description":"Dispatch another agent headlessly and return its answer. claude=architect, codex=builder, pi=generalist.",
            "parameters":{"type":"object","properties":{
                "name":{"type":"string","enum":["claude","codex","pi"]},
                "prompt":{"type":"string"},"cwd":{"type":"string"},"timeout":{"type":"integer"}},
                "required":["name","prompt"]}}},
        {"type":"function","function":{
            "name":"job","description":"Inspect launchd jobs: list, or show one job's schedule and scripts.",
            "parameters":{"type":"object","properties":{
                "action":{"type":"string","enum":["list","show"]},"name":{"type":"string"}}}}},
        {"type":"function","function":{
            "name":"plan","description":"Set or read the task list for multi-step work.",
            "parameters":{"type":"object","properties":{
                "items":{"type":"array","items":{"type":"string"}}}}}},
    ])
}

pub fn schema_for(mode: usize) -> Value {
    let all = schema();
    if mode == 1 {
        return all;
    }
    let keep = mode_tools(mode);
    let filtered: Vec<Value> = all
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| {
            t.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|n| n.as_str())
                .map(|n| keep.contains(&n))
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    Value::Array(filtered)
}

// ---------------------------------------------------------------------------
// execution
// ---------------------------------------------------------------------------
fn expand(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        return config::home().join(rest).to_string_lossy().into();
    }
    if p == "~" {
        return config::home().to_string_lossy().into();
    }
    p.to_string()
}

pub fn execute(name: &str, args: &Value, sh: &Shared) -> String {
    let mode = sh.mode();
    if !allowed(name, mode) {
        return format!(
            "unavailable: {} is not part of {} mode ({})",
            name,
            MODES[mode].to_uppercase(),
            MODE_BLURB[mode]
        );
    }
    let result = match name {
        "graph" => t_graph(args, sh),
        "system_map" => t_system_map(args),
        "scan" => t_scan(),
        "graph_build" => t_graph_build(args, sh),
        "sh" => t_sh(args, sh),
        "read" => t_read(args),
        "write" => t_write(args, sh),
        "patch" => t_patch(args, sh),
        "brain" => t_brain(args, sh),
        "agent" => t_agent(args, sh),
        "job" => t_job(args),
        "plan" => t_plan(args, sh),
        other => return format!("error: unknown tool {}", other),
    };
    util::truncate(&result, 24000)
}

fn s_arg<'a>(args: &'a Value, key: &str) -> String {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn n_arg(args: &Value, key: &str, default: u64) -> u64 {
    args.get(key).and_then(|v| v.as_u64()).unwrap_or(default)
}

fn t_graph(args: &Value, _sh: &Shared) -> String {
    let q = s_arg(args, "question");
    let root = s_arg(args, "root");
    let deep = args.get("deep").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut cmd: Vec<&str> = vec!["query", &q];
    if deep {
        cmd.push("--deep");
        if !root.is_empty() {
            cmd.push("--root");
            cmd.push(&root);
        }
    }
    match run(&config::sg_bin(), &cmd, &sh_cwd(), Duration::from_secs(120)) {
        Ok((_, out, err)) => {
            let text = format!("{}{}", out, err);
            if text.trim().is_empty() {
                "no matches".into()
            } else {
                text
            }
        }
        Err(e) => format!("error: {}", e),
    }
}

fn sh_cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().into())
        .unwrap_or_else(|_| "/".into())
}

fn t_system_map(args: &Value) -> String {
    let path = config::sg_base().join("system.json");
    let Ok(text) = fs::read_to_string(path) else {
        return "no system graph yet - run scan".into();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return "system.json unreadable".into();
    };
    let kind = s_arg(args, "kind");
    let wiring = args.get("wiring").and_then(|b| b.as_bool()).unwrap_or(false);
    let empty = vec![];
    let nodes = v.get("nodes").and_then(|n| n.as_array()).unwrap_or(&empty);
    let edges = v.get("edges").and_then(|n| n.as_array()).unwrap_or(&empty);
    let label = |id: &str| -> String {
        nodes
            .iter()
            .find(|n| n.get("id").and_then(|i| i.as_str()) == Some(id))
            .and_then(|n| n.get("label").and_then(|l| l.as_str()))
            .unwrap_or(id)
            .to_string()
    };
    let mut out = vec![format!(
        "{} nodes / {} edges, built {}",
        nodes.len(),
        edges.len(),
        v.get("built_at").and_then(|b| b.as_str()).unwrap_or("-")
    )];
    for n in nodes {
        let k = n.get("kind").and_then(|x| x.as_str()).unwrap_or("");
        if !kind.is_empty() && k != kind {
            continue;
        }
        let lbl = n.get("label").and_then(|x| x.as_str()).unwrap_or("");
        let extra = n
            .get("path")
            .and_then(|x| x.as_str())
            .or_else(|| n.get("description").and_then(|x| x.as_str()))
            .unwrap_or("");
        out.push(format!("- [{}] {} {}", k, lbl, if extra.is_empty() { String::new() } else { format!("({})", extra) }));
    }
    if wiring || kind.is_empty() {
        out.push(String::new());
        out.push("WIRING".into());
        for e in edges {
            let s = e.get("source").and_then(|x| x.as_str()).unwrap_or("");
            let t = e.get("target").and_then(|x| x.as_str()).unwrap_or("");
            let rel = e.get("rel").and_then(|x| x.as_str()).unwrap_or("");
            out.push(format!("  {} --{}--> {}", label(s), rel, label(t)));
        }
    }
    out.join("\n")
}

fn t_scan() -> String {
    match run(&config::sg_bin(), &["scan"], &sh_cwd(), Duration::from_secs(300)) {
        Ok((_, out, err)) => format!("{}{}", out, err).trim().to_string(),
        Err(e) => format!("scan failed: {}", e),
    }
}

fn t_graph_build(args: &Value, sh: &Shared) -> String {
    let root = s_arg(args, "root");
    let all = args.get("all").and_then(|b| b.as_bool()).unwrap_or(false);
    if let Err(e) = sh.approve("graph_build", &format!("root={}", root), "rebuilds graph files") {
        return e;
    }
    let mut cmd: Vec<String> = vec!["build".into()];
    if all {
        cmd.push("--all".into());
    } else {
        cmd.push(root);
    }
    let refs: Vec<&str> = cmd.iter().map(|s| s.as_str()).collect();
    match run(&config::sg_bin(), &refs, &sh_cwd(), Duration::from_secs(1800)) {
        Ok((_, out, err)) => format!("{}{}", out, err).trim().to_string(),
        Err(e) => format!("build failed: {}", e),
    }
}

fn t_sh(args: &Value, sh: &Shared) -> String {
    let cmd = s_arg(args, "cmd");
    // yolo == Claude Code's bypass-permissions: the shell is the shell. Only the two
    // confirm-modes consult the pattern lists, so nothing is silently rewritten.
    if sh.approver() != "yolo" {
        for (re, why) in hard_blocks() {
            if re.is_match(&cmd) {
                return format!("needs confirmation: {} ({}). Re-run with confirmation, or use yolo.", why, cmd);
            }
        }
        for (re, why) in soft_blocks() {
            if re.is_match(&cmd) {
                return format!("needs confirmation: {} ({}). Re-run with confirmation, or use yolo.", why, cmd);
            }
        }
        if let Err(e) = sh.approve("sh", &format!("cmd={}", cmd.chars().take(80).collect::<String>()), "") {
            return e;
        }
    }
    let cwd = {
        let a = s_arg(args, "cwd");
        if a.is_empty() { sh.cwd.lock().unwrap().clone() } else { expand(&a) }
    };
    let timeout = n_arg(args, "timeout", 120).min(1800);
    let started = Instant::now();
    match bash(&cmd, &cwd, Duration::from_secs(timeout)) {
        Ok((code, out, err)) => {
            let mut body = out;
            if !err.trim().is_empty() {
                body.push_str("\n[stderr]\n");
                body.push_str(&err);
            }
            format!(
                "$ {}\n{}\n[exit {}, {:.1}s]",
                cmd,
                if body.trim().is_empty() { "(no output)" } else { body.trim() },
                code,
                started.elapsed().as_secs_f64()
            )
        }
        Err(e) => format!("$ {}\n{}", cmd, e),
    }
}

fn t_read(args: &Value) -> String {
    let path = expand(&s_arg(args, "path"));
    let start = n_arg(args, "start", 1).max(1) as usize;
    let limit = n_arg(args, "limit", 400).min(4000) as usize;
    let Ok(text) = fs::read_to_string(&path) else {
        return format!("error: cannot read {}", path);
    };
    let lines: Vec<&str> = text.lines().collect();
    let mut out = format!("{} ({} lines)\n", path, lines.len());
    for (i, line) in lines.iter().skip(start - 1).take(limit).enumerate() {
        out.push_str(&format!("{:5}| {}\n", i + start, line));
    }
    let shown = start - 1 + limit;
    if shown < lines.len() {
        out.push_str(&format!("... {} more lines", lines.len() - shown));
    }
    out
}

fn t_write(args: &Value, sh: &Shared) -> String {
    let path = expand(&s_arg(args, "path"));
    let content = s_arg(args, "content");
    let yolo = sh.approver() == "yolo";
    if !yolo {
        let home = config::home().to_string_lossy().to_string();
        if !(path.starts_with(&home) || path.starts_with("/tmp/")) {
            return format!("needs confirmation: writes outside {} and /tmp are not automatic", home);
        }
        if let Err(e) = sh.approve("write", &format!("path={}", path), "writes a file") {
            return e;
        }
    }
    let existed = Path::new(&path).exists();
    if let Some(parent) = Path::new(&path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::write(&path, content.as_bytes()) {
        Ok(_) => format!(
            "{} {} ({} bytes)",
            if existed { "overwrote" } else { "created" },
            path,
            content.len()
        ),
        Err(e) => format!("error: {}", e),
    }
}

fn t_patch(args: &Value, sh: &Shared) -> String {
    let path = expand(&s_arg(args, "path"));
    let old = s_arg(args, "old");
    let new = s_arg(args, "new");
    let Ok(text) = fs::read_to_string(&path) else {
        return format!("error: cannot read {}", path);
    };
    let count = text.matches(&old).count();
    if count != 1 {
        return format!("error: `old` appears {} times in {} (must be exactly 1)", count, path);
    }
    if let Err(e) = sh.approve("patch", &format!("path={}", path), "edits a file") {
        return e;
    }
    match fs::write(&path, text.replacen(&old, &new, 1)) {
        Ok(_) => format!("patched {}", path),
        Err(e) => format!("error: {}", e),
    }
}

fn t_brain(args: &Value, sh: &Shared) -> String {
    let action = s_arg(args, "action");
    let text = s_arg(args, "text");
    if action == "search" {
        let limit = n_arg(args, "limit", 6).to_string();
        return match run(
            &config::brain_bin(),
            &["search", &text, "-k", &limit, "--fast"],
            &sh_cwd(),
            Duration::from_secs(120),
        ) {
            Ok((_, out, err)) => {
                let t = format!("{}{}", out, err).trim().to_string();
                if t.is_empty() { "(no vault hits)".into() } else { t }
            }
            Err(e) => format!("brain search failed: {}", e),
        };
    }
    if sh.mode() == 0 {
        return "remember is unavailable in ask mode (read-only)".into();
    }
    if let Err(e) = sh.approve("brain", &format!("kind={}", s_arg(args, "kind")), "writes to the vault") {
        return e;
    }
    let kind = {
        let k = s_arg(args, "kind");
        if k.is_empty() { "reference".to_string() } else { k }
    };
    match run(
        &config::brain_bin(),
        &["remember", &text, "--kind", &kind, "--agent", "superagent-rs"],
        &sh_cwd(),
        Duration::from_secs(240),
    ) {
        Ok((_, out, err)) => {
            let t = format!("{}{}", out, err).trim().to_string();
            if t.is_empty() { "(remembered)".into() } else { t }
        }
        Err(e) => format!("brain remember failed: {}", e),
    }
}

fn t_agent(args: &Value, sh: &Shared) -> String {
    let name = s_arg(args, "name");
    let prompt = s_arg(args, "prompt");
    let defs = config::agents();
    let Some(def) = defs.iter().find(|d| d.name == name) else {
        return format!("agent {:?} not installed", name);
    };
    if def.path.is_empty() {
        return format!("agent {:?} not installed", name);
    }
    if !def.headless {
        return format!("{} is interactive-only (TUI); use claude/codex/pi instead", name);
    }
    if let Err(e) = sh.approve("agent", &format!("name={} prompt={}", name, prompt.chars().take(60).collect::<String>()), "spawns another agent") {
        return e;
    }
    let cwd = {
        let a = s_arg(args, "cwd");
        if a.is_empty() { sh.cwd.lock().unwrap().clone() } else { expand(&a) }
    };
    let timeout = n_arg(args, "timeout", 600).min(1800);
    let mut cmd = Command::new(&def.path);
    match name.as_str() {
        "claude" => {
            cmd.arg("-p").arg(&prompt);
        }
        "codex" => {
            cmd.arg("exec").arg(&prompt);
        }
        _ => {
            cmd.arg("-p").arg(&prompt);
        }
    }
    sh.set_agent(&name, "working", None);
    let started = Instant::now();
    cmd.current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            sh.set_agent(&name, "failed", Some("spawn"));
            return format!("{} failed to start: {}", name, e);
        }
    };
    let mut out = child.stdout.take().unwrap();
    let mut err = child.stderr.take().unwrap();
    let oh = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        String::from_utf8_lossy(&b).to_string()
    });
    let eh = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        String::from_utf8_lossy(&b).to_string()
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {
                if started.elapsed() > Duration::from_secs(timeout) {
                    let _ = child.kill();
                    let _ = child.wait();
                    sh.set_agent(&name, "failed", Some("timeout"));
                    return format!("{} timed out after {}s", name, timeout);
                }
                std::thread::sleep(Duration::from_millis(80));
            }
            Err(e) => return format!("{} wait failed: {}", name, e),
        }
    };
    let stdout = oh.join().unwrap_or_default();
    let stderr = eh.join().unwrap_or_default();
    let secs = started.elapsed().as_secs();
    sh.set_agent(
        &name,
        if status.success() { "done" } else { "failed" },
        Some(&format!("{}s", secs)),
    );
    let body = if stdout.trim().is_empty() { stderr } else { stdout };
    format!(
        "{} ({}, {}s, exit {}):\n{}",
        name,
        Path::new(&cwd).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or(cwd.clone()),
        secs,
        status.code().unwrap_or(-1),
        if body.trim().is_empty() { "(empty)" } else { body.trim() }
    )
}

fn t_job(args: &Value) -> String {
    let path = config::sg_base().join("system.json");
    let Ok(text) = fs::read_to_string(path) else {
        return "no system graph yet - run scan".into();
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return "system.json unreadable".into();
    };
    let empty = vec![];
    let nodes = v.get("nodes").and_then(|n| n.as_array()).unwrap_or(&empty);
    let edges = v.get("edges").and_then(|n| n.as_array()).unwrap_or(&empty);
    let jobs: Vec<&Value> = nodes
        .iter()
        .filter(|n| n.get("kind").and_then(|k| k.as_str()) == Some("job"))
        .collect();
    let scripts_for = |id: &str| -> String {
        edges
            .iter()
            .filter(|e| {
                e.get("source").and_then(|s| s.as_str()) == Some(id)
                    && e.get("rel").and_then(|r| r.as_str()) == Some("runs")
            })
            .filter_map(|e| e.get("target").and_then(|t| t.as_str()))
            .map(|t| t.split(':').last().unwrap_or(t).to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    if s_arg(args, "action") == "show" {
        let want = s_arg(args, "name").to_lowercase();
        let mut out = Vec::new();
        for j in jobs {
            let lbl = j.get("label").and_then(|l| l.as_str()).unwrap_or("");
            if want.is_empty() || lbl.to_lowercase().contains(&want) {
                let id = j.get("id").and_then(|i| i.as_str()).unwrap_or("");
                out.push(format!(
                    "{}\n  plist={}\n  interval={}s calendar={} disabled={}\n  runs={}",
                    lbl,
                    j.get("plist").and_then(|p| p.as_str()).unwrap_or("-"),
                    j.get("interval").map(|i| i.to_string()).unwrap_or("-".into()),
                    j.get("calendar").map(|c| c.to_string()).unwrap_or("-".into()),
                    j.get("disabled").map(|c| c.to_string()).unwrap_or("-".into()),
                    scripts_for(id)
                ));
            }
        }
        return if out.is_empty() { format!("no job matching {:?}", want) } else { out.join("\n") };
    }
    let mut rows: Vec<String> = jobs
        .iter()
        .map(|j| {
            let id = j.get("id").and_then(|i| i.as_str()).unwrap_or("");
            format!(
                "{:<42} every {:<6} {}",
                j.get("label").and_then(|l| l.as_str()).unwrap_or(""),
                j.get("interval").map(|i| i.to_string()).unwrap_or("-".into()),
                {
                    let s = scripts_for(id);
                    if s.is_empty() { "-".into() } else { s }
                }
            )
        })
        .collect();
    rows.sort();
    rows.join("\n")
}

fn t_plan(args: &Value, sh: &Shared) -> String {
    if let Some(items) = args.get("items").and_then(|i| i.as_array()) {
        let mut plan = sh.plan.lock().unwrap();
        plan.clear();
        for i in items {
            if let Some(s) = i.as_str() {
                plan.push(s.to_string());
            }
        }
    }
    let plan = sh.plan.lock().unwrap();
    if plan.is_empty() {
        return "(empty plan)".into();
    }
    plan.iter()
        .enumerate()
        .map(|(i, s)| format!("{}. {}", i + 1, s))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn approve_blocked(s: &str) -> bool {
    s.starts_with("error")
        || s.starts_with("denied")
        || s.starts_with("blocked")
        || s.starts_with("unavailable")
        || s.starts_with("refused")
        || s.starts_with("declined")
}
