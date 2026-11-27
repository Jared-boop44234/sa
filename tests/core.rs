//! Behaviour that must not regress: redaction, wrapping, guardrail classification.

#[path = "../src/util.rs"]
#[allow(dead_code)]
mod util;

#[test]
fn redacts_api_keys_and_jwts() {
    let out = util::redact("key sk-abcdefghijklmnopqrstuvwx token=supersecretvalue123456");
    assert!(!out.contains("sk-abcdefghijklmnopqrstuvwx"), "{out}");
    assert!(!out.contains("supersecretvalue123456"), "{out}");
    assert!(out.contains("<redacted>"), "{out}");
}

#[test]
fn keeps_ordinary_text_intact() {
    let text = "the queue drained at 14:02 and depth went to 0";
    assert_eq!(util::redact(text), text);
}

#[test]
fn call_brief_prefers_the_primary_argument() {
    let args = serde_json::json!({"cmd": "git status", "cwd": "/tmp"});
    assert_eq!(util::call_brief("sh", &args), "sh(git status)");
    let read = serde_json::json!({"path": "src/main.rs", "limit": 40});
    assert_eq!(util::call_brief("read", &read), "read(src/main.rs)");
}

#[test]
fn truncate_reports_elision() {
    let out = util::truncate(&"x".repeat(500), 100);
    assert!(out.len() < 500);
    assert!(out.contains("chars elided"), "{out}");
}
