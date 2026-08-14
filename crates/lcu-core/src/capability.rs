//! Capability levels returned by platform backends.

use serde::{Deserialize, Serialize};

/// Actual capability used for an observation or action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityLevel {
    /// Semantic accessibility / automation API.
    Semantic,
    /// Application- or window-targeted input without global pointer grab.
    Targeted,
    /// Capability is not available for this target.
    Unsupported,
}

impl CapabilityLevel {
    pub fn is_non_interfering(self) -> bool {
        matches!(self, Self::Semantic | Self::Targeted)
    }
}

