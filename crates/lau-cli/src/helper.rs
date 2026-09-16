//! JSON-line client for the on-device LAU AccessibilityService helper.
//! ADB is used only to `forward` the localabstract socket.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::time::Duration;

pub const SOCKET_NAME: &str = "dev.anythinguse.lau.helper";
pub const PACKAGE: &str = "dev.anythinguse.lau.helper";
pub const SERVICE: &str = "dev.anythinguse.lau.helper/.LauAccessibilityService";

pub fn helper_port(serial: &str) -> u16 {
    if let Ok(p) = std::env::var("LAU_HELPER_PORT") {
        if let Ok(n) = p.parse::<u16>() {
            if n > 0 {
                return n;
            }
        }
    }
    let mut h: u32 = 18730;
    for b in serial.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as u32);
    }
    18700 + (h % 100) as u16
}

pub fn rpc(
    serial: &str,
    ensure_forward: impl FnOnce(&str, u16) -> Result<()>,
    req: Value,
) -> Result<Value> {
    let port = helper_port(serial);
    ensure_forward(serial, port)?;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).with_context(|| {
        format!(
            "helper not reachable on 127.0.0.1:{port} — enable Accessibility → AnythingUse LAU"
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let line = format!("{req}\n");
    stream.write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut resp = String::new();
    reader.read_line(&mut resp).with_context(|| {
        format!("helper closed the connection — {HELPER_DOWN_HINT}")
    })?;
    if resp.trim().is_empty() {
        // Plan §5.5: the socket can still be accepted by a stale instance after
        // the accessibility service is disabled. Say what to do about it.
        bail!("helper accepted the connection but sent no response — {HELPER_DOWN_HINT}");
    }
    serde_json::from_str(resp.trim()).context("helper response is not JSON")
}

/// Actionable guidance shared by the "helper is not really there" errors.
const HELPER_DOWN_HINT: &str =
    "the AccessibilityService is probably disabled or restarting; run `lau doctor --json` \
     and re-enable Settings → Accessibility → AnythingUse LAU";

pub fn request_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("r{nanos}")
}

pub fn wrap_op(op: &str, extra: Value) -> Value {
    let mut obj = extra.as_object().cloned().unwrap_or_default();
    obj.insert("v".into(), json!(1));
    obj.insert("id".into(), json!(request_id()));
    obj.insert("op".into(), json!(op));
    Value::Object(obj)
}

pub fn unwrap_ok(resp: Value) -> Result<Value> {
    if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        return Ok(resp.get("data").cloned().unwrap_or(json!({})));
    }
    let err = resp.get("error").cloned().unwrap_or(json!({}));
    let code = err.get("code").and_then(|v| v.as_str()).unwrap_or("error");
    let message = err
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("helper error");
    bail!("{code}: {message}")
}
