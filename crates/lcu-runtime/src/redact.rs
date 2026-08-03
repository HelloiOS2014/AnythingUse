//! Structured log redaction — secrets, paths to credentials, raw screenshots.

use serde_json::{json, Value};

/// Fields that must never appear in ordinary logs.
const SENSITIVE_KEYS: &[&str] = &[
    "secret",
    "password",
    "token",
    "credential",
    "client_secret",
    "png_bytes",
    "image_b64",
    "screenshot",
    "authorization",
];

/// Recursively redact sensitive keys in a JSON value for structured logging.
pub fn redact_value(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                if SENSITIVE_KEYS
                    .iter()
                    .any(|s| k.eq_ignore_ascii_case(s) || k.to_lowercase().contains(s))
                {
                    out.insert(k.clone(), json!("[REDACTED]"));
                } else {
                    out.insert(k.clone(), redact_value(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(redact_value).collect()),
        other => other.clone(),
    }
}

/// Redact a free-form message that might embed secrets.
pub fn redact_message(msg: &str) -> String {
    let mut s = msg.to_string();
    for key in ["Bearer ", "password=", "secret=", "token="] {
        if let Some(idx) = s.find(key) {
            let value_start = idx + key.len();
            let value_end = s[value_start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .map(|i| value_start + i)
                .unwrap_or(s.len());
            s.replace_range(idx..value_end, &format!("{key}[REDACTED]"));
        }
    }
    s
}

