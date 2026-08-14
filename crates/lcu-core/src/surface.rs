//! Shared control-surface types: window/tab identity and backend control state.
//!
//! External `lcu` CLI / JSON / Skill shapes are unchanged. No session map, registry,
//! or multi-layer control state machine lives here.

use serde::{Deserialize, Serialize};

use crate::observation::{AppObservation, AppTarget};

// ---------------------------------------------------------------------------
// Target surfaces
// ---------------------------------------------------------------------------

/// macOS window target: process + CGWindow id.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct MacWindow {
    pub pid: u32,
    pub window_id: u64,
}

impl MacWindow {
    pub fn new(pid: u32, window_id: u64) -> Self {
        Self { pid, window_id }
    }

    pub fn matches_app_target(&self, target: &AppTarget) -> bool {
        self.pid == target.pid && self.window_id == target.window_id
    }
}

impl From<&AppTarget> for MacWindow {
    fn from(t: &AppTarget) -> Self {
        Self {
            pid: t.pid,
            window_id: t.window_id,
        }
    }
}

impl From<AppTarget> for MacWindow {
    fn from(t: AppTarget) -> Self {
        Self::from(&t)
    }
}

/// Chrome tab target: profile/session key + extension tab id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ChromeTab {
    /// Chrome user profile directory name or session key (e.g. `"Default"`, `"Profile 1"`).
    pub profile: String,
    /// Chrome extension tab id (`chrome.tabs` / debugger target).
    pub tab_id: i64,
}

impl ChromeTab {
    pub fn new(profile: impl Into<String>, tab_id: i64) -> Self {
        Self {
            profile: profile.into(),
            tab_id,
        }
    }
}

/// Resolved control target for a surface (window or tab).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "surface", rename_all = "snake_case")]
pub enum ControlTarget {
    MacWindow(MacWindow),
    ChromeTab(ChromeTab),
}

impl ControlTarget {
    pub fn mac_window(pid: u32, window_id: u64) -> Self {
        Self::MacWindow(MacWindow::new(pid, window_id))
    }

    pub fn chrome_tab(profile: impl Into<String>, tab_id: i64) -> Self {
        Self::ChromeTab(ChromeTab::new(profile, tab_id))
    }

    pub fn from_app_target(target: &AppTarget) -> Self {
        Self::MacWindow(MacWindow::from(target))
    }

    /// Stable key for logging / diagnostics (not a lease table).
    pub fn surface_key(&self) -> String {
        match self {
            Self::MacWindow(w) => format!("mac_window:{}:{}", w.pid, w.window_id),
            Self::ChromeTab(t) => format!("chrome_tab:{}:{}", t.profile, t.tab_id),
        }
    }

    pub fn is_mac_window(&self) -> bool {
        matches!(self, Self::MacWindow(_))
    }

    pub fn is_chrome_tab(&self) -> bool {
        matches!(self, Self::ChromeTab(_))
    }
}

// ---------------------------------------------------------------------------
// Single control vocabulary (backends report; Runtime consumes directly)
// ---------------------------------------------------------------------------

/// Backend control state for a single target.
///
/// Runtime auto-acts only while `None`. `TakenOver` / `TargetLost` stop automation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum ControlState {
    /// Agent may hold/control the target (no user takeover, target still valid).
    #[default]
    None,
    /// User operated the same window/tab; agent yields.
    TakenOver,
    /// Process, window, or tab is gone / unresolvable.
    TargetLost,
}

impl ControlState {
    /// Whether Runtime may continue automatic observe/act on this target.
    pub fn allows_auto_control(self) -> bool {
        matches!(self, Self::None)
    }

    /// Whether automation is stopped due to user or target failure.
    pub fn is_blocking(self) -> bool {
        !self.allows_auto_control()
    }

    /// Apply a backend-reported state. `TargetLost` wins over `TakenOver`.
    pub fn apply(self, next: ControlState) -> ControlState {
        match next {
            ControlState::None => self,
            ControlState::TakenOver if self != ControlState::TargetLost => ControlState::TakenOver,
            ControlState::TakenOver => self,
            ControlState::TargetLost => ControlState::TargetLost,
        }
    }
}

// ---------------------------------------------------------------------------
// Mapping helpers
// ---------------------------------------------------------------------------

/// Map a legacy app observation to the surface control target (macOS window path).
pub fn control_target_from_observation(obs: &AppObservation) -> ControlTarget {
    ControlTarget::from_app_target(&obs.target)
}

/// Map a resolved `AppTarget` to `MacWindow` / `ControlTarget`.
pub fn control_target_from_app_target(target: &AppTarget) -> ControlTarget {
    ControlTarget::from_app_target(target)
}
