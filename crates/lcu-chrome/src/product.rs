//! Product platform backend: routes an explicitly targeted Chrome app identity
//! to the Chrome tab surface and everything else to the macOS window backend.
//!
//! Realignment contract §4.1/§5: the surface is chosen from the explicit
//! target/surface only — goal text is never parsed for Chrome/browser keywords.

use std::sync::{Arc, Mutex};

use lcu_core::action::{SemanticAction, TargetedInput};
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::{AppObservation, AppSelector, AppTarget};
use lcu_core::surface::ChromeTab;
use lcu_core::task::{ActionReceipt, TaskId};
use lcu_core::surface::ControlState;
use lcu_platform::{PermissionState, PlatformBackend};

use crate::{ChromeControlClient, ChromeTabSurface};

/// Routes `com.google.Chrome` (and Chrome-like goals) through CDP/extension;
/// all other apps through the macOS window `PlatformBackend`.
pub struct ProductBackend {
    window: Arc<dyn PlatformBackend>,
    chrome_client: ChromeControlClient,
    state: Mutex<ProductState>,
}

#[derive(Default)]
struct ProductState {
    /// Goal bound for the current product step (Chrome vs window routing).
    goal: Option<String>,
    task_id: Option<String>,
    /// Active Chrome tab surface for the serial worker (at most one).
    chrome: Option<ActiveChrome>,
}

struct ActiveChrome {
    task_id: Option<String>,
    surface: ChromeTabSurface,
    tab: ChromeTab,
}

impl ProductBackend {
    pub fn new(window: Arc<dyn PlatformBackend>, chrome_client: ChromeControlClient) -> Self {
        Self {
            window,
            chrome_client,
            state: Mutex::new(ProductState::default()),
        }
    }

    pub fn with_defaults(window: Arc<dyn PlatformBackend>) -> LcuResult<Self> {
        Ok(Self::new(window, ChromeControlClient::with_default_path()?))
    }

    pub fn chrome_client(&self) -> &ChromeControlClient {
        &self.chrome_client
    }

    pub fn window_backend(&self) -> &dyn PlatformBackend {
        self.window.as_ref()
    }

    /// Exact explicit Chrome app identities (surface routing only — never a
    /// substring or goal-keyword match, and never an execution-policy special).
    fn is_chrome_app(app_id: &str) -> bool {
        matches!(app_id, "com.google.Chrome" | "com.google.Chrome.canary")
    }

    fn chrome_available(&self) -> bool {
        self.chrome_client.socket_present() && self.chrome_client.host_ping().is_ok()
    }

    fn ensure_chrome_claimed(&self, target: &AppTarget) -> LcuResult<()> {
        let mut st = self.state.lock().expect("product state");
        if st.chrome.is_some() {
            return Ok(());
        }
        if !Self::is_chrome_app(&target.app_id) {
            return Ok(());
        }
        if !self.chrome_client.socket_present() {
            return Err(LcuError::coded(
                ErrorCode::RuntimeUnavailable,
                "Chrome control socket missing; run install-native-host.sh, then load the extension: path it prints in chrome://extensions",
            ));
        }

        let task_id = st.task_id.clone().map(TaskId);
        // Background task tab only. Must be a page `chrome.debugger` can attach to:
        // chrome:// and chrome-extension:// URLs are blocked by Chromium.
        // Start on a neutral https page (inactive tab); VLM navigates later.
        // Never use chrome://newtab — claim would fail and/or steal focus paths.
        let url = "https://example.com/";
        let mut surface = ChromeTabSurface::new(self.chrome_client.clone());
        let tab = surface.claim(url, task_id, None).map_err(|e| {
            LcuError::coded(
                e.code(),
                format!("chrome claim failed for url={url}: {e}"),
            )
        })?;
        st.chrome = Some(ActiveChrome {
            task_id: st.task_id.clone(),
            surface,
            tab,
        });
        Ok(())
    }

    fn with_chrome_mut<R>(
        &self,
        f: impl FnOnce(&mut ChromeTabSurface, &ChromeTab) -> LcuResult<R>,
    ) -> LcuResult<R> {
        let mut st = self.state.lock().expect("product state");
        let active = st.chrome.as_mut().ok_or_else(|| {
            LcuError::coded(
                ErrorCode::InvalidRequest,
                "no active Chrome tab surface; claim first",
            )
        })?;
        f(&mut active.surface, &active.tab)
    }

    fn release_chrome_locked(st: &mut ProductState) {
        if let Some(mut active) = st.chrome.take() {
            let _ = active.surface.release();
        }
    }
}

impl PlatformBackend for ProductBackend {
    fn serial_surface_key(&self, selector: &AppSelector) -> Option<String> {
        selector
            .app_id
            .as_deref()
            .filter(|app_id| Self::is_chrome_app(app_id))
            .map(|_| "chrome_extension_lease".into())
    }

    fn resolve_target(&self, selector: &AppSelector) -> LcuResult<AppTarget> {
        let goal = self
            .state
            .lock()
            .expect("product state")
            .goal
            .clone();
        let chrome_explicit = selector
            .app_id
            .as_deref()
            .map(Self::is_chrome_app)
            .unwrap_or(false);
        if chrome_explicit && self.chrome_available() {
            let app_id = selector
                .app_id
                .clone()
                .unwrap_or_else(|| "com.google.Chrome".into());
            // Claim on resolve so Runtime opens ControlTarget::ChromeTab (not mac_window:0:0).
            let provisional = AppTarget {
                app_id: app_id.clone(),
                pid: 0,
                window_id: 0,
                window_title: goal.clone().unwrap_or_default(),
            };
            self.ensure_chrome_claimed(&provisional)?;
            let tab = self.chrome_tab_for(&provisional).ok_or_else(|| {
                LcuError::coded(ErrorCode::InternalError, "chrome claim missing tab binding")
            })?;
            return Ok(AppTarget {
                app_id,
                pid: 0,
                window_id: tab.tab_id.max(0) as u64,
                window_title: goal.unwrap_or_default(),
            });
        }
        // Chrome explicitly targeted but host unavailable → clear error (do not fall back to AX).
        if chrome_explicit {
            return Err(LcuError::coded(
                ErrorCode::RuntimeUnavailable,
                "Chrome surface required but chrome-control host is not connected; \
                 run install-native-host.sh, then load the extension: path it prints in chrome://extensions",
            ));
        }
        self.window.resolve_target(selector)
    }

    fn observe(&self, target: &AppTarget) -> LcuResult<AppObservation> {
        if Self::is_chrome_app(&target.app_id) {
            self.ensure_chrome_claimed(target)?;
            let mut obs = self.with_chrome_mut(|surface, _tab| surface.observe())?;
            // Keep app_id stable for goal assertions / approval binding.
            if obs.target.app_id.is_empty() {
                obs.target.app_id = target.app_id.clone();
            }
            return Ok(obs);
        }
        self.window.observe(target)
    }

    fn perform_semantic_action(
        &self,
        target: &AppTarget,
        action: &SemanticAction,
    ) -> LcuResult<ActionReceipt> {
        if Self::is_chrome_app(&target.app_id) {
            self.ensure_chrome_claimed(target)?;
            return self.with_chrome_mut(|surface, _| surface.perform_semantic(action));
        }
        self.window.perform_semantic_action(target, action)
    }

    fn perform_targeted_input(
        &self,
        target: &AppTarget,
        action: &TargetedInput,
    ) -> LcuResult<ActionReceipt> {
        if Self::is_chrome_app(&target.app_id) {
            self.ensure_chrome_claimed(target)?;
            return self.with_chrome_mut(|surface, _| surface.perform_targeted(action));
        }
        self.window.perform_targeted_input(target, action)
    }

    fn detect_user_conflict(&self, target: &AppTarget) -> LcuResult<ControlState> {
        if Self::is_chrome_app(&target.app_id) {
            let mut st = self.state.lock().expect("product state");
            let Some(active) = st.chrome.as_mut() else {
                return Ok(ControlState::None);
            };
            return active.surface.refresh_control_state();
        }
        self.window.detect_user_conflict(target)
    }

    fn permission_state(&self) -> LcuResult<PermissionState> {
        self.window.permission_state()
    }

    fn stable_app_identity(&self, target: &AppTarget) -> LcuResult<String> {
        if Self::is_chrome_app(&target.app_id) {
            let ping = self.chrome_client.ping()?;
            let extension = ping
                .get("extensionId")
                .or_else(|| ping.get("extension_id"))
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    LcuError::coded(ErrorCode::TaskFailed, "Chrome identity missing extensionId")
                })?;
            let profile = ping
                .get("profile")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
                .or_else(|| self.chrome_tab_for(target).map(|tab| tab.profile))
                .ok_or_else(|| {
                    LcuError::coded(ErrorCode::TaskFailed, "Chrome identity missing profile key")
                })?;
            return Ok(format!("chrome:{extension}:profile:{profile}"));
        }
        self.window.stable_app_identity(target)
    }

    fn set_takeover_watch(&self, target: &AppTarget, active: bool) -> LcuResult<()> {
        if Self::is_chrome_app(&target.app_id) {
            return Ok(());
        }
        self.window.set_takeover_watch(target, active)
    }

    fn control_epoch(&self) -> LcuResult<u64> {
        self.window.control_epoch()
    }

    fn set_agent_session(&self, target: &AppTarget, active: bool) -> LcuResult<()> {
        if !Self::is_chrome_app(&target.app_id) {
            return self.window.set_agent_session(target, active);
        }
        // Chrome tab lease is released only via clear_task_context (task terminal).
        let _ = active;
        Ok(())
    }
    fn suspend_agent_session(&self, target: &AppTarget) -> LcuResult<()> {
        if Self::is_chrome_app(&target.app_id) {
            return Ok(());
        }
        self.window.suspend_agent_session(target)
    }

    fn resume_agent_session(&self, target: &AppTarget) -> LcuResult<()> {
        if Self::is_chrome_app(&target.app_id) {
            return Ok(());
        }
        self.window.resume_agent_session(target)
    }

    fn bind_task_context(&self, goal: &str, task_id: Option<&str>) {
        let mut st = self.state.lock().expect("product state");
        st.goal = Some(goal.to_string());
        st.task_id = task_id.map(|s| s.to_string());
        // Runtime owns the serial Chrome-surface reservation. A macOS task may
        // run while a Chrome task waits for its Actor and must not drop its tab.
    }

    fn clear_task_context(&self) {
        let mut st = self.state.lock().expect("product state");
        let owns_active = st.chrome.as_ref().is_some_and(|active| {
            active.task_id.as_deref() == st.task_id.as_deref()
        });
        if owns_active {
            Self::release_chrome_locked(&mut st);
        }
        st.goal = None;
        st.task_id = None;
    }

    fn release(&self, target: &AppTarget) -> LcuResult<()> {
        if Self::is_chrome_app(&target.app_id) {
            let mut st = self.state.lock().expect("product state");
            let matches = st.chrome.as_ref().is_some_and(|active| {
                active.tab.tab_id.max(0) as u64 == target.window_id
            });
            if matches {
                Self::release_chrome_locked(&mut st);
            }
            return Ok(());
        }
        self.window.release(target)
    }

    fn chrome_tab_for(&self, target: &AppTarget) -> Option<ChromeTab> {
        if !Self::is_chrome_app(&target.app_id) {
            return None;
        }
        self.state
            .lock()
            .expect("product state")
            .chrome
            .as_ref()
            .map(|a| a.tab.clone())
    }

    fn doctor_surface_notes(&self) -> Vec<String> {
        let mut notes = self.window.doctor_surface_notes();
        let sock = self.chrome_client.sock_path().display();
        if !self.chrome_client.socket_present() {
            notes.push(format!(
                "chrome_tab offline: socket missing at {sock} (install native host + load extension)"
            ));
            return notes;
        }
        match self.chrome_client.host_ping() {
            Ok(_) => match self.chrome_client.ping() {
                Ok(_) => notes.push(format!(
                    "chrome_tab connected sock={sock} host=ok extension=ok"
                )),
                Err(e) => notes.push(format!(
                    "chrome_tab host up sock={sock} extension not ready: {e}"
                )),
            },
            Err(e) => notes.push(format!(
                "chrome_tab socket present sock={sock} host_ping failed: {e}"
            )),
        }
        notes
    }

    fn ensure_surfaces(&self) -> LcuResult<()> {
        let _ = self.window.ensure_surfaces();
        // Chrome host is launched by Chrome via Native Messaging — do not spawn here.
        Ok(())
    }
}

impl Drop for ProductBackend {
    fn drop(&mut self) {
        let mut st = self.state.lock().expect("product state");
        Self::release_chrome_locked(&mut st);
    }
}
