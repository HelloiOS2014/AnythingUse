//! Chrome tab surface adapter (Wave 2 D3) + product routing (I1).
//!
//! Talks to the product Native Messaging host over the Runtime **private** Unix
//! socket (`chrome-control.sock`). Never opens a TCP control listener.
//!
//! Maps extension observe/act results onto shared types:
//! [`ChromeTab`], [`ControlState`], [`AppObservation`], [`Action`].
//!
//! This crate intentionally does **not** call macOS AX or AppleScript.

pub mod product;

pub use product::ProductBackend;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::Utc;
use lcu_core::action::{is_http_navigation_url, Action, SemanticAction, TargetedInput};
use lcu_core::capability::CapabilityLevel;
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::{
    AppObservation, AppTarget, ElementNode, ModelSize, ObservationId, Rect, TransformId,
};
use lcu_core::risk::RiskLevel;
use lcu_core::surface::{ChromeTab, ControlState, ControlTarget};
use lcu_core::task::{ActionReceipt, TaskId};
use lcu_core::types::Frame;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Paths (mirror lcu-runtime RuntimePaths root; no dependency on runtime crate)
// ---------------------------------------------------------------------------

/// Default per-user Application Support root used by Runtime private entry.
pub fn default_runtime_root() -> LcuResult<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| {
        LcuError::coded(
            ErrorCode::InternalError,
            "cannot resolve user data directory for Chrome control socket",
        )
    })?;
    Ok(base.join("AnythingUse"))
}

/// Unix socket path for the Chrome control host (private entry sibling of runtime.sock).
pub fn default_chrome_control_sock() -> LcuResult<PathBuf> {
    Ok(default_runtime_root()?.join("chrome-control.sock"))
}

pub fn chrome_control_sock_in(root: impl AsRef<Path>) -> PathBuf {
    root.as_ref().join("chrome-control.sock")
}

// ---------------------------------------------------------------------------
// Wire protocol (JSON lines over Unix stream)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcRequest {
    id: String,
    method: String,
    #[serde(default)]
    params: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "timeoutMs")]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcResponse {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    ok: Option<bool>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    event: Option<bool>,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Low-level client to `chrome-control.sock`.
#[derive(Debug, Clone)]
pub struct ChromeControlClient {
    sock_path: PathBuf,
    timeout: Duration,
}

impl ChromeControlClient {
    pub fn new(sock_path: impl Into<PathBuf>) -> Self {
        Self {
            sock_path: sock_path.into(),
            timeout: Duration::from_secs(60),
        }
    }

    pub fn with_default_path() -> LcuResult<Self> {
        Ok(Self::new(default_chrome_control_sock()?))
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn sock_path(&self) -> &Path {
        &self.sock_path
    }

    /// True when the private Unix socket file exists (host likely running).
    pub fn socket_present(&self) -> bool {
        self.sock_path.exists()
    }

    pub fn call(&self, method: &str, params: Value) -> LcuResult<Value> {
        if !self.sock_path.exists() {
            return Err(LcuError::coded(
                ErrorCode::RuntimeUnavailable,
                format!(
                    "chrome control socket missing at {}; load LCU Chrome Control extension / install native host",
                    self.sock_path.display()
                ),
            ));
        }

        let mut stream = UnixStream::connect(&self.sock_path).map_err(|e| {
            LcuError::coded(
                ErrorCode::RuntimeUnavailable,
                format!("connect chrome control socket: {e}"),
            )
        })?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, e.to_string()))?;
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, e.to_string()))?;

        let id = format!("rs_{}", Uuid::new_v4());
        let req = RpcRequest {
            id: id.clone(),
            method: method.to_string(),
            params,
            timeout_ms: Some(self.timeout.as_millis() as u64),
        };
        let line = serde_json::to_string(&req).map_err(|e| {
            LcuError::coded(ErrorCode::InternalError, format!("encode rpc: {e}"))
        })?;
        stream
            .write_all(line.as_bytes())
            .and_then(|_| stream.write_all(b"\n"))
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("write rpc: {e}")))?;

        let mut reader = BufReader::new(stream);
        let deadline = Instant::now() + self.timeout;
        loop {
            if Instant::now() > deadline {
                return Err(LcuError::coded(
                    ErrorCode::RuntimeUnavailable,
                    format!("timeout waiting for chrome control method {method}"),
                ));
            }
            let mut buf = String::new();
            let n = reader.read_line(&mut buf).map_err(|e| {
                LcuError::coded(ErrorCode::InternalError, format!("read rpc: {e}"))
            })?;
            if n == 0 {
                return Err(LcuError::coded(
                    ErrorCode::RuntimeUnavailable,
                    "chrome control socket closed before response",
                ));
            }
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                continue;
            }
            let resp: RpcResponse = serde_json::from_str(trimmed).map_err(|e| {
                LcuError::coded(ErrorCode::InternalError, format!("decode rpc: {e}"))
            })?;
            if resp.event == Some(true) {
                continue;
            }
            if let Some(ref rid) = resp.id {
                if rid != &id {
                    continue;
                }
            }
            if resp.ok == Some(false) {
                return Err(LcuError::coded(
                    ErrorCode::TaskFailed,
                    resp.error.unwrap_or_else(|| "chrome control error".into()),
                ));
            }
            if resp.ok == Some(true) {
                return Ok(resp.result.unwrap_or(Value::Null));
            }
        }
    }

    pub fn host_ping(&self) -> LcuResult<Value> {
        self.call("host_ping", json!({}))
    }

    pub fn ping(&self) -> LcuResult<Value> {
        self.call("ping", json!({}))
    }
}

// ---------------------------------------------------------------------------
// Surface adapter
// ---------------------------------------------------------------------------

/// Product ChromeTab surface: extension tab lease + control state + observe/act.
#[derive(Debug)]
pub struct ChromeTabSurface {
    client: ChromeControlClient,
    /// Claimed background tab; `None` means no lease.
    tab: Option<ChromeTab>,
    task_id: Option<TaskId>,
    control_state: ControlState,
    /// Last element map is held in the extension; we keep the last observation.
    last_observation: Option<AppObservation>,
}

impl ChromeTabSurface {
    pub fn new(client: ChromeControlClient) -> Self {
        Self {
            client,
            tab: None,
            task_id: None,
            control_state: ControlState::None,
            last_observation: None,
        }
    }

    pub fn with_default_path() -> LcuResult<Self> {
        Ok(Self::new(ChromeControlClient::with_default_path()?))
    }

    pub fn client(&self) -> &ChromeControlClient {
        &self.client
    }

    pub fn tab(&self) -> Option<&ChromeTab> {
        self.tab.as_ref()
    }

    pub fn control_state(&self) -> ControlState {
        self.control_state
    }

    pub fn last_observation(&self) -> Option<&AppObservation> {
        self.last_observation.as_ref()
    }

    /// Claim a background task tab (extension holds debugger/tab lease).
    pub fn claim(
        &mut self,
        url: &str,
        task_id: Option<TaskId>,
        profile: Option<&str>,
    ) -> LcuResult<ChromeTab> {
        if self.tab.is_some() {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                "chrome tab lease already held on this surface instance",
            ));
        }

        let mut params = json!({ "url": url });
        if let Some(ref tid) = task_id {
            params["taskId"] = json!(tid.0);
        }
        if let Some(p) = profile {
            params["profile"] = json!(p);
        }

        let result = self.client.call("claim", params)?;
        let tab = chrome_tab_from_claim(&result)?;
        self.tab = Some(tab.clone());
        self.task_id = task_id;
        self.control_state = ControlState::None;
        Ok(tab)
    }

    /// Handoff lease ownership to another task id (same tab/debugger).
    pub fn handoff(&mut self, task_id: TaskId) -> LcuResult<()> {
        self.ensure_auto()?;
        let _ = self
            .client
            .call("handoff", json!({ "taskId": task_id.0 }))?;
        self.task_id = Some(task_id);
        Ok(())
    }

    /// Observe the leased tab (CDP DOM snapshot → AppObservation).
    pub fn observe(&mut self) -> LcuResult<AppObservation> {
        self.ensure_auto()?;
        let result = self.client.call("observe", json!({}))?;
        self.sync_control_state_from_value(&result);
        let obs = map_observe_result(&result)?;
        self.last_observation = Some(obs.clone());
        Ok(obs)
    }

    /// Perform a P3 Action on the leased tab.
    pub fn act(&mut self, action: &Action) -> LcuResult<ActionReceipt> {
        // Local control-session terminal actions do not require extension auto-control
        // beyond release semantics.
        match action {
            Action::Done { .. } | Action::Fail { .. } | Action::RequestUser { .. } => {
                return Ok(receipt(action, true, Some("chrome surface passthrough")));
            }
            Action::Wait { milliseconds } => {
                self.ensure_auto()?;
                let ms = (*milliseconds).min(30_000);
                let action_json = json!({ "kind": "wait", "milliseconds": ms });
                let _ = self.client.call("act", json!({ "action": action_json }))?;
                return Ok(receipt(action, true, Some("waited")));
            }
            Action::Observe => {
                let _ = self.observe()?;
                return Ok(receipt(action, true, Some("observed")));
            }
            _ => {}
        }

        self.ensure_auto()?;
        if let Action::Semantic(SemanticAction::Navigate { url }) = action {
            if !is_http_navigation_url(url) {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    "navigate requires an explicit http:// or https:// URL with a host",
                ));
            }
            let result = self.client.call("navigate", json!({ "url": url }))?;
            self.sync_control_state_from_value(&result);
            return Ok(receipt(action, true, Some("navigated")));
        }
        let action_json = action_to_extension_json(action)?;
        let result = self
            .client
            .call("act", json!({ "action": action_json }))?;
        self.sync_control_state_from_value(&result);

        let success = result
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let message = result
            .get("error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Ok(receipt(action, success, message.as_deref()))
    }

    /// Semantic action convenience (maps through [`Action::Semantic`]).
    pub fn perform_semantic(&mut self, action: &SemanticAction) -> LcuResult<ActionReceipt> {
        self.act(&Action::Semantic(action.clone()))
    }

    /// Targeted input convenience.
    pub fn perform_targeted(&mut self, action: &TargetedInput) -> LcuResult<ActionReceipt> {
        self.act(&Action::Targeted(action.clone()))
    }

    /// Release debugger + tab lease (cancel / complete / fail / task end).
    ///
    /// Always asks the extension to detach the debugger. When already
    /// `taken_over`, leave the tab open (user is viewing it); otherwise
    /// close the background task tab.
    pub fn release(&mut self) -> LcuResult<()> {
        let close_tab = !matches!(self.control_state, ControlState::TakenOver);
        // Prefer release (explicit detach + lease drop); fall back to cleanup.
        let release_res = self.client.call(
            "release",
            json!({ "closeTab": close_tab, "close_tab": close_tab }),
        );
        if release_res.is_err() {
            let reason = if close_tab {
                "adapter_release"
            } else {
                "taken_over"
            };
            let _ = self.client.call("cleanup", json!({ "reason": reason }));
        }
        self.clear_local();
        Ok(())
    }

    /// Force cleanup (crash / cancel / user takeover terminal).
    /// Always detaches debugger and clears local lease bookkeeping.
    pub fn cleanup(&mut self, reason: &str) -> LcuResult<()> {
        let _ = self.client.call("cleanup", json!({ "reason": reason }));
        // Second chance: explicit release if cleanup path failed to clear attach.
        let _ = self.client.call(
            "release",
            json!({
                "closeTab": !reason.contains("taken_over"),
                "close_tab": !reason.contains("taken_over"),
            }),
        );
        self.clear_local();
        Ok(())
    }

    /// Apply a control-state report from host events (taken_over / target_lost).
    pub fn apply_control_state(&mut self, state: ControlState) -> bool {
        if self.tab.is_none() {
            return false;
        }
        let prev = self.control_state;
        self.control_state = prev.apply(state);
        prev != self.control_state
    }

    /// Refresh control state from extension get_state.
    pub fn refresh_control_state(&mut self) -> LcuResult<ControlState> {
        let result = self.client.call("get_state", json!({}))?;
        self.sync_control_state_from_value(&result);
        Ok(self.control_state)
    }

    fn clear_local(&mut self) {
        self.tab = None;
        self.task_id = None;
        self.control_state = ControlState::None;
        self.last_observation = None;
    }

    fn ensure_auto(&self) -> LcuResult<()> {
        if self.tab.is_none() {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                "no chrome tab lease; call claim first",
            ));
        }
        if !self.control_state.allows_auto_control() {
            return Err(LcuError::coded(
                ErrorCode::WaitingUser,
                format!("chrome control blocked: state={:?}", self.control_state),
            ));
        }
        Ok(())
    }

    fn sync_control_state_from_value(&mut self, value: &Value) {
        let raw = value
            .get("controlState")
            .or_else(|| value.get("control_state"))
            .and_then(|v| v.as_str());
        let Some(raw) = raw else {
            return;
        };
        let next = match raw {
            "taken_over" => ControlState::TakenOver,
            "target_lost" => ControlState::TargetLost,
            "none" => return,
            _ => return,
        };
        let _ = self.apply_control_state(next);
    }
}

impl Drop for ChromeTabSurface {
    fn drop(&mut self) {
        if self.tab.is_some() {
            let _ = self.cleanup("surface_drop");
        }
    }
}

// ---------------------------------------------------------------------------
// Mapping helpers
// ---------------------------------------------------------------------------

fn chrome_tab_from_claim(result: &Value) -> LcuResult<ChromeTab> {
    if let Some(ct) = result.get("chromeTab").or_else(|| result.get("chrome_tab")) {
        let profile = ct
            .get("profile")
            .and_then(|v| v.as_str())
            .ok_or_else(|| LcuError::coded(ErrorCode::TaskFailed, "claim missing profile key"))?
            .to_string();
        let tab_id = ct
            .get("tab_id")
            .or_else(|| ct.get("tabId"))
            .and_then(|v| v.as_i64())
            .ok_or_else(|| {
                LcuError::coded(ErrorCode::TaskFailed, "claim response missing tab_id")
            })?;
        return Ok(ChromeTab::new(profile, tab_id));
    }
    if let Some(lease) = result.get("lease") {
        let tab_id = lease
            .get("tabId")
            .or_else(|| lease.get("tab_id"))
            .and_then(|v| v.as_i64())
            .ok_or_else(|| {
                LcuError::coded(ErrorCode::TaskFailed, "lease missing tabId")
            })?;
        let profile = lease
            .get("profile")
            .and_then(|v| v.as_str())
            .ok_or_else(|| LcuError::coded(ErrorCode::TaskFailed, "lease missing profile key"))?
            .to_string();
        return Ok(ChromeTab::new(profile, tab_id));
    }
    Err(LcuError::coded(
        ErrorCode::TaskFailed,
        "claim response missing chromeTab/lease",
    ))
}

fn map_observe_result(result: &Value) -> LcuResult<AppObservation> {
    let obs = result
        .get("observation")
        .ok_or_else(|| LcuError::coded(ErrorCode::TaskFailed, "observe missing observation"))?;

    let target_v = obs.get("target").cloned().unwrap_or(json!({}));
    let chrome = obs
        .get("chrome_tab")
        .or_else(|| obs.get("chromeTab"))
        .cloned()
        .unwrap_or(json!({}));

    let tab_id = chrome
        .get("tab_id")
        .or_else(|| chrome.get("tabId"))
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| {
            target_v
                .get("window_id")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as i64
        });

    let title = obs
        .get("page_title")
        .or_else(|| target_v.get("window_title"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let profile = chrome
        .get("profile")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LcuError::coded(ErrorCode::TaskFailed, "observe missing profile key"))?;
    let page_url = obs
        .get("page_url")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LcuError::coded(ErrorCode::TaskFailed, "observe missing page_url"))?;

    let frame = obs.get("window_frame").cloned().unwrap_or(json!({}));
    let model = obs.get("model_size").cloned().unwrap_or(json!({}));

    let elements = obs
        .get("elements")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|el| {
                    Some(ElementNode {
                        id: el.get("id")?.as_str()?.to_string(),
                        role: el
                            .get("role")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string(),
                        label: el
                            .get("label")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        value: el.get("value").and_then(|v| {
                            if v.is_null() {
                                None
                            } else {
                                v.as_str()
                                    .map(|s| s.to_string())
                                    .or_else(|| Some(v.to_string()))
                            }
                        }),
                        frame: Rect {
                            x: el.get("frame")?.get("x")?.as_f64().unwrap_or(0.0),
                            y: el.get("frame")?.get("y")?.as_f64().unwrap_or(0.0),
                            width: el.get("frame")?.get("width")?.as_f64().unwrap_or(0.0),
                            height: el.get("frame")?.get("height")?.as_f64().unwrap_or(0.0),
                        },
                        actions: el
                            .get("actions")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    let image_png = obs
        .get("image_png_b64")
        .and_then(|v| v.as_str())
        .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok())
        .filter(|bytes| !bytes.is_empty())
        .ok_or_else(|| {
            LcuError::coded(
                ErrorCode::TaskFailed,
                "chrome observation missing real screenshot (image_png_b64 empty/absent); \
                 reload the unpacked LCU Chrome extension (version mismatch or CDP capture failed)",
            )
        })?;
    let image_hash = hex::encode(Sha256::digest(&image_png));

    Ok(AppObservation {
        observation_id: ObservationId::new(),
        timestamp_ms: Utc::now().timestamp_millis(),
        target: AppTarget {
            app_id: "com.google.Chrome".into(),
            // Chrome surface uses tab id in window_id for wire compatibility with
            // AppTarget; ControlTarget::ChromeTab is the authoritative surface key.
            pid: 0,
            window_id: tab_id as u64,
            window_title: title,
        },
        window_frame: Frame {
            x: frame.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0),
            y: frame.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0),
            width: frame.get("width").and_then(|v| v.as_f64()).unwrap_or(0.0),
            height: frame.get("height").and_then(|v| v.as_f64()).unwrap_or(0.0),
        },
        model_size: ModelSize {
            width: model.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            height: model.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        },
        elements,
        transform_id: TransformId::new(),
        surface_scope: Some(format!("chrome:profile:{profile}:tab:{tab_id}:url:{page_url}")),
        image_hash: Some(image_hash),
        capture_backend: Some(
            obs.get("capture_backend")
                .and_then(|v| v.as_str())
                .unwrap_or("chrome_debugger_cdp")
                .to_string(),
        ),
        // CDP Page.captureScreenshot base64 (no data: URL prefix).
        // Never fabricate a placeholder PNG — that masks stale extension builds
        // and broken observation paths as valid VLM input. Fail closed so the
        // operator reloads the unpacked extension instead of burning step budget.
        image_png: Some(image_png),
    })
}

/// Serialize lcu_core::Action into the extension act() JSON shape.
pub fn action_to_extension_json(action: &Action) -> LcuResult<Value> {
    // Use serde representation then normalize Semantic/Targeted tag layout for the
    // extension dispatcher (expects kind + type fields flattened for variants).
    let v = serde_json::to_value(action).map_err(|e| {
        LcuError::coded(ErrorCode::InternalError, format!("serialize action: {e}"))
    })?;
    // serde of Action::Semantic(SemanticAction::Invoke{..}) yields
    // { "kind": "semantic", "type": "invoke", "element_id": "..." } with internally tagged
    // SemanticAction — which is already what the extension expects.
    if v.get("kind").and_then(|k| k.as_str()).is_none() {
        return Err(LcuError::coded(
            ErrorCode::InternalError,
            "action json missing kind",
        ));
    }
    Ok(v)
}

fn receipt(action: &Action, success: bool, message: Option<&str>) -> ActionReceipt {
    let capability_used = match action {
        Action::Semantic(_) => CapabilityLevel::Semantic,
        Action::Targeted(_) => CapabilityLevel::Targeted,
        _ => CapabilityLevel::Semantic,
    };
    ActionReceipt {
        action: action.clone(),
        action_hash: action.action_hash(),
        capability_used,
        risk_level: RiskLevel::R1,
        success,
        message: message.map(|s| s.to_string()),
        executed_at: Utc::now(),
    }
}

/// Map a raw control-state string from the extension/host.
pub fn parse_control_state(raw: &str) -> Option<ControlState> {
    match raw {
        "none" => Some(ControlState::None),
        "taken_over" => Some(ControlState::TakenOver),
        "target_lost" => Some(ControlState::TargetLost),
        _ => None,
    }
}

/// Build ControlTarget from a claimed ChromeTab.
pub fn control_target_for_tab(tab: &ChromeTab) -> ControlTarget {
    ControlTarget::ChromeTab(tab.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigate_uses_existing_rpc() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("chrome.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "navigate");
            assert_eq!(request["params"]["url"], "https://example.com/path");
            let response = json!({
                "id": request["id"],
                "ok": true,
                "result": { "url": request["params"]["url"] }
            });
            writeln!(stream, "{response}").unwrap();
        });

        let mut surface = ChromeTabSurface::new(ChromeControlClient::new(socket));
        surface.tab = Some(ChromeTab::new("Default", 1));
        let receipt = surface
            .act(&Action::Semantic(SemanticAction::Navigate {
                url: "https://example.com/path".into(),
            }))
            .unwrap();
        assert!(receipt.success);
        server.join().unwrap();
    }

    #[test]
    fn control_state_blocks_auto_after_takeover() {
        let mut surface =
            ChromeTabSurface::new(ChromeControlClient::new("/tmp/lcu-test-missing.sock"));
        surface.tab = Some(ChromeTab::new("Default", 1));
        assert!(surface.control_state.allows_auto_control());
        surface.apply_control_state(ControlState::TakenOver);
        assert!(!surface.control_state.allows_auto_control());
        surface.apply_control_state(ControlState::TargetLost);
        assert_eq!(surface.control_state, ControlState::TargetLost);
        surface.clear_local();
        assert!(surface.control_state.allows_auto_control());
        assert!(surface.tab.is_none());
    }
}
