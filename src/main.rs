//! sa - DeepSeek superagent for this machine. Native Rust build of the terminal UI.
mod agent;
mod config;
mod llm;
mod prompt;
mod tools;
mod ui;
mod util;

use agent::{Agent, Ev};
use std::sync::Arc;

const DIM: &str = "\x1b[2m";
const OFF: &str = "\x1b[0m";

struct Args {
    prompt: Vec<String>,
    print: bool,
    classic: bool,
    mode: usize,
    model: String,
    effort: String,
    ask: bool,
    no_sidebar: bool,
    think: bool,
    cwd: Option<String>,
    steps: u32,
    no_color: bool,
    check: bool,
    map: bool,
    agents: bool,
    sessions: bool,
    selftest: bool,
    no_full: bool,
    show_prompt: bool,
}

impl Default for Args {
    fn default() -> Self {
        Args {
            prompt: vec![],
            print: false,
            classic: false,
            mode: 1,
            model: std::env::var("SA_MODEL").unwrap_or_else(|_| "deepseek-flash".into()),
            effort: std::env::var("SA_EFFORT").unwrap_or_else(|_| "high".into()),
            ask: false,
            no_sidebar: false,
            think: false,
            cwd: None,
            steps: 40,
            no_color: false,
            check: false,
            map: false,
            agents: false,
            sessions: false,
            selftest: false,
            no_full: false,
            show_prompt: false,
        }
    }
}

fn parse() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1).peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-p" | "--print" => a.print = true,
            "--classic" => a.classic = true,
            "--mode" => {
                let v = it.next().ok_or("--mode needs a value")?;
                a.mode = match v.as_str() {
                    "ask" => 0,
                    "agent" => 1,
                    "dispatch" => 2,
                    "system" => 3,
                    other => return Err(format!("unknown mode {:?}", other)),
                };
            }
            "--model" => a.model = it.next().ok_or("--model needs a value")?,
            "--effort" => a.effort = it.next().ok_or("--effort needs a value")?,
            "--ask" | "--safe" => a.ask = true,
            "--no-sidebar" => a.no_sidebar = true,
            "--think" => a.think = true,
            "--cwd" => a.cwd = Some(it.next().ok_or("--cwd needs a value")?),
            "--steps" => {
                a.steps = it
                    .next()
                    .ok_or("--steps needs a value")?
                    .parse()
                    .map_err(|_| "--steps must be a number")?
            }
            "--no-color" => a.no_color = true,
            "--check" => a.check = true,
            "--map" => a.map = true,
            "--agents" => a.agents = true,
            "--sessions" => a.sessions = true,
            "--selftest-ui" => a.selftest = true,
            "--no-full" => a.no_full = true,
            "--show-prompt" => a.show_prompt = true,
            "-h" | "--help" => return Err("help".into()),
            other => a.prompt.push(other.to_string()),
        }
    }
    if a.prompt.first().map(|s| s == "--").unwrap_or(false) {
        a.prompt.remove(0);
    }
    Ok(a)
}

fn help() {
    println!(
        "sa - DeepSeek superagent for this machine

usage
  sa                          full-screen UI (default)
  sa \"question\"              one-shot answer
  sa -p \"question\"           one-shot, non-interactive
  sa --classic                plain line REPL
  sa --mode ask|agent|dispatch|system
  sa --check | --map | --agents | --sessions
  sa --model deepseek-v4-pro --effort high
  sa --ask                    confirm writes and shell (yolo is the default)
  sa --no-sidebar --think

modes
  ask       read-only: graph, files, vault search
  agent     full operator, auto-approved (default)
  dispatch  plan and hand work to claude / codex / pi
  system    map keeper: graph, roots, jobs, wiring

inside the UI
  ⏎ send · F1-F4 mode · F5 sidebar · ^C stop · ^L clear · ^T reasoning · ^N new · ^D quit
  /help /map /roots /graph /scan /build /jobs /agents /cost /model /mode /think /new /session

state
  ~/.systemgraph/registry.json   graph roots
  ~/.systemgraph/sessions/       transcripts
  key from DEEPSEEK_API_KEY or ~/.pi/agent/auth.json"
    );
}

fn check() -> i32 {
    let mut ok = true;
    match config::api_key() {
        Some(_) => {
            println!("deepseek key   present");
            let c = match llm::Client::new("deepseek-flash", "high") {
                Ok(c) => c,
                Err(e) => {
                    println!("client         {}", e);
                    return 1;
                }
            };
            let models = c.models();
            println!("models         {}", if models.is_empty() { "(could not list)".into() } else { models.join(", ") });
            ok = ok && !models.is_empty();
        }
        None => {
            println!("deepseek key   MISSING");
            ok = false;
        }
    }
    let roots = config::roots();
    println!(
        "graph roots    {} ({})",
        roots.len(),
        roots.iter().map(|r| r.name.clone()).collect::<Vec<_>>().join(", ")
    );
    let (n, e, j) = config::system_counts();
    println!("system graph   {} nodes, {} edges, {} jobs", n, e, j);
    for a in config::agents() {
        println!("agent {:<9} {}", a.name, if a.path.is_empty() { "no" } else { "yes" });
    }
    println!("ui             ratatui + crossterm");
    if ok { 0 } else { 1 }
}

fn main() {
    // behave like a normal unix tool when stdout is a closed pipe (sa --check | head)
    unsafe {
        extern "C" {
            fn signal(sig: i32, handler: usize) -> usize;
        }
        signal(13, 0); // SIGPIPE -> default
    }
    let a = match parse() {
        Ok(a) => a,
        Err(e) => {
            if e == "help" {
                help();
                std::process::exit(0);
            }
            eprintln!("error: {}", e);
            std::process::exit(2);
        }
    };
    if a.check {
        std::process::exit(check());
    }
    if a.show_prompt {
        println!("{}", prompt::build(a.mode));
        return;
    }
    if a.map {
        let sh = Arc::new(tools::Shared::new(&a.model, a.mode, "auto", &cwd(&a)));
        println!("{}", tools::execute("system_map", &serde_json::json!({"wiring": true}), &sh));
        return;
    }
    if a.agents {
        for d in config::agents() {
            println!(
                "{:<9} {:<6} {:<10} {}",
                d.name,
                if d.path.is_empty() { "no" } else { "yes" },
                if d.headless { "headless" } else { "tui-only" },
                d.path
            );
        }
        return;
    }
    if a.sessions {
        list_sessions();
        return;
    }

    let approver = if a.ask {
        "ask"
    } else if a.mode == 0 {
        "auto"
    } else {
        "yolo"
    };
    let shared = Arc::new(tools::Shared::new(&a.model, a.mode, approver, &cwd(&a)));
    let agent = match Agent::new(&a.model, &a.effort, shared.clone(), a.steps) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: {}", e);
            std::process::exit(1);
        }
    };

    if a.selftest {
        return selftest_ui(agent, shared.clone());
    }

    let interactive = a.prompt.is_empty() && !a.print;
    let tty = isatty();
    if interactive && !a.classic && tty {
        if !a.no_full && std::env::var("SA_NO_MAXIMIZE").is_err() {
            std::thread::sleep(std::time::Duration::from_millis(220));
            maximize_window();
        }
        let app = ui::Ui::new(agent, shared.clone()).with_sidebar(!a.no_sidebar).with_think(a.think);
        if let Err(e) = app.run() {
            eprintln!("ui error: {}", e);
            std::process::exit(1);
        }
        let s = shared.stats_snapshot();
        println!(
            "{}→ session · {} steps · {} in / {} out · ${:.4}{}",
            DIM, s.steps, util::kfmt(s.input), util::kfmt(s.output), s.cost, OFF
        );
        return;
    }

    if interactive {
        classic(agent, shared.clone(), a.no_color);
        return;
    }

    // one-shot
    let text = if a.prompt.is_empty() {
        use std::io::Read;
        let mut s = String::new();
        let _ = std::io::stdin().read_to_string(&mut s);
        s
    } else {
        a.prompt.join(" ")
    };
    if text.trim().is_empty() {
        eprintln!("empty prompt");
        std::process::exit(2);
    }
    let color = !a.no_color && tty;
    let mut agent = agent;
    let mut emit = |ev: Ev| match ev {
        Ev::Content(t) => {
            print!("{}", t);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
        Ev::Reasoning(t) if a.think => {
            print!("\x1b[2m{}\x1b[0m", t);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
        Ev::Tool { name, args } => {
            if color {
                eprintln!("\x1b[38;5;209m⏺ {}\x1b[0m", util::call_brief(&name, &args));
            }
        }
        Ev::Result { summary, ok, .. } => {
            if color {
                eprintln!("\x1b[{}m  ← {}\x1b[0m", if ok { "32" } else { "31" }, summary);
            }
        }
        Ev::Error(e) => eprintln!("\x1b[31m  ! {}\x1b[0m", e),
        Ev::Done(_) => println!(),
        Ev::Stats(_) | Ev::Idle | Ev::Status(_) | Ev::Mode(_) | Ev::Session(_) | Ev::Reasoning(_) => {}
    };
    let answer = agent.ask(&text, &mut emit);
    let c = agent.total_cost();
    eprintln!(
        "\n{}steps {} · {} in / {} out · ${:.4}{}",
        DIM,
        shared.stats_snapshot().steps,
        util::kfmt(agent.usage.input),
        util::kfmt(agent.usage.output),
        c,
        OFF
    );
    let bad = answer.is_empty() || answer.starts_with('(');
    std::process::exit(if bad { 1 } else { 0 });
}

fn cwd(a: &Args) -> String {
    a.cwd.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().into())
            .unwrap_or_else(|_| "/".into())
    })
}

/// Fill the Terminal window with the app. Safe to call repeatedly: it sets bounds, never toggles.
pub fn maximize_window() -> bool {
    let term = std::env::var("TERM_PROGRAM").unwrap_or_default();
    if term != "Apple_Terminal" {
        return false;
    }
    let script = "tell application \"Finder\" to set b to bounds of window of desktop\n\
                  tell application \"Terminal\" to set bounds of front window to b";
    std::process::Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn isatty() -> bool {
    // cheap isatty without a libc dependency
    extern "C" {
        fn isatty(fd: i32) -> i32;
    }
    unsafe { isatty(1) == 1 }
}

fn list_sessions() {
    let dir = config::sessions_dir();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).collect())
        .unwrap_or_else(|_| vec![]);
    entries.sort_by_key(|e| std::cmp::Reverse(e.path()));
    for (i, e) in entries.iter().enumerate() {
        if i >= 20 {
            break;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if let Some(id) = name.strip_suffix(".jsonl") {
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            println!("{}  {:6.1}KB", id, size as f64 / 1024.0);
        }
    }
}

fn classic(mut agent: Agent, shared: Arc<tools::Shared>, no_color: bool) {
    let color = !no_color;
    let roots = config::roots().len();
    println!(
        "\x1b[1mSUPERAGENT\x1b[0m  {} · {} mode · {} roots",
        agent.model_name(),
        tools::MODES[shared.mode()].to_uppercase(),
        roots
    );
    println!("\x1b[2m{} · cwd {}\x1b[0m", agent.session_id, shared.cwd.lock().unwrap());
    println!("\x1b[2m/help for commands, /exit to quit\x1b[0m\n");
    let _ = color;
    let mut line = String::new();
    loop {
        use std::io::{BufRead, Write};
        print!("\x1b[36msa>\x1b[0m ");
        let _ = std::io::stdout().flush();
        line.clear();
        if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
            println!();
            return;
        }
        let text = line.trim().to_string();
        if text.is_empty() {
            continue;
        }
        if text.starts_with('/') {
            let mut parts = text.splitn(2, ' ');
            let cmd = parts.next().unwrap_or("");
            let rest = parts.next().unwrap_or("").trim();
            match cmd {
                "/exit" | "/quit" | "/q" => return,
                "/help" => println!("  /map /roots /scan /build /graph /agents /jobs /cost /model /mode /think /new /session /exit"),
                "/mode" => {
                    if let Some(i) = tools::MODES.iter().position(|m| *m == rest) {
                        shared.set_mode(i);
                        println!("  mode -> {}", rest.to_uppercase());
                    } else {
                        println!("  modes: ask | agent | dispatch | system");
                    }
                }
                "/map" => println!("{}", tools::execute("system_map", &serde_json::json!({"wiring": true}), &shared)),
                "/roots" => {
                    for r in config::roots() {
                        println!("  {:<18} {:<50} {:>7} nodes", r.name, r.path, r.nodes);
                    }
                }
                "/agents" => {
                    for d in config::agents() {
                        println!("  {:<9} {:<6} {}", d.name, if d.path.is_empty() { "no" } else { "yes" }, if d.headless { "headless" } else { "tui-only" });
                    }
                }
                "/graph" => println!("{}", tools::execute("graph", &serde_json::json!({"question": rest}), &shared)),
                "/scan" => println!("{}", tools::execute("scan", &serde_json::json!({}), &shared)),
                "/build" => println!(
                    "{}",
                    tools::execute(
                        "graph_build",
                        &serde_json::json!({"root": rest, "all": rest == "all" || rest.is_empty()}),
                        &shared
                    )
                ),
                "/jobs" => println!("{}", tools::execute("job", &serde_json::json!({"action":"list"}), &shared)),
                "/cost" => {
                    let s = shared.stats_snapshot();
                    println!("  {} steps · {} in / {} out · ${:.4}", s.steps, util::kfmt(s.input), util::kfmt(s.output), s.cost);
                }
                "/model" => {
                    if rest.is_empty() {
                        println!("  {}", agent.model_name());
                    } else if let Err(e) = agent.set_model(rest) {
                        println!("  error: {}", e);
                    } else {
                        println!("  model -> {}", rest);
                    }
                }
                "/think" => println!("  (thinking display is on in the UI; -p --think in one-shot)"),
                "/session" => println!("  {}\n  {}", agent.session_id, agent.session_path.display()),
                _ => println!("  unknown command"),
            }
            continue;
        }
        if text.starts_with('!') {
            match tools::bash(&text[1..], &shared.cwd.lock().unwrap().clone(), std::time::Duration::from_secs(300)) {
                Ok((_, o, e)) => println!("{}{}", o, e),
                Err(e) => println!("error: {}", e),
            }
            continue;
        }
        let mut emit = |ev: Ev| match ev {
            Ev::Content(t) => {
                print!("{}", t);
                let _ = std::io::stdout().flush();
            }
            Ev::Tool { name, args } => println!("\x1b[38;5;209m⏺ {}\x1b[0m", util::call_brief(&name, &args)),
            Ev::Result { summary, ok, .. } => println!("\x1b[{}m  ← {}\x1b[0m", if ok { "32" } else { "31" }, summary),
            Ev::Error(e) => println!("\x1b[31m  ! {}\x1b[0m", e),
            Ev::Done(_) => println!(),
            _ => {}
        };
        agent.ask(&text, &mut emit);
        let s = shared.stats_snapshot();
        println!("\x1b[2m{} steps · {} in / {} out · ${:.4}\x1b[0m\n", s.steps, util::kfmt(s.input), util::kfmt(s.output), s.cost);
    }
}

/// Render one frame into an offscreen 120x36 buffer and dump it as text.
/// Proves the layout (top bar, transcript, status, input, hint, sidebar) without a terminal.
fn selftest_ui(agent: Agent, shared: Arc<tools::Shared>) {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    let mut app = ui::Ui::new(agent, shared.clone());
    app.seed_demo();
    let backend = TestBackend::new(120, 36);
    let mut term = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("terminal init failed: {}", e);
            std::process::exit(1);
        }
    };
    if let Err(e) = term.draw(|f| app.draw(f)) {
        eprintln!("draw failed: {}", e);
        std::process::exit(1);
    }
    let buf = term.backend().buffer().clone();
    let area = *buf.area();
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        println!("|{}|", line.trim_end());
    }
}
