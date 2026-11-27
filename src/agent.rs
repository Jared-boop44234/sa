use crate::config;
use crate::llm::{cost, Client, Delta, Usage};
use crate::prompt;
use crate::tools::{self, Shared, Stats};
use crate::util;
use serde_json::{json, Value};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

pub enum Ev {
    Content(String),
    Reasoning(String),
    Status(String),
    Tool { name: String, args: Value },
    Result { summary: String, ok: bool, ms: u64 },
    Error(String),
    Done(String),
    Stats(Stats),
    Idle,
    Mode(usize),
    Session(String),
}

pub struct Agent {
    pub client: Client,
    pub shared: Arc<Shared>,
    pub messages: Vec<Value>,
    pub session_id: String,
    pub session_path: PathBuf,
    pub max_steps: u32,
    pub think: bool,
    pub usage: Usage,
}

impl Agent {
    pub fn new(model: &str, effort: &str, shared: Arc<Shared>, max_steps: u32) -> Result<Agent, String> {
        let client = Client::new(model, effort)?;
        let id = util::stamp();
        let path = config::sessions_dir().join(format!("{}.jsonl", id));
        Ok(Agent {
            client,
            shared,
            messages: Vec::new(),
            session_id: id,
            session_path: path,
            max_steps,
            think: false,
            usage: Usage::default(),
        })
    }

    pub fn model_name(&self) -> String {
        self.client.model.clone()
    }

    pub fn set_model(&mut self, model: &str) -> Result<(), String> {
        let c = Client::new(model, &self.client.effort)?;
        self.client = c;
        self.sync_stats();
        Ok(())
    }

    fn record(&self, obj: Value) {
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.session_path) {
            let _ = writeln!(f, "{}", obj);
        }
    }

    fn record_message(&self, msg: &Value) {
        self.record(json!({"type": "message", "message": msg}));
    }

    fn sync_stats(&self) {
        let mut s = self.shared.stats.lock().unwrap();
        s.model = self.client.model.clone();
        s.input = self.usage.input;
        s.output = self.usage.output;
        s.cost = cost(&self.client.model, &self.usage);
    }

    fn system_message(&self) -> Value {
        json!({"role": "system", "content": prompt::build(self.shared.mode())})
    }

    pub fn total_cost(&self) -> f64 {
        cost(&self.client.model, &self.usage)
    }

    pub fn ask<F: FnMut(Ev)>(&mut self, text: &str, emit: &mut F) -> String {
        // system prompt is rebuilt each turn so a mode switch takes effect immediately
        if self.messages.first().map(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
            == Some(true)
        {
            self.messages[0] = self.system_message();
        } else {
            let sys = self.system_message();
            self.messages.insert(0, sys.clone());
            self.record_message(&sys);
        }
        let user = json!({"role": "user", "content": text});
        self.messages.push(user.clone());
        self.record_message(&user);
        self.shared.cancel.store(false, Ordering::SeqCst);
        let answer = self.run(emit);
        emit(Ev::Idle);
        answer
    }

    fn run<F: FnMut(Ev)>(&mut self, emit: &mut F) -> String {
        for _ in 0..self.max_steps {
            if self.shared.cancel.load(Ordering::SeqCst) {
                emit(Ev::Status("cancelled".into()));
                return "(cancelled)".into();
            }
            {
                let mut s = self.shared.stats.lock().unwrap();
                s.steps += 1;
            }
            emit(Ev::Status("thinking".into()));
            let mode = self.shared.mode();
            let tools_schema = tools::schema_for(mode);
            let mut content_acc = String::new();
            let reply = {
                let cancel = &self.shared.cancel;
                let mut on_delta = |d: Delta| match d {
                    Delta::Content(t) => {
                        content_acc.push_str(&t);
                        emit(Ev::Content(t));
                    }
                    Delta::Reasoning(t) => emit(Ev::Reasoning(t)),
                };
                self.client
                    .chat(&self.messages, Some(&tools_schema), cancel, &mut on_delta)
            };
            let reply = match reply {
                Ok(r) => r,
                Err(e) => {
                    if e == "cancelled" {
                        emit(Ev::Status("cancelled".into()));
                        return "(cancelled)".into();
                    }
                    emit(Ev::Error(e.clone()));
                    self.sync_stats();
                    emit(Ev::Stats(self.shared.stats_snapshot()));
                    return String::new();
                }
            };
            self.usage.input += reply.usage.input;
            self.usage.output += reply.usage.output;
            self.usage.cached += reply.usage.cached;
            self.sync_stats();

            let mut assistant = json!({"role": "assistant", "content": reply.content});
            if !reply.reasoning.is_empty() {
                assistant["reasoning_content"] = json!(reply.reasoning);
            }
            if !reply.tool_calls.is_empty() {
                let calls: Vec<Value> = reply
                    .tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.id,
                            "type": "function",
                            "function": {"name": c.name, "arguments": c.arguments}
                        })
                    })
                    .collect();
                assistant["tool_calls"] = Value::Array(calls);
            }
            self.messages.push(assistant.clone());
            self.record_message(&assistant);

            if reply.tool_calls.is_empty() {
                emit(Ev::Stats(self.shared.stats_snapshot()));
                emit(Ev::Done(reply.content.clone()));
                return reply.content;
            }

            emit(Ev::Status("tools".into()));
            for call in &reply.tool_calls {
                let args: Value = serde_json::from_str(&call.arguments).unwrap_or(json!({}));
                emit(Ev::Tool { name: call.name.clone(), args: args.clone() });
                let started = Instant::now();
                let result = tools::execute(&call.name, &args, &self.shared);
                let ms = started.elapsed().as_millis() as u64;
                let ok = !tools::approve_blocked(&result);
                emit(Ev::Result {
                    summary: util::first_line(&result, 110),
                    ok,
                    ms,
                });
                let msg = json!({
                    "role": "tool",
                    "tool_call_id": call.id,
                    "content": result
                });
                self.messages.push(msg.clone());
                self.record_message(&msg);
            }
            emit(Ev::Stats(self.shared.stats_snapshot()));
        }
        emit(Ev::Error(format!("step budget exhausted ({})", self.max_steps)));
        "(stopped: step budget exhausted)".into()
    }
}
