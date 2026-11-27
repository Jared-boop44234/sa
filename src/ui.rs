use crate::agent::{Agent, Ev};
use crate::config;
use crate::prompt;
use crate::tools::{self, Shared, Stats, MODES};
use crate::util;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub enum Cmd {
    Ask(String),
    SetMode(usize),
    SetModel(String),
    NewSession,
}

const SPINNER: [&str; 10] = ["✻", "✽", "✶", "✳", "✢", "·", "✢", "✳", "✶", "✽"];
/// Claude Code's warm accent, used for the mark, the spinner and the active mode.
const ACCENT: Color = Color::Rgb(215, 119, 87);
const ACCENT_DIM: Color = Color::Rgb(150, 84, 62);
const VERBS: [&str; 9] = [
    "Thinking", "Pondering", "Noodling", "Percolating", "Mulling",
    "Synthesizing", "Ruminating", "Considering", "Working",
];

struct Seg {
    text: String,
    style: Style,
}

pub struct Ui {
    tx: Sender<Cmd>,
    rx: Receiver<Ev>,
    shared: Arc<Shared>,
    segments: Vec<Seg>,
    stream: String,
    wrapped: Vec<Line<'static>>,
    wrap_width: u16,
    input: String,
    cursor: usize,
    history: Vec<String>,
    hist_idx: usize,
    scroll: usize,
    busy: bool,
    status: String,
    spinner: usize,
    show_side: bool,
    show_think: bool,
    thinking: String,
    started: Instant,
    session_id: String,
    model: String,
    stats: Stats,
    quit: bool,
    help: bool,
    busy_since: Instant,
}

impl Ui {
    pub fn new(agent: Agent, shared: Arc<Shared>) -> Ui {
        let (ctx, rx) = channel::<Ev>();
        let (tx, cmd_rx) = channel::<Cmd>();
        let model = agent.model_name();
        // the runtime thread owns the Agent for its whole life
        std::thread::spawn(move || {
            let mut agent = agent;
            let ctx = ctx;
            while let Ok(cmd) = cmd_rx.recv() {
                match cmd {
                    Cmd::Ask(text) => {
                        let session = format!("#{}", agent.session_id);
                        let _ = ctx.send(Ev::Session(session));
                        let mut emit = |ev: Ev| {
                            let _ = ctx.send(ev);
                        };
                        agent.ask(&text, &mut emit);
                    }
                    Cmd::SetMode(m) => {
                        let mut emit = |ev: Ev| {
                            let _ = ctx.send(ev);
                        };
                        agent.shared.set_mode(m);
                        emit(Ev::Mode(m));
                    }
                    Cmd::SetModel(m) => {
                        let _ = agent.set_model(&m);
                        let _ = ctx.send(Ev::Stats(agent.shared.stats_snapshot()));
                    }
                    Cmd::NewSession => {
                        let model = agent.client.model.clone();
                        let effort = agent.client.effort.clone();
                        let max_steps = agent.max_steps;
                        if let Ok(fresh) = Agent::new(&model, &effort, agent.shared.clone(), max_steps) {
                            agent = fresh;
                        }
                        let _ = ctx.send(Ev::Session(format!("#{}", agent.session_id)));
                    }
                }
            }
        });

        Ui {
            tx,
            rx,
            shared,
            segments: vec![],
            stream: String::new(),
            wrapped: vec![],
            wrap_width: 0,
            input: String::new(),
            cursor: 0,
            history: vec![],
            hist_idx: 0,
            scroll: 0,
            busy: false,
            status: "ready".into(),
            spinner: 0,
            show_side: true,
            show_think: false,
            thinking: String::new(),
            started: Instant::now(),
            session_id: String::new(),
            model,
            stats: Stats::default(),
            quit: false,
            help: false,
            busy_since: Instant::now(),
        }
    }

    /// Fill the buffer with representative content so a TestBackend frame shows the layout.
    pub fn seed_demo(&mut self) {
        self.wrap_width = 84;
        self.push_seg(
            "✻ SUPERAGENT ready · 3 roots · /Users/you/code",
            Style::default().fg(Color::DarkGray),
        );
        self.push_wrapped(
            "what does the ingest service do when the queue backs up",
            Style::default().fg(Color::Rgb(220, 220, 220)),
            "> ",
            "  ",
        );
        self.push_seg("", Style::default());
        self.push_seg("⏺ graph(ingest service queue backpressure)", Style::default().fg(ACCENT));
        self.push_seg("    ⎿ ## System graph (146 nodes)", Style::default().fg(Color::Green));
        self.push_seg("⏺ read(src/ingest/worker.rs)", Style::default().fg(ACCENT));
        self.push_seg("  ⎿  src/ingest/worker.rs (212 lines)", Style::default().fg(ACCENT_DIM));
        self.push_seg(
            "⏺ The worker retries with backoff, then parks the batch in the dead-letter",
            Style::default().fg(ACCENT),
        );
        self.push_seg("  table and emits a queue_depth metric.", Style::default());
        self.push_seg("", Style::default());
        self.push_seg(
            "  4 steps · 15.6k in / 623 out · $0.0022",
            Style::default().fg(Color::DarkGray),
        );
        self.stats = Stats { steps: 4, input: 15600, output: 623, cost: 0.0022, model: "deepseek-flash".into() };
        self.session_id = "#20261126-204500".into();
        self.status = "ready".into();
        {
            let mut s = self.shared.statuses.lock().unwrap();
            for a in s.iter_mut() {
                if a.name == "pi" {
                    a.status = "working";
                }
                if a.name == "codex" {
                    a.status = "done";
                    a.last = "42s".into();
                }
            }
        }
        self.wrapped.clear();
    }

    pub fn with_sidebar(mut self, yes: bool) -> Self {
        self.show_side = yes;
        self
    }

    pub fn with_think(mut self, yes: bool) -> Self {
        self.show_think = yes;
        self
    }

    // -- buffer ------------------------------------------------------------
    fn push_seg(&mut self, text: impl Into<String>, style: Style) {
        self.segments.push(Seg { text: text.into(), style });
        self.scroll = 0;
        self.wrapped.clear();
    }

    fn push_block(&mut self, text: &str, style: Style) {
        for line in text.split('\n') {
            self.segments.push(Seg { text: line.to_string(), style });
        }
        self.scroll = 0;
        self.wrapped.clear();
    }

    fn push_wrapped(&mut self, text: &str, style: Style, first: &str, rest: &str) {
        let width = self.wrap_width.max(40) as usize;
        let mut first_line = true;
        for para in text.split('\n') {
            let limit = if first_line { width.saturating_sub(first.len() + 1) } else { width.saturating_sub(rest.len()) };
            let chunks = wrap(para, limit.max(20));
            for chunk in chunks {
                let prefix = if first_line { first } else { rest };
                self.segments.push(Seg { text: format!("{}{}", prefix, chunk), style });
                first_line = false;
            }
        }
        self.scroll = 0;
        self.wrapped.clear();
    }

    fn flush_stream(&mut self) {
        let text = std::mem::take(&mut self.stream);
        if !text.trim().is_empty() {
            let width = self.wrap_width.max(40) as usize;
            let mut first = true;
            for para in text.split('\n') {
                for chunk in wrap(para, width.saturating_sub(3)) {
                    let (prefix, style) = if first {
                        first = false;
                        ("⏺ ", Style::default().fg(ACCENT))
                    } else {
                        ("  ", Style::default())
                    };
                    self.segments.push(Seg {
                        text: format!("{}{}", prefix, chunk),
                        style,
                    });
                }
            }
        }
        self.wrapped.clear();
    }

    fn rewrap(&mut self) {
        let width = self.wrap_width.max(20) as usize;
        let mut lines: Vec<Line<'static>> = Vec::with_capacity(self.segments.len());
        for seg in &self.segments {
            if seg.text.is_empty() {
                lines.push(Line::from(""));
                continue;
            }
            for chunk in wrap(&seg.text, width) {
                lines.push(Line::from(Span::styled(chunk, seg.style)));
            }
        }
        self.wrapped = lines;
    }

    // -- events ------------------------------------------------------------
    fn drain(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(Ev::Content(t)) => {
                    self.stream.push_str(&t);
                }
                Ok(Ev::Reasoning(t)) => self.thinking.push_str(&t),
                Ok(Ev::Status(s)) => {
                    self.status = s.clone();
                    if s == "thinking" || s == "tools" {
                        self.thinking.clear();
                    }
                }
                Ok(Ev::Tool { name, args }) => {
                    self.flush_stream();
                    self.push_seg(
                        format!("⏺ {}", util::call_brief(&name, &args)),
                        Style::default().fg(ACCENT),
                    );
                }
                Ok(Ev::Result { summary, ok, .. }) => {
                    let color = if ok { ACCENT_DIM } else { Color::Red };
                    self.push_seg(format!("  ⎿  {}", summary), Style::default().fg(color));
                }
                Ok(Ev::Error(e)) => {
                    self.flush_stream();
                    self.push_seg(format!("⏺ ! {}", e), Style::default().fg(Color::Red));
                }
                Ok(Ev::Done(_)) => {
                    self.flush_stream();
                    self.push_seg("", Style::default());
                }
                Ok(Ev::Stats(s)) => self.stats = s,
                Ok(Ev::Idle) => {
                    self.busy = false;
                    self.status = "ready".into();
                    self.flush_stream();
                    let s = self.stats.clone();
                    self.push_seg(
                        format!(
                            "  {} steps · {} in / {} out · ${:.4}",
                            s.steps,
                            util::kfmt(s.input),
                            util::kfmt(s.output),
                            s.cost
                        ),
                        Style::default().fg(Color::DarkGray),
                    );
                    self.push_seg("", Style::default());
                }
                Ok(Ev::Mode(_)) => {}
                Ok(Ev::Session(id)) => self.session_id = id,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    // -- input -------------------------------------------------------------
    fn submit(&mut self) {
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        if text == "/exit" || text == "/quit" || text == "/q" {
            self.quit = true;
            return;
        }
        self.history.push(text.clone());
        self.hist_idx = self.history.len();
        self.push_seg("", Style::default());
        self.push_wrapped(&text, Style::default().fg(Color::Rgb(220, 220, 220)), "> ", "  ");
        self.push_seg("", Style::default());
        if text.starts_with('!') {
            let out = tools::bash(&text[1..], &self.cwd(), Duration::from_secs(300));
            match out {
                Ok((_, o, e)) => {
                    let body = format!("{}{}", o, e);
                    self.push_block(if body.trim().is_empty() { "(no output)" } else { body.trim() }, Style::default().fg(Color::DarkGray));
                }
                Err(e) => self.push_seg(format!("  ! {}", e), Style::default().fg(Color::Red)),
            }
            return;
        }
        if text.starts_with('/') {
            self.slash(&text);
            return;
        }
        if self.busy {
            self.push_seg("  · still working - ^C to stop", Style::default().fg(Color::Yellow));
            return;
        }
        self.busy = true;
        self.busy_since = Instant::now();
        self.status = "thinking".into();
        self.thinking.clear();
        self.shared.cancel.store(false, Ordering::SeqCst);
        let _ = self.tx.send(Cmd::Ask(text));
    }

    fn cwd(&self) -> String {
        self.shared.cwd.lock().unwrap().clone()
    }

    fn slash(&mut self, text: &str) {
        let mut parts = text.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("").to_lowercase();
        let rest = parts.next().unwrap_or("").trim().to_string();
        match cmd.as_str() {
            "/help" => self.help = true,
            "/mode" => {
                if let Some(i) = MODES.iter().position(|m| *m == rest) {
                    self.set_mode(i);
                } else {
                    self.push_seg("  modes: ask | agent | dispatch | system  (F1-F4)", Style::default().fg(Color::DarkGray));
                }
            }
            "/map" => {
                let out = tools::execute("system_map", &serde_json::json!({"wiring": true}), &self.shared);
                self.push_block(&out, Style::default());
            }
            "/roots" => {
                let roots = config::roots();
                let body = roots
                    .iter()
                    .map(|r| format!("  {:<18} {:<50} {:>7} nodes", r.name, r.path, r.nodes))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.push_block(if body.is_empty() { "(none)" } else { &body }, Style::default());
            }
            "/agents" => {
                let body = config::agents()
                    .iter()
                    .map(|a| {
                        format!(
                            "  {} {:<9} {}",
                            if a.path.is_empty() { "·" } else { "●" },
                            a.name,
                            if a.path.is_empty() { "absent" } else if a.headless { "headless" } else { "tui-only" }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.push_block(&body, Style::default());
            }
            "/graph" => {
                if rest.is_empty() {
                    self.push_seg("  usage: /graph <question>", Style::default().fg(Color::Yellow));
                } else {
                    let out = tools::execute("graph", &serde_json::json!({"question": rest}), &self.shared);
                    self.push_block(&out, Style::default());
                }
            }
            "/scan" => {
                let out = tools::execute("scan", &serde_json::json!({}), &self.shared);
                self.push_block(&out, Style::default());
            }
            "/build" => {
                let out = tools::execute(
                    "graph_build",
                    &serde_json::json!({"root": rest, "all": rest == "all" || rest.is_empty()}),
                    &self.shared,
                );
                self.push_block(&out, Style::default());
            }
            "/jobs" => {
                let out = tools::execute("job", &serde_json::json!({"action": "list"}), &self.shared);
                self.push_block(&out, Style::default());
            }
            "/cost" => {
                let s = &self.stats;
                self.push_seg(
                    format!("  {} steps · {} in / {} out · ${:.4}", s.steps, util::kfmt(s.input), util::kfmt(s.output), s.cost),
                    Style::default().fg(Color::DarkGray),
                );
            }
            "/model" => {
                if rest.is_empty() {
                    self.push_seg(format!("  {}", self.model), Style::default().fg(Color::DarkGray));
                } else {
                    self.model = rest.clone();
                    let _ = self.tx.send(Cmd::SetModel(rest));
                    self.push_seg(format!("  · model → {}", self.model), Style::default().fg(Color::Magenta));
                }
            }
            "/think" => {
                self.show_think = !self.show_think;
                self.push_seg(
                    format!("  · reasoning display {}", if self.show_think { "on" } else { "off" }),
                    Style::default().fg(Color::Magenta),
                );
            }
            "/new" => {
                let _ = self.tx.send(Cmd::NewSession);
                self.segments.clear();
                self.wrapped.clear();
                self.push_seg("  · new session", Style::default().fg(Color::Magenta));
            }
            "/session" => {
                self.push_seg(format!("  {}", self.session_id), Style::default().fg(Color::DarkGray));
                self.push_seg("  ~/.systemgraph/sessions/", Style::default().fg(Color::DarkGray));
            }
            "/cd" => {
                if rest.is_empty() {
                    let here = self.cwd();
                    self.push_seg(format!("  cwd {}", here), Style::default().fg(Color::DarkGray));
                } else {
                    let target = if rest.starts_with('~') {
                        rest.replacen('~', &config::home().to_string_lossy(), 1)
                    } else {
                        rest.clone()
                    };
                    if std::path::Path::new(&target).is_dir() {
                        *self.shared.cwd.lock().unwrap() = target.clone();
                        self.push_seg(format!("  · cwd → {}", target), Style::default().fg(Color::Magenta));
                    } else {
                        self.push_seg(format!("  not a directory: {}", target), Style::default().fg(Color::Red));
                    }
                }
            }
            "/full" => {
                let ok = crate::maximize_window();
                self.push_seg(
                    if ok { "  · window maximised" } else { "  · maximise unavailable (not Terminal.app)" },
                    Style::default().fg(Color::Magenta),
                );
            }
            "/clear" => {
                self.segments.clear();
                self.wrapped.clear();
            }
            "/exit" => self.quit = true,
            other => {
                self.push_seg(format!("  unknown command {} - /help", other), Style::default().fg(Color::Yellow));
            }
        }
    }

    fn set_mode(&mut self, i: usize) {
        self.shared.set_mode(i);
        let _ = self.tx.send(Cmd::SetMode(i));
        self.push_seg(
            format!(
                "  · mode → {}{}",
                MODES[i].to_uppercase(),
                if i == 0 { "  (read-only)" } else { "" }
            ),
            Style::default().fg(Color::Magenta),
        );
    }

    fn key(&mut self, k: KeyEvent) {
        if k.kind != KeyEventKind::Press && k.kind != KeyEventKind::Repeat {
            return;
        }
        if self.shared.ask.active.load(Ordering::SeqCst) {
            match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.shared.answer_ask(true);
                    self.push_seg("  · approval given", Style::default().fg(Color::Green));
                }
                _ => {
                    self.shared.answer_ask(false);
                    self.push_seg("  · approval declined", Style::default().fg(Color::Red));
                }
            }
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::F(n) if (1..=4).contains(&n) => self.set_mode((n - 1) as usize),
            KeyCode::F(5) => self.show_side = !self.show_side,
            KeyCode::Char('c') if ctrl => {
                if self.busy {
                    self.shared.cancel.store(true, Ordering::SeqCst);
                    self.push_seg("  · stopping…", Style::default().fg(Color::Yellow));
                    self.busy = false;
                    self.status = "ready".into();
                } else {
                    self.quit = true;
                }
            }
            KeyCode::Char('d') if ctrl => {
                if self.input.is_empty() {
                    self.quit = true;
                }
            }
            KeyCode::Char('l') if ctrl => {
                self.segments.clear();
                self.wrapped.clear();
            }
            KeyCode::Char('t') if ctrl => {
                self.show_think = !self.show_think;
            }
            KeyCode::Char('n') if ctrl => {
                let _ = self.tx.send(Cmd::NewSession);
                self.segments.clear();
                self.wrapped.clear();
                self.push_seg("  · new session", Style::default().fg(Color::Magenta));
            }
            KeyCode::Char('b') if ctrl => self.show_side = !self.show_side,
            KeyCode::Char('u') if ctrl => {
                self.input.clear();
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let head = self.input[..self.cursor].trim_end().to_string();
                let cut = head.rfind(' ').map(|i| i + 1).unwrap_or(0);
                self.input = format!("{}{}", &head[..cut], &self.input[self.cursor..]);
                self.cursor = cut;
            }
            KeyCode::Char('k') if ctrl => {
                self.input.truncate(self.cursor);
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.input.remove(self.cursor - 1);
                    self.cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.input.chars().count() {
                    self.input.remove(self.cursor);
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => {
                if self.cursor < self.input.chars().count() {
                    self.cursor += 1;
                }
            }
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.input.chars().count(),
            KeyCode::Up => {
                if !self.history.is_empty() {
                    self.hist_idx = self.hist_idx.saturating_sub(1);
                    self.input = self.history.get(self.hist_idx).cloned().unwrap_or_default();
                    self.cursor = self.input.chars().count();
                }
            }
            KeyCode::Down => {
                if self.hist_idx < self.history.len() {
                    self.hist_idx += 1;
                    self.input = self.history.get(self.hist_idx).cloned().unwrap_or_default();
                    self.cursor = self.input.chars().count();
                }
            }
            KeyCode::PageUp => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Char('?') if self.input.is_empty() => {
                self.push_shortcuts();
            }
            KeyCode::Char(c) if !ctrl => {
                let idx = self.input.chars().take(self.cursor).map(|c| c.len_utf8()).sum();
                self.input.insert(idx, c);
                self.cursor += 1;
            }
            KeyCode::Esc => {
                if self.busy {
                    self.shared.cancel.store(true, Ordering::SeqCst);
                    self.busy = false;
                    self.status = "ready".into();
                    self.push_seg("  ⎿  interrupted", Style::default().fg(Color::DarkGray));
                } else {
                    self.help = false;
                    self.input.clear();
                    self.cursor = 0;
                }
            }
            _ => {}
        }
    }

    // -- draw --------------------------------------------------------------
    pub fn draw(&mut self, f: &mut Frame) {
        // keep one blank row at the bottom: if the terminal's real height is a row
        // shorter than the PTY claims, the scroll eats this row instead of the top bar
        let full = f.area();
        let area = Rect { height: full.height.saturating_sub(1), ..full };
        // top bar / transcript / bottom strip (camera + status + input + hint)
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(4),
                Constraint::Length(5),
            ])
            .split(area);

        let side_w = if self.show_side && area.width >= 96 { 34 } else { 0 };
        let body = if side_w > 0 {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(30), Constraint::Length(side_w)])
                .split(rows[1])
        } else {
            Layout::default().constraints([Constraint::Min(30)]).split(rows[1])
        };

        let body_w = body[0].width.max(20);
        if body_w != self.wrap_width {
            self.wrap_width = body_w;
            self.wrapped.clear();
        }
        if self.wrapped.is_empty() && (!self.segments.is_empty() || !self.stream.is_empty()) {
            self.rewrap();
        }

        // bottom strip: the sa camera tile in the left corner, then the working column
        let cam_w = if area.width >= 70 { 20 } else { 0 };
        let bottom = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(cam_w), Constraint::Min(20)])
            .split(rows[2]);
        if cam_w > 0 {
            self.draw_camera(f, bottom[0]);
        }
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(bottom[1]);

        self.draw_top(f, rows[0]);
        self.draw_body(f, body[0]);
        if side_w > 0 {
            self.draw_sidebar(f, body[1]);
        }
        self.draw_status(f, right[0]);
        self.draw_input(f, right[1]);
        self.draw_hint(f, right[2]);
        self.spinner = self.spinner.wrapping_add(1);
    }

    /// A little self-view tile in the corner: the agent's face while it works.
    fn draw_camera(&self, f: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(if self.busy { ACCENT } else { ACCENT_DIM }));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let w = inner.width as usize;
        if w < 12 || inner.height < 3 {
            return;
        }
        let mut line = |y: u16, spans: Vec<Span<'static>>| {
            f.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect { x: inner.x, y: inner.y + y, width: inner.width, height: 1 },
            );
        };

        // row 1: the mark and name, state on the right
        let (glyph, state) = if self.busy { ("◉", "live") } else { ("●", "idle") };
        let state_span = format!("{} {} ", glyph, state);
        let left_len = 5; // " ✻ sa"
        let gap = w.saturating_sub(left_len + state_span.chars().count());
        line(
            0,
            vec![
                Span::styled(" ✻ ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled("sa", Style::default().fg(Color::Rgb(240, 240, 240)).add_modifier(Modifier::BOLD)),
                Span::raw(" ".repeat(gap)),
                Span::styled(
                    state_span,
                    Style::default().fg(if self.busy { ACCENT } else { Color::DarkGray }),
                ),
            ],
        );

        // row 2: a live waveform, so the tile reads as a feed rather than a label
        let bars = ["▁", "▂", "▃", "▅", "▇", "▅", "▃", "▂"];
        let shift = if self.busy { self.spinner } else { 0 };
        let n = w.saturating_sub(2).min(bars.len() * 2);
        let wave: String = (0..n).map(|i| bars[(i + shift) % bars.len()]).collect();
        line(
            1,
            vec![
                Span::raw(" "),
                Span::styled(
                    wave,
                    Style::default().fg(if self.busy { ACCENT } else { Color::Rgb(70, 70, 70) }),
                ),
            ],
        );

        // row 3: mode on the left, approval mode on the right
        let mode = format!(" {}", prompt::mode_name(self.shared.mode()));
        let appr = format!("{} ", self.shared.approver());
        let gap3 = w.saturating_sub(mode.chars().count() + appr.chars().count());
        line(
            2,
            vec![
                Span::styled(mode, Style::default().fg(ACCENT_DIM)),
                Span::raw(" ".repeat(gap3)),
                Span::styled(appr, Style::default().fg(Color::Rgb(80, 80, 80))),
            ],
        );
    }

    fn draw_top(&self, f: &mut Frame, area: Rect) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20), Constraint::Length(24)])
            .split(area);
        let mut spans = vec![
            Span::styled("✻ ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
            Span::styled(
                "SUPERAGENT",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", self.model), Style::default().fg(Color::DarkGray)),
            Span::raw("   "),
        ];
        for (i, m) in MODES.iter().enumerate() {
            let active = i == self.shared.mode();
            let style = if active {
                Style::default()
                    .fg(Color::Rgb(20, 20, 20))
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            spans.push(Span::styled(format!(" {} ", m.to_uppercase()), style));
            spans.push(Span::raw(" "));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), cols[0]);
        let right = format!("${:.4}  {} steps  ", self.stats.cost, self.stats.steps);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(right, Style::default().fg(Color::DarkGray)))),
            cols[1],
        );
    }

    fn draw_body(&mut self, f: &mut Frame, area: Rect) {
        let height = area.height as usize;
        let mut lines = self.wrapped.clone();
        if !self.stream.trim().is_empty() {
            let width = area.width.saturating_sub(3).max(20) as usize;
            let mut first = true;
            for para in self.stream.split('\n') {
                for chunk in wrap(para, width) {
                    if first {
                        first = false;
                        lines.push(Line::from(vec![
                            Span::styled("⏺ ", Style::default().fg(ACCENT)),
                            Span::raw(chunk),
                        ]));
                    } else {
                        lines.push(Line::from(format!("  {}", chunk)));
                    }
                }
            }
        }
        let total = lines.len();
        let max_scroll = total.saturating_sub(height);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        let start = total.saturating_sub(height + self.scroll);
        let view: Vec<Line> = lines.into_iter().skip(start).take(height).collect();
        f.render_widget(Paragraph::new(view), area);
    }

    fn draw_sidebar(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        let head = |t: &str| {
            Line::from(Span::styled(
                t.to_string(),
                Style::default().fg(ACCENT_DIM).add_modifier(Modifier::BOLD),
            ))
        };
        lines.push(head("agents"));
        for a in self.shared.agents_snapshot() {
            let (glyph, color, label) = if !a.present {
                ("·", Color::DarkGray, "absent".to_string())
            } else {
                match a.status {
                    "working" => ("✻", ACCENT, "working".to_string()),
                    "done" => ("●", Color::Green, "done".to_string()),
                    "failed" => ("●", Color::Red, "failed".to_string()),
                    _ => ("○", Color::DarkGray, "idle".to_string()),
                }
            };
            let tail = if a.last.is_empty() {
                if a.headless { String::new() } else { "tui".into() }
            } else {
                a.last.clone()
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", glyph), Style::default().fg(color)),
                Span::styled(
                    format!("{:<8}", a.name),
                    Style::default().fg(if a.present { Color::Reset } else { Color::DarkGray }),
                ),
                Span::styled(
                    format!("{:<8}", label),
                    Style::default().fg(if a.status == "working" { ACCENT } else { Color::DarkGray }),
                ),
                Span::styled(tail, Style::default().fg(Color::DarkGray)),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(head("system"));
        let (nodes, edges, jobs) = config::system_counts();
        let roots = config::roots();
        let total: u64 = roots.iter().map(|r| r.nodes).sum();
        lines.push(Line::from(format!(" {} nodes · {} edges", nodes, edges)));
        lines.push(Line::from(format!(" {} roots · {} symbols", roots.len(), util::kfmt(total))));
        lines.push(Line::from(format!(" {} jobs scheduled", jobs)));
        let mut top = roots.clone();
        top.sort_by(|a, b| b.nodes.cmp(&a.nodes));
        for r in top.iter().take(4) {
            lines.push(Line::from(Span::styled(
                format!(" {:<14} {}", trim(&r.name, 14), util::kfmt(r.nodes)),
                Style::default().fg(Color::DarkGray),
            )));
        }
        lines.push(Line::from(""));
        lines.push(head("session"));
        lines.push(Line::from(Span::styled(
            format!(" {} · {}", prompt::mode_name(self.shared.mode()).to_uppercase(), self.shared.approver()),
            Style::default().fg(ACCENT),
        )));
        lines.push(Line::from(format!(
            " {} steps · {} in / {} out",
            self.stats.steps,
            util::kfmt(self.stats.input),
            util::kfmt(self.stats.output)
        )));
        lines.push(Line::from(Span::styled(
            format!(" ${:.4} · {}", self.stats.cost, mmss(self.started.elapsed().as_secs())),
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            format!(" {}", self.session_id),
            Style::default().fg(Color::DarkGray),
        )));
        let mut out: Vec<Line> = Vec::with_capacity(lines.len());
        for (i, l) in lines.into_iter().enumerate() {
            let mut spans = vec![Span::styled(
                if i == 0 { " " } else { "│" },
                Style::default().fg(Color::Rgb(60, 60, 60)),
            )];
            spans.extend(l.spans);
            out.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(out), area);
    }

    fn draw_status(&self, f: &mut Frame, area: Rect) {
        if self.shared.ask.active.load(Ordering::SeqCst) {
            let q = self.shared.ask.question.lock().unwrap().clone();
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" ✻ approve? [y/N] ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                    Span::styled(q, Style::default()),
                ])),
                area,
            );
            return;
        }
        if self.busy {
            let verb = VERBS[(self.started.elapsed().as_secs() / 3) as usize % VERBS.len()];
            let secs = self.busy_since.elapsed().as_secs();
            let mut spans = vec![
                Span::styled(
                    format!(" {} {}…", SPINNER[self.spinner % SPINNER.len()], verb),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  (esc to interrupt · {}s)", secs),
                    Style::default().fg(Color::DarkGray),
                ),
            ];
            if self.show_think && !self.thinking.is_empty() {
                let tail: String = self.thinking.chars().rev().take(70).collect::<String>().chars().rev().collect();
                spans.push(Span::styled(
                    format!("  {}", tail.replace('\n', " ")),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            f.render_widget(Paragraph::new(Line::from(spans)), area);
        }
    }

    fn draw_input(&self, f: &mut Frame, area: Rect) {
        let border = if self.busy { ACCENT_DIM } else { ACCENT };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border));
        let inner = block.inner(area);
        f.render_widget(block, area);

        let width = inner.width.saturating_sub(3).max(10) as usize;
        let offset = if self.cursor > width { self.cursor - width } else { 0 };
        let visible: String = self.input.chars().skip(offset).take(width + 2).collect();
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("❯ ", Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(visible, Style::default()),
            ])),
            inner,
        );
        let cx = inner.x + 2 + (self.cursor.saturating_sub(offset)) as u16;
        f.set_cursor_position((cx.min(inner.x + inner.width.saturating_sub(1)), inner.y));
    }

    fn draw_hint(&self, f: &mut Frame, area: Rect) {
        let (left, right) = if self.help {
            (
                "  ? for shortcuts".to_string(),
                "F1-F4 mode · F5 sidebar · ^C interrupt · ^L clear · ^T reasoning · ^N new · ^D quit ".to_string(),
            )
        } else if self.busy {
            ("  esc to interrupt".to_string(), "input queues after this turn ".to_string())
        } else {
            (
                "  ? for shortcuts".to_string(),
                format!("{} · yolo · esc clear ", prompt::mode_name(self.shared.mode())),
            )
        };
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20), Constraint::Length(right.chars().count() as u16 + 1)])
            .split(area);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(left, Style::default().fg(Color::DarkGray)))),
            cols[0],
        );
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(right, Style::default().fg(Color::Rgb(90, 90, 90))))),
            cols[1],
        );
    }

    fn push_shortcuts(&mut self) {
        let dim = Style::default().fg(Color::DarkGray);
        for line in [
            "shortcuts",
            "  F1 / F2 / F3 / F4   ask · agent · dispatch · system",
            "  F5                  toggle the agent sidebar",
            "  ⏎                   send          esc   interrupt / clear input",
            "  ^C                  interrupt     ^D    quit",
            "  ^L                  clear screen  ^N    new session",
            "  ^T                  reasoning     ^B    sidebar",
            "commands",
            "  /map /roots /agents /graph <q> /scan /build <root|all> /jobs",
            "  /cost /model [id] /mode <m> /cd <dir> /think /new /session /clear /exit",
            "",
        ] {
            self.push_seg(line.to_string(), dim);
        }
    }

    fn push_welcome(&mut self) {
        let inner = 56usize;
        let border = Style::default().fg(ACCENT_DIM);
        let dim = Style::default().fg(Color::DarkGray);
        let mut rows: Vec<(String, Style)> = Vec::new();
        rows.push(("✻ Welcome to SUPERAGENT".to_string(), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)));
        rows.push((String::new(), dim));
        rows.push(("Ask anything, or drive the whole machine.".to_string(), Style::default()));
        rows.push((String::new(), dim));
        for (cmd, what) in [
            ("/map", "system graph and wiring"),
            ("/roots", "registered repos and node counts"),
            ("/agents", "installed agent CLIs"),
            ("/scan", "rebuild the machine graph"),
        ] {
            rows.push((format!("  {:<9} {}", cmd, what), dim));
        }
        rows.push((String::new(), dim));
        rows.push(("F1 ask · F2 agent · F3 dispatch · F4 system".to_string(), Style::default()));
        rows.push(("F5 sidebar · ^C interrupt · ^D quit".to_string(), Style::default()));
        rows.push((String::new(), dim));
        rows.push((format!("cwd    {}", self.cwd()), dim));
        rows.push((
            format!("model  {} · 1M ctx · yolo", self.model),
            dim,
        ));

        self.push_seg(format!("╭{}╮", "─".repeat(inner)), border);
        for (text, style) in rows {
            let pad = inner.saturating_sub(text.chars().count() + 2);
            self.push_seg(format!("│ {}{} │", text, " ".repeat(pad)), style);
        }
        self.push_seg(format!("╰{}╯", "─".repeat(inner)), border);
        self.push_seg("", dim);
    }

    pub fn run(mut self) -> std::io::Result<()> {
        let mut terminal = ratatui::init();
        self.push_welcome();
        let result = self.event_loop(&mut terminal);
        ratatui::restore();
        result
    }

    fn event_loop(&mut self, terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<()> {
        let mut last_draw = Instant::now();
        while !self.quit {
            self.drain();
            let due = self.busy || last_draw.elapsed() > Duration::from_millis(120);
            if due {
                terminal.draw(|f| self.draw(f))?;
                last_draw = Instant::now();
            }
            if event::poll(Duration::from_millis(40))? {
                match event::read()? {
                    Event::Key(k) => self.key(k),
                    Event::Resize(_, _) => {
                        self.wrapped.clear();
                        terminal.draw(|f| self.draw(f))?;
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    if width < 8 || text.chars().count() <= width {
        return vec![text.to_string()];
    }
    let indent: String = text.chars().take_while(|c| *c == ' ').collect();
    let body = &text[indent.len()..];
    if body.is_empty() {
        return vec![text.to_string()];
    }
    let width = width.saturating_sub(indent.len()).max(8);
    let mut inner = wrap_body(body, width);
    for (i, line) in inner.iter_mut().enumerate() {
        if i > 0 {
            *line = format!("{}{}", indent, line);
        }
    }
    inner
}

fn wrap_body(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let mut w = word;
        while w.chars().count() > width {
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            let head: String = w.chars().take(width).collect();
            out.push(head);
            w = &w[w.char_indices().nth(width).map(|(i, _)| i).unwrap_or(w.len())..];
        }
        if line.is_empty() {
            line = w.to_string();
        } else if line.chars().count() + 1 + w.chars().count() <= width {
            line.push(' ');
            line.push_str(w);
        } else {
            out.push(std::mem::take(&mut line));
            line = w.to_string();
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn trim(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn mmss(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}
