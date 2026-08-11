//! macOS platform backend — **thin adapter** to the native Swift
//! `macos-window-service` (per-user private Unix socket + JSON).
//!
//! Wave 2 D2:
//! - Observation: window screenshot, AX state, stable `MacWindow(pid, window_id)`
//! - Actions: AX semantic + PID/window directed input (no real mouse move)
//! - Same-window user takeover → `ControlState::TakenOver`
//! - Removed: frontmost-app conflict model, TextEdit AppleScript specials, global HID
//!   non-exclusive fallbacks

pub mod client;

use std::sync::Mutex;

use base64::Engine;
use chrono::Utc;
use lcu_core::action::{Action, SemanticAction, TargetedInput};
use lcu_core::capability::CapabilityLevel;
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::{
    AppObservation, AppSelector, AppTarget, ElementNode, ModelSize, ObservationId, Rect, TransformId,
};
use lcu_core::risk::RiskLevel;
use lcu_core::task::ActionReceipt;
use lcu_core::types::Frame;
use lcu_core::surface::ControlState;
use lcu_platform::{PermissionFlag, PermissionState, PlatformBackend};
use serde_json::{json, Value};

use crate::client::NativeClient;

/// Live macOS backend. All OS effects go through the native socket service.
pub struct MacosBackend {
    /// When true, refuse real OS effects (unit tests / dry-run).
    pub skeleton_only: bool,
    client: NativeClient,
    /// Last resolved window frame for exclusive/targeted mapping fallbacks.
    last_frame: Mutex<Option<Frame>>,
}

impl Default for MacosBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl MacosBackend {
    pub fn new() -> Self {
        Self {
            skeleton_only: false,
            client: NativeClient::default(),
            last_frame: Mutex::new(None),
        }
    }

    /// Connect to an explicit socket (tests / alternate runtime layout).
    pub fn with_socket(path: impl Into<std::path::PathBuf>, auto_spawn: bool) -> Self {
        Self {
            skeleton_only: false,
            client: NativeClient::new(path, auto_spawn),
            last_frame: Mutex::new(None),
        }
    }

    pub fn skeleton() -> Self {
        Self {
            skeleton_only: true,
            client: NativeClient::new(client::default_socket_path(), false),
            last_frame: Mutex::new(None),
        }
    }

    fn ensure_live(&self) -> LcuResult<()> {
        if self.skeleton_only {
            return Err(LcuError::coded(
                ErrorCode::NotImplemented,
                "MacosBackend skeleton_only=true; refusing OS effects",
            ));
        }
        Ok(())
    }

    pub fn socket_path(&self) -> &std::path::Path {
        self.client.socket_path()
    }

    /// Session / process health via unified `detect_conflict` control-state RPC.
    pub fn session_health(&self, target: &AppTarget) -> SessionHealth {
        if self.skeleton_only {
            return SessionHealth {
                target_process_alive: false,
                window_exists: false,
                control_state: "none".into(),
                notes: vec!["skeleton_only".into()],
            };
        }
        // Same RPC as detect_user_conflict — service merged session_health fields in.
        match self.client.call(
            "detect_conflict",
            Some(json!({
                "pid": target.pid,
                "window_id": target.window_id,
            })),
        ) {
            Ok(v) => SessionHealth {
                target_process_alive: v
                    .get("target_process_alive")
                    .and_then(|x| x.as_bool())
                    .unwrap_or_else(|| process_alive(target.pid)),
                window_exists: v
                    .get("window_exists")
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false),
                control_state: v
                    .get("control_state")
                    .and_then(|x| x.as_str())
                    .unwrap_or("none")
                    .to_string(),
                notes: vec![],
            },
            Err(e) => SessionHealth {
                target_process_alive: process_alive(target.pid),
                window_exists: false,
                control_state: "unknown".into(),
                notes: vec![format!("detect_conflict rpc failed: {e}")],
            },
        }
    }

    /// Whether automation must stop (same-window takeover or target lost).
    pub fn must_pause_before_action(&self, target: &AppTarget) -> LcuResult<bool> {
        Ok(self.detect_user_conflict(target)?.is_blocking())
    }

    /// Ensure the native window service is up (auto-spawn when binary is found).
    pub fn ensure_service(&self) -> LcuResult<()> {
        self.ensure_live()?;
        let _ = self.client.call("ping", None)?;
        Ok(())
    }

    /// Doctor / readiness line for the macOS window surface.
    pub fn doctor_note(&self) -> String {
        if self.skeleton_only {
            return "mac_window: skeleton_only (no OS effects)".into();
        }
        let path = self.socket_path().display();
        match self.client.call("ping", None) {
            Ok(v) => {
                let ver = v
                    .get("version")
                    .and_then(|x| x.as_u64())
                    .or_else(|| v.get("service").and_then(|_| Some(1)))
                    .unwrap_or(1);
                let perms = self
                    .permission_state()
                    .map(|p| {
                        format!(
                            "ax={:?} screen={:?}",
                            p.accessibility, p.screen_recording
                        )
                    })
                    .unwrap_or_else(|_| "perms=?".into());
                format!("mac_window connected sock={path} service_v{ver} {perms}")
            }
            Err(e) => format!("mac_window offline sock={path} err={e}"),
        }
    }
}

impl PlatformBackend for MacosBackend {
    fn resolve_target(&self, selector: &AppSelector) -> LcuResult<AppTarget> {
        self.ensure_live()?;
        let mut params = json!({});
        if let Some(ref id) = selector.app_id {
            params["app_id"] = json!(id);
        }
        if let Some(pid) = selector.pid {
            params["pid"] = json!(pid);
        }
        if let Some(ref t) = selector.window_title_contains {
            params["window_title_contains"] = json!(t);
        }
        let v = self.client.call("resolve", Some(params))?;
        parse_app_target(&v)
    }

    fn observe(&self, target: &AppTarget) -> LcuResult<AppObservation> {
        self.ensure_live()?;
        let v = self.client.call(
            "observe",
            Some(json!({
                "pid": target.pid,
                "window_id": target.window_id,
                "max_width": 1440,
                "max_height": 900,
            })),
        )?;

        if let Some(error) = v.get("semantic_error").and_then(|x| x.as_str()) {
            tracing::warn!(
                pid = target.pid,
                window_id = target.window_id,
                error,
                "macOS semantic observation unavailable; using screenshot-only observation"
            );
        }

        // Surface backend control state immediately on observe (no multi-layer session).
        if let Some(cs) = v.get("control_state").and_then(|c| c.as_str()) {
            match cs {
                "target_lost" => {
                    return Err(LcuError::coded(
                        ErrorCode::TaskFailed,
                        "target process/window lost",
                    ));
                }
                "taken_over" => {
                    return Err(LcuError::coded(
                        ErrorCode::WaitingUser,
                        "user focused the same window (taken_over)",
                    ));
                }
                _ => {}
            }
        }

        let resolved = v
            .get("target")
            .map(parse_app_target)
            .transpose()?
            .unwrap_or_else(|| target.clone());

        let window_frame = parse_frame(v.get("window_frame")).unwrap_or(Frame {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        });
        *self.last_frame.lock().expect("frame") = Some(window_frame);

        let model_size = ModelSize {
            width: v
                .pointer("/model_size/width")
                .and_then(|x| x.as_u64())
                .unwrap_or(window_frame.width.max(1.0) as u64) as u32,
            height: v
                .pointer("/model_size/height")
                .and_then(|x| x.as_u64())
                .unwrap_or(window_frame.height.max(1.0) as u64) as u32,
        };

        let elements = parse_elements(v.get("elements"));
        let image_hash = v
            .get("image_hash")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let capture_backend = v
            .get("capture_backend")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string());
        let image_png = v
            .get("image_png_b64")
            .and_then(|x| x.as_str())
            .and_then(|b64| base64::engine::general_purpose::STANDARD.decode(b64).ok());

        Ok(AppObservation {
            observation_id: ObservationId::new(),
            timestamp_ms: Utc::now().timestamp_millis(),
            target: resolved,
            window_frame,
            model_size,
            elements,
            transform_id: TransformId::new(),
            image_hash,
            capture_backend,
            image_png,
        })
    }

    fn perform_semantic_action(
        &self,
        target: &AppTarget,
        action: &SemanticAction,
    ) -> LcuResult<ActionReceipt> {
        let action_json = semantic_to_json(action)?;
        self.ensure_live()?;
        let v = self.client.call(
            "semantic",
            Some(json!({
                "pid": target.pid,
                "window_id": target.window_id,
                "action": action_json,
            })),
        )?;
        let path = v
            .get("path")
            .and_then(|x| x.as_str())
            .unwrap_or("semantic");
        let detail = v
            .get("detail")
            .and_then(|x| x.as_str())
            .unwrap_or("ok");
        Ok(receipt(
            Action::Semantic(action.clone()),
            CapabilityLevel::Semantic,
            risk_for_semantic(action),
            true,
            format!("{path}: {detail}"),
        ))
    }

    fn perform_targeted_input(
        &self,
        target: &AppTarget,
        action: &TargetedInput,
    ) -> LcuResult<ActionReceipt> {
        self.ensure_live()?;
        let action_json = targeted_to_json(action)?;
        let v = self.client.call(
            "targeted",
            Some(json!({
                "pid": target.pid,
                "window_id": target.window_id,
                "action": action_json,
            })),
        )?;
        let path = v
            .get("path")
            .and_then(|x| x.as_str())
            .unwrap_or("targeted");
        let detail = v
            .get("detail")
            .and_then(|x| x.as_str())
            .unwrap_or("ok");
        Ok(receipt(
            Action::Targeted(action.clone()),
            CapabilityLevel::Targeted,
            RiskLevel::R3,
            true,
            format!("{path}: {detail}"),
        ))
    }

    fn perform_exclusive_input(
        &self,
        _target: &AppTarget,
        _action: &TargetedInput,
    ) -> LcuResult<ActionReceipt> {
        self.ensure_live()?;
        // D2: exclusive global HID is not part of the window surface path.
        // Surfaces refuse Exclusive by contract (`action_is_surface_applicable`).
        Err(LcuError::coded(
            ErrorCode::UnsupportedCapability,
            "exclusive global HID input removed from macOS window surface; use semantic/targeted directed input",
        ))
    }

    fn detect_user_conflict(&self, target: &AppTarget) -> LcuResult<ControlState> {
        self.ensure_live()?;
        // Same-window takeover only (not frontmost-app).
        let v = self.client.call(
            "detect_conflict",
            Some(json!({
                "pid": target.pid,
                "window_id": target.window_id,
            })),
        )?;
        let conflict = v
            .get("conflict")
            .and_then(|c| c.as_str())
            .or_else(|| v.get("control_state").and_then(|c| c.as_str()))
            .unwrap_or("none");
        Ok(match conflict {
            "user_active_in_target" | "taken_over" => ControlState::TakenOver,
            "target_lost" => ControlState::TargetLost,
            _ => ControlState::None,
        })
    }

    fn permission_state(&self) -> LcuResult<PermissionState> {
        if self.skeleton_only {
            // Local probe is not available without the service; report undetermined.
            return Ok(PermissionState {
                screen_recording: PermissionFlag::NotDetermined,
                accessibility: PermissionFlag::NotDetermined,
                input_monitoring: PermissionFlag::NotDetermined,
            });
        }
        match self.client.call("permissions", None) {
            Ok(v) => Ok(PermissionState {
                screen_recording: parse_perm_flag(v.get("screen_recording")),
                accessibility: parse_perm_flag(v.get("accessibility")),
                input_monitoring: parse_perm_flag(v.get("input_monitoring")),
            }),
            Err(_) => Ok(PermissionState {
                screen_recording: PermissionFlag::NotDetermined,
                accessibility: PermissionFlag::NotDetermined,
                input_monitoring: PermissionFlag::NotDetermined,
            }),
        }
    }

    fn set_agent_session(&self, _target: &AppTarget, _active: bool) -> LcuResult<()> {
        // D2: no frontmost-app agent lease. Same-window takeover is detected by the service.
        Ok(())
    }

    fn doctor_surface_notes(&self) -> Vec<String> {
        vec![self.doctor_note()]
    }

    fn ensure_surfaces(&self) -> LcuResult<()> {
        if self.skeleton_only {
            return Ok(());
        }
        // Best-effort: log and continue if binary missing (doctor reports offline).
        match self.ensure_service() {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "macos-window-service not ready at startup");
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SessionHealth {
    pub target_process_alive: bool,
    pub window_exists: bool,
    pub control_state: String,
    pub notes: Vec<String>,
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn parse_app_target(v: &Value) -> LcuResult<AppTarget> {
    let app_id = v
        .get("app_id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let pid = v
        .get("pid")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| LcuError::coded(ErrorCode::InternalError, "resolve missing pid"))?
        as u32;
    let window_id = v
        .get("window_id")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| LcuError::coded(ErrorCode::InternalError, "resolve missing window_id"))?;
    let window_title = v
        .get("window_title")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    Ok(AppTarget {
        app_id,
        pid,
        window_id,
        window_title,
    })
}

fn parse_frame(v: Option<&Value>) -> Option<Frame> {
    let v = v?;
    Some(Frame {
        x: v.get("x").and_then(|x| x.as_f64()).unwrap_or(0.0),
        y: v.get("y").and_then(|x| x.as_f64()).unwrap_or(0.0),
        width: v.get("width").and_then(|x| x.as_f64()).unwrap_or(1.0),
        height: v.get("height").and_then(|x| x.as_f64()).unwrap_or(1.0),
    })
}

fn parse_elements(v: Option<&Value>) -> Vec<ElementNode> {
    let Some(Value::Array(arr)) = v else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|el| {
            let id = el.get("id")?.as_str()?.to_string();
            let role = el
                .get("role")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let label = el
                .get("label")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            let value = el
                .get("value")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string());
            let frame = el.get("frame");
            let rect = Rect {
                x: frame
                    .and_then(|f| f.get("x"))
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0),
                y: frame
                    .and_then(|f| f.get("y"))
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0),
                width: frame
                    .and_then(|f| f.get("width"))
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0),
                height: frame
                    .and_then(|f| f.get("height"))
                    .and_then(|x| x.as_f64())
                    .unwrap_or(0.0),
            };
            let actions = el
                .get("actions")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            Some(ElementNode {
                id,
                role,
                label,
                value,
                frame: rect,
                actions,
            })
        })
        .collect()
}

fn parse_perm_flag(v: Option<&Value>) -> PermissionFlag {
    match v.and_then(|x| x.as_str()).unwrap_or("not_determined") {
        "granted" => PermissionFlag::Granted,
        "denied" => PermissionFlag::Denied,
        "unsupported" => PermissionFlag::Unsupported,
        _ => PermissionFlag::NotDetermined,
    }
}

fn semantic_to_json(action: &SemanticAction) -> LcuResult<Value> {
    Ok(match action {
        SemanticAction::Navigate { .. } => {
            return Err(LcuError::coded(
                ErrorCode::UnsupportedCapability,
                "navigate is only supported by the ChromeTab surface",
            ));
        }
        SemanticAction::Invoke { element_id } => json!({
            "type": "invoke",
            "element_id": element_id,
        }),
        SemanticAction::SetValue { element_id, value } => json!({
            "type": "set_value",
            "element_id": element_id,
            "value": value,
        }),
        SemanticAction::Focus { element_id } => json!({
            "type": "focus",
            "element_id": element_id,
        }),
        SemanticAction::Scroll {
            element_id,
            delta_x,
            delta_y,
        } => json!({
            "type": "scroll",
            "element_id": element_id,
            "delta_x": delta_x,
            "delta_y": delta_y,
        }),
    })
}

fn targeted_to_json(action: &TargetedInput) -> LcuResult<Value> {
    Ok(match action {
        TargetedInput::Click { x, y, button } => {
            let button = match button {
                lcu_core::action::MouseButton::Left => "left",
                lcu_core::action::MouseButton::Right => "right",
                lcu_core::action::MouseButton::Middle => "middle",
            };
            json!({
                "type": "click",
                "x": x,
                "y": y,
                "button": button,
            })
        }
        TargetedInput::TypeText { text } => json!({
            "type": "type_text",
            "text": text,
        }),
        TargetedInput::KeyCombo { keys } => json!({
            "type": "key_combo",
            "keys": keys,
        }),
    })
}

fn risk_for_semantic(action: &SemanticAction) -> RiskLevel {
    match action {
        SemanticAction::Focus { .. } => RiskLevel::R0,
        SemanticAction::Navigate { .. }
        | SemanticAction::Invoke { .. }
        | SemanticAction::Scroll { .. } => RiskLevel::R1,
        SemanticAction::SetValue { .. } => RiskLevel::R2,
    }
}

fn receipt(
    action: Action,
    capability_used: CapabilityLevel,
    risk_level: RiskLevel,
    success: bool,
    message: impl Into<String>,
) -> ActionReceipt {
    let action_hash = action.action_hash();
    ActionReceipt {
        action,
        action_hash,
        capability_used,
        risk_level,
        success,
        message: Some(message.into()),
        executed_at: Utc::now(),
    }
}

pub const PLATFORM_ID: &str = "macos";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_refuses_actions() {
        let backend = MacosBackend::skeleton();
        assert_eq!(PLATFORM_ID, "macos");
        let err = backend
            .resolve_target(&AppSelector {
                app_id: Some("com.apple.finder".into()),
                pid: None,
                window_title_contains: None,
            })
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotImplemented);
    }





    #[test]
    fn exclusive_refused_when_live_path_would_run() {
        // skeleton refuses earlier; check error code via skeleton.
        let backend = MacosBackend::skeleton();
        let err = backend
            .perform_exclusive_input(
                &AppTarget {
                    app_id: "x".into(),
                    pid: 1,
                    window_id: 1,
                    window_title: "".into(),
                },
                &TargetedInput::TypeText {
                    text: "no".into(),
                },
            )
            .unwrap_err();
        assert_eq!(err.code(), ErrorCode::NotImplemented);
    }

    #[test]
    fn navigate_is_explicitly_unsupported() {
        let err = semantic_to_json(&SemanticAction::Navigate {
            url: "https://example.com".into(),
        })
        .unwrap_err();
        assert_eq!(err.code(), ErrorCode::UnsupportedCapability);
    }
}
