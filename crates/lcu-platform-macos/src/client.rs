//! Unix-socket JSON client for `macos-window-service`.
//!
//! Wire format: one JSON object per line (request → response).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use serde_json::{json, Value};

/// Default per-user private socket (must match Swift `resolveSocketPath`).
pub fn default_socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("LCU_MACOS_WINDOW_SOCK") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("AnythingUse")
        .join("macos-window.sock")
}

/// Locate the native service binary when auto-spawn is enabled.
pub fn default_service_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("LCU_MACOS_WINDOW_SERVICE") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    // Relative to CARGO_MANIFEST_DIR / workspace when developing.
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../native/macos-window-service/.build/release/macos-window-service"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../native/macos-window-service/.build/debug/macos-window-service"),
        PathBuf::from("native/macos-window-service/.build/release/macos-window-service"),
    ];
    for c in candidates {
        if c.is_file() {
            return Some(c);
        }
    }
    None
}

/// Thin JSON-RPC client. Connections are short-lived (one RPC per connect) to keep
/// the adapter simple and avoid half-open sockets after service restarts.
pub struct NativeClient {
    socket_path: PathBuf,
    /// When true, try to spawn the service binary if the socket is missing.
    auto_spawn: bool,
    child: Mutex<Option<Child>>,
    next_id: Mutex<u64>,
}

impl Default for NativeClient {
    fn default() -> Self {
        Self::new(default_socket_path(), true)
    }
}

impl NativeClient {
    pub fn new(socket_path: impl Into<PathBuf>, auto_spawn: bool) -> Self {
        Self {
            socket_path: socket_path.into(),
            auto_spawn,
            child: Mutex::new(None),
            next_id: Mutex::new(1),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn call(&self, method: &str, params: Option<Value>) -> LcuResult<Value> {
        self.ensure_running()?;
        let id = {
            let mut g = self.next_id.lock().expect("id");
            let id = *g;
            *g = g.saturating_add(1);
            id
        };
        let mut req = json!({
            "id": id,
            "method": method,
        });
        if let Some(p) = params {
            req["params"] = p;
        }
        let line = serde_json::to_string(&req).map_err(|e| {
            LcuError::coded(ErrorCode::InternalError, format!("encode request: {e}"))
        })?;

        let mut stream = UnixStream::connect(&self.socket_path).map_err(|e| {
            LcuError::coded(
                ErrorCode::RuntimeUnavailable,
                format!(
                    "connect macos-window-service at {}: {e}",
                    self.socket_path.display()
                ),
            )
        })?;
        stream
            .set_read_timeout(Some(Duration::from_secs(45)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .ok();

        writeln!(stream, "{line}").map_err(|e| {
            LcuError::coded(ErrorCode::InternalError, format!("write request: {e}"))
        })?;
        stream.flush().ok();

        let mut reader = BufReader::new(stream);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).map_err(|e| {
            LcuError::coded(
                ErrorCode::RuntimeUnavailable,
                format!("read response from macos-window-service: {e}"),
            )
        })?;
        if resp_line.trim().is_empty() {
            return Err(LcuError::coded(
                ErrorCode::RuntimeUnavailable,
                "empty response from macos-window-service",
            ));
        }
        let resp: Value = serde_json::from_str(resp_line.trim()).map_err(|e| {
            LcuError::coded(
                ErrorCode::InternalError,
                format!("parse response: {e}; body={}", resp_line.trim()),
            )
        })?;
        if resp.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            return Ok(resp.get("result").cloned().unwrap_or(Value::Null));
        }
        let err = resp.get("error").cloned().unwrap_or(Value::Null);
        let code = err
            .get("code")
            .and_then(|c| c.as_str())
            .unwrap_or("internal_error");
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("native service error");
        Err(map_service_error(code, message))
    }

    fn ensure_running(&self) -> LcuResult<()> {
        if self.socket_path.exists() {
            // Quick connect probe.
            if UnixStream::connect(&self.socket_path).is_ok() {
                return Ok(());
            }
        }
        if !self.auto_spawn {
            return Err(LcuError::coded(
                ErrorCode::RuntimeUnavailable,
                format!(
                    "macos-window-service not available at {}",
                    self.socket_path.display()
                ),
            ));
        }
        self.spawn_service()?;
        // Wait for socket to appear.
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if self.socket_path.exists() && UnixStream::connect(&self.socket_path).is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Err(LcuError::coded(
            ErrorCode::RuntimeUnavailable,
            format!(
                "macos-window-service did not become ready at {}",
                self.socket_path.display()
            ),
        ))
    }

    fn spawn_service(&self) -> LcuResult<()> {
        let bin = default_service_binary().ok_or_else(|| {
            LcuError::coded(
                ErrorCode::RuntimeUnavailable,
                "macos-window-service binary not found; set LCU_MACOS_WINDOW_SERVICE or build native/macos-window-service",
            )
        })?;
        // Ensure parent dir exists.
        if let Some(parent) = self.socket_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let child = Command::new(&bin)
            .arg("serve")
            .arg("--socket")
            .arg(&self.socket_path)
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| {
                LcuError::coded(
                    ErrorCode::RuntimeUnavailable,
                    format!("spawn {}: {e}", bin.display()),
                )
            })?;
        *self.child.lock().expect("child") = Some(child);
        tracing::info!(
            bin = %bin.display(),
            socket = %self.socket_path.display(),
            "spawned macos-window-service"
        );
        Ok(())
    }
}

impl Drop for NativeClient {
    fn drop(&mut self) {
        // Leave the service running for other clients (I1 may own lifecycle).
        // Only drop the Child handle without kill — OS reaps on process exit if orphaned.
        if let Ok(mut g) = self.child.lock() {
            // Detach: don't kill. Caller (desktop/I1) owns long-lived service.
            let _ = g.take();
        }
    }
}

fn map_service_error(code: &str, message: &str) -> LcuError {
    let ec = match code {
        "permission_denied" => ErrorCode::PermissionDenied,
        "not_found" | "target_lost" | "action_failed" => ErrorCode::TaskFailed,
        "taken_over" => ErrorCode::WaitingUser,
        "foreground_required" => ErrorCode::ForegroundRequired,
        "unsupported_capability" => ErrorCode::UnsupportedCapability,
        "invalid_request" => ErrorCode::InvalidRequest,
        "not_implemented" => ErrorCode::NotImplemented,
        _ => ErrorCode::InternalError,
    };
    LcuError::coded(ec, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_socket_is_under_local_computer_use() {
        let p = default_socket_path();
        assert!(
            p.to_string_lossy().contains("AnythingUse")
                || std::env::var("LCU_MACOS_WINDOW_SOCK").is_ok()
        );
        assert!(p.to_string_lossy().ends_with("macos-window.sock")
            || std::env::var("LCU_MACOS_WINDOW_SOCK").is_ok());
    }

    #[test]
    fn preserves_foreground_required_from_native_service() {
        let error = map_service_error("foreground_required", "background input unavailable");
        assert_eq!(error.code(), ErrorCode::ForegroundRequired);
        assert_eq!(
            error.to_string(),
            "foreground_required: background input unavailable"
        );
    }

}
