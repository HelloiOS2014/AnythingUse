//! Platform backend trait. Implementations live in platform-specific crates.
//!
//! `lcu-core` must never depend on this crate's reverse direction into OS APIs.
//!
//! Runtime's only backend interface is [`PlatformBackend`]. Duplicate surface
//! routers / session maps were removed; backends own release and takeover state.

use lcu_core::action::{SemanticAction, TargetedInput};
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::{AppObservation, AppSelector, AppTarget};
use lcu_core::surface::{ChromeTab, ControlState};
use lcu_core::task::ActionReceipt;
use serde::{Deserialize, Serialize};

/// Platform permission snapshot for doctor and runtime gating.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionState {
    pub screen_recording: PermissionFlag,
    pub accessibility: PermissionFlag,
    pub input_monitoring: PermissionFlag,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionFlag {
    Granted,
    Denied,
    NotDetermined,
    Unsupported,
}

/// Unified platform capability surface used by Runtime only.
pub trait PlatformBackend: Send + Sync {
    /// Optional product-surface capacity key reserved before target resolution.
    /// Backends returning the same key are serialized while unrelated surfaces run.
    fn serial_surface_key(&self, _selector: &AppSelector) -> Option<String> {
        None
    }

    fn resolve_target(&self, selector: &AppSelector) -> LcuResult<AppTarget>;

    fn observe(&self, target: &AppTarget) -> LcuResult<AppObservation>;

    fn perform_semantic_action(
        &self,
        target: &AppTarget,
        action: &SemanticAction,
    ) -> LcuResult<ActionReceipt>;

    fn perform_targeted_input(
        &self,
        target: &AppTarget,
        action: &TargetedInput,
    ) -> LcuResult<ActionReceipt>;

    /// Report backend control state for `target` (`None` / `TakenOver` / `TargetLost`).
    fn detect_user_conflict(&self, target: &AppTarget) -> LcuResult<ControlState>;

    fn permission_state(&self) -> LcuResult<PermissionState>;

    /// Stable identity used by persistent app-access decisions.
    fn stable_app_identity(&self, target: &AppTarget) -> LcuResult<String> {
        Ok(target.app_id.clone())
    }

    /// Arm/disarm real-user HID takeover detection for this exact target.
    fn set_takeover_watch(&self, _target: &AppTarget, _active: bool) -> LcuResult<()> {
        Ok(())
    }

    /// Monotonic local-login/session generation. Zero means unsupported.
    fn control_epoch(&self) -> LcuResult<u64> {
        Ok(0)
    }

    /// Bring the exact permitted target to the front. The next action must use a
    /// fresh observation; this method never executes the rejected old action.
    fn activate_target(&self, _target: &AppTarget) -> LcuResult<()> {
        Err(LcuError::coded(
            ErrorCode::ForegroundRequired,
            "foreground activation is unsupported by this backend",
        ))
    }

    /// Bind product-loop goal/task context so surface adapters can claim Chrome tabs.
    fn bind_task_context(&self, _goal: &str, _task_id: Option<&str>) {}

    /// Clear task-scoped surface state (Chrome tab lease, claim URL).
    fn clear_task_context(&self) {}

    /// Release backend ownership of `target` (complete / cancel / fail / takeover).
    fn release(&self, target: &AppTarget) -> LcuResult<()> {
        let _ = self.set_takeover_watch(target, false);
        Ok(())
    }

    /// If the backend has claimed a Chrome tab for `target`, return it.
    fn chrome_tab_for(&self, _target: &AppTarget) -> Option<ChromeTab> {
        None
    }

    /// Connectivity / readiness notes for `lcu doctor`.
    fn doctor_surface_notes(&self) -> Vec<String> {
        Vec::new()
    }

    /// Ensure native services the backend owns are reachable.
    fn ensure_surfaces(&self) -> LcuResult<()> {
        Ok(())
    }
}

/// Backend that refuses all real system actions. Used in unit tests.
#[derive(Debug, Default, Clone)]
pub struct NullBackend;

impl PlatformBackend for NullBackend {
    fn resolve_target(&self, _selector: &AppSelector) -> LcuResult<AppTarget> {
        Err(LcuError::coded(
            ErrorCode::NotImplemented,
            "NullBackend cannot resolve targets; no real system action",
        ))
    }

    fn observe(&self, _target: &AppTarget) -> LcuResult<AppObservation> {
        Err(LcuError::coded(
            ErrorCode::NotImplemented,
            "NullBackend cannot observe; all system actions must go through a real backend via Runtime",
        ))
    }

    fn perform_semantic_action(
        &self,
        _target: &AppTarget,
        _action: &SemanticAction,
    ) -> LcuResult<ActionReceipt> {
        Err(LcuError::coded(
            ErrorCode::NotImplemented,
            "NullBackend blocks semantic actions",
        ))
    }

    fn perform_targeted_input(
        &self,
        _target: &AppTarget,
        _action: &TargetedInput,
    ) -> LcuResult<ActionReceipt> {
        Err(LcuError::coded(
            ErrorCode::NotImplemented,
            "NullBackend blocks targeted input",
        ))
    }

    fn detect_user_conflict(&self, _target: &AppTarget) -> LcuResult<ControlState> {
        Ok(ControlState::None)
    }

    fn permission_state(&self) -> LcuResult<PermissionState> {
        Ok(PermissionState {
            screen_recording: PermissionFlag::NotDetermined,
            accessibility: PermissionFlag::NotDetermined,
            input_monitoring: PermissionFlag::NotDetermined,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_backend_never_performs_real_actions() {
        let backend = NullBackend;
        let selector = AppSelector {
            app_id: Some("com.google.Chrome".into()),
            pid: None,
            window_title_contains: None,
        };
        assert!(backend.resolve_target(&selector).is_err());
        let perms = backend.permission_state().unwrap();
        assert_eq!(perms.screen_recording, PermissionFlag::NotDetermined);
    }

    #[test]
    fn control_state_apply_priority() {
        assert!(ControlState::None.allows_auto_control());
        assert!(!ControlState::TakenOver.allows_auto_control());
        assert_eq!(
            ControlState::TakenOver.apply(ControlState::TargetLost),
            ControlState::TargetLost
        );
    }
}
