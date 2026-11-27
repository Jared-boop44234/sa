use crate::config;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub enum Delta {
    Content(String),
    Reasoning(String),
}

#[derive(Debug, Default, Clone)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
}

#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Default)]
pub struct Reply {
    pub content: String,
    pub reasoning: String,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

pub struct Client {
    pub key: String,
    pub model: String,
    pub effort: String,
    pub max_tokens: u32,
    pub temperature: f64,
}

impl Client {
    pub fn new(model: &str, effort: &str) -> Result<Client, String> {
        let key = config::api_key()
            .ok_or_else(|| "no DeepSeek key: set DEEPSEEK_API_KEY or run `pi auth login deepseek`".to_string())?;
        Ok(Client {
            key,
            model: model.to_string(),
            effort: effort.to_string(),
            max_tokens: 8000,
            temperature: 0.2,
        })
    }

    pub fn models(&self) -> Vec<String> {
        let out = ureq::get(&format!("{}/models", config::BASE_URL))
            .set("Authorization", &format!("Bearer {}", self.key))
            .timeout(Duration::from_secs(20))
            .call();
        match out {
            Ok(resp) => {
                let v: Value = serde_json::from_str(&resp.into_string().unwrap_or_default())
                    .unwrap_or_else(|_| json!({}));
                v.get("data")
                    .and_then(|d| d.as_array())
                    .map(|a| {
                        a.iter()
                            .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            Err(_) => Vec::new(),
        }
    }

    fn payload(&self, messages: &[Value], tools: Option<&Value>) -> Value {
        let mut p = json!({
            "model": self.model,
            "messages": messages,
            "temperature": self.temperature,
            "max_tokens": self.max_tokens,
            "stream": true,
            "stream_options": {"include_usage": true},
            "reasoning_effort": self.effort,
        });
        if let Some(t) = tools {
            p["tools"] = t.clone();
        }
        p
    }

    pub fn chat(
        &self,
        messages: &[Value],
        tools: Option<&Value>,
        cancel: &AtomicBool,
        on_delta: &mut dyn FnMut(Delta),
    ) -> Result<Reply, String>
    {
        let body = self.payload(messages, tools).to_string();
        let mut last = String::from("request failed");
        for attempt in 0..4 {
            if cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            let resp = ureq::post(&format!("{}/chat/completions", config::BASE_URL))
                .set("Authorization", &format!("Bearer {}", self.key))
                .set("Content-Type", "application/json")
                .timeout(Duration::from_secs(600))
                .send_string(&body);
            match resp {
                Ok(r) => return self.read_stream(r, cancel, on_delta),
                Err(ureq::Error::Status(code, r)) => {
                    let detail = r.into_string().unwrap_or_default();
                    last = format!("HTTP {}: {}", code, detail.chars().take(300).collect::<String>());
                    if matches!(code, 400 | 401 | 402 | 403 | 422) {
                        return Err(last);
                    }
                }
                Err(e) => last = format!("network: {}", e),
            }
            std::thread::sleep(Duration::from_millis(500 * (1 << attempt)));
        }
        Err(last)
    }

    fn read_stream(
        &self,
        resp: ureq::Response,
        cancel: &AtomicBool,
        on_delta: &mut dyn FnMut(Delta),
    ) -> Result<Reply, String>
    {
        let mut reply = Reply::default();
        let mut calls: BTreeMap<usize, ToolCall> = BTreeMap::new();
        let reader = BufReader::new(resp.into_reader());
        for line in reader.lines() {
            if cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            let Ok(line) = line else { break };
            let line = line.trim();
            let Some(data) = line.strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let Ok(chunk) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
                reply.usage.input = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                reply.usage.output = u.get("completion_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                reply.usage.cached = u
                    .get("prompt_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|v| v.as_u64())
                    .or_else(|| u.get("prompt_cache_hit_tokens").and_then(|v| v.as_u64()))
                    .unwrap_or(0);
            }
            let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else {
                continue;
            };
            let Some(delta) = choice.get("delta") else { continue };
            if let Some(t) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
                reply.reasoning.push_str(t);
                on_delta(Delta::Reasoning(t.to_string()));
            }
            if let Some(t) = delta.get("content").and_then(|v| v.as_str()) {
                reply.content.push_str(t);
                on_delta(Delta::Content(t.to_string()));
            }
            if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tcs {
                    let idx = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    let slot = calls.entry(idx).or_insert_with(|| ToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            slot.id = id.to_string();
                        }
                    }
                    if let Some(f) = tc.get("function") {
                        if let Some(n) = f.get("name").and_then(|v| v.as_str()) {
                            slot.name.push_str(n);
                        }
                        if let Some(a) = f.get("arguments").and_then(|v| v.as_str()) {
                            slot.arguments.push_str(a);
                        }
                    }
                }
            }
        }
        for (i, mut c) in calls {
            if c.id.is_empty() {
                c.id = format!("call_{}", i);
            }
            reply.tool_calls.push(c);
        }
        Ok(reply)
    }
}

pub fn cost(model: &str, usage: &Usage) -> f64 {
    let (pin, pout, pcache) = match model {
        "deepseek-v4-pro" => (0.30, 1.20, 0.006),
        _ => (0.30, 1.20, 0.006),
    };
    let fresh = usage.input.saturating_sub(usage.cached) as f64;
    (fresh * pin + usage.cached as f64 * pcache + usage.output as f64 * pout) / 1_000_000.0
}
