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
    // Action payload fields that carry model-typed content: they can hold
    // credentials and must not hit logs verbatim.
    "value",
    "text",
    "url",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_entire_navigation_url() {
        let value = json!({
            "kind": "semantic",
            "type": "navigate",
            "url": "https://example.com/?password=secret"
        });
        assert_eq!(redact_value(&value)["url"], "[REDACTED]");
    }
}
