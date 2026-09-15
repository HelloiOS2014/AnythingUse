//! Risk levels for side-effect policy.

use serde::{Deserialize, Serialize};

/// Independent risk classification used by EffectGuard.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    /// Observation only.
    R0,
    /// Reversible navigation.
    R1,
    /// Local reversible modification.
    R2,
    /// External or hard-to-reverse side effect; requires per-action approval.
    R3,
    /// Security / financial / credential; user takeover only.
    R4,
}

impl RiskLevel {
    pub fn requires_per_action_approval(self) -> bool {
        matches!(self, Self::R3 | Self::R4)
    }

    pub fn requires_user_takeover(self) -> bool {
        matches!(self, Self::R4)
    }
}

