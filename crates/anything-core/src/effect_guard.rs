//! Independent side-effect re-evaluation scaffolding — shared, platform-neutral.
//!
//! Actor effect declarations are structured safety classifications, never
//! authorization and never trusted as final risk. This module defines the
//! guard contract (`EffectGuard`), the evaluation inputs/outputs
//! (`EffectContext` / `EffectJudgement`), and the closed-set policy table
//! (`effect_policy`). It deliberately contains **no evidence heuristics**:
//! platform crates (macOS `lcu-core`, Android `lau`) implement the trait with
//! their own platform-normalized evidence (AX roles vs Android
//! `AccessibilityNodeInfo` capabilities).

use crate::action::{Action, EffectClaim};
use crate::observation::AppObservation;
use crate::risk::RiskLevel;
use serde::{Deserialize, Serialize};

/// Inputs considered when re-scoring risk before execution.
#[derive(Debug, Clone)]
pub struct EffectContext<'a> {
    pub observation: &'a AppObservation,
    pub action: &'a Action,
    /// Actor-declared closed-set consequence. `None` for control actions is
    /// fine; `None` for executable actions fails closed as unknown.
    pub effect: Option<&'a EffectClaim>,
    pub task_authorized_max_risk: RiskLevel,
}

/// Result of independent re-evaluation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectJudgement {
    pub risk: RiskLevel,
    pub rationale: String,
    /// True when the actor claim was ignored or contradicted.
    pub model_claim_overridden: bool,
    /// Actor declared `unknown` (or executable action had no effect): stop and
    /// ask the user; never auto-execute and never map to a hidden default.
    pub unknown: bool,
}

/// Trait implemented by each endpoint's Runtime policy. Implementations must
/// start from `effect_policy` (closed-set table) plus platform evidence, and
/// may only raise the resulting risk, never lower it below the evidence floor.
pub trait EffectGuard: Send + Sync {
    fn judge(&self, ctx: &EffectContext<'_>) -> EffectJudgement;
}

/// Whether an action is executable (needs a closed-set effect claim).
pub fn is_executable(action: &Action) -> bool {
    matches!(action, Action::Semantic(_) | Action::Targeted(_))
}

/// Base policy risk of a closed-set consequence classification (§3.3 table).
/// Shared by every endpoint's guard implementation.
pub fn effect_policy(kind: crate::action::EffectKind) -> Option<(RiskLevel, &'static str)> {
    use crate::action::EffectKind;
    Some(match kind {
        EffectKind::Observe => (RiskLevel::R0, "observe classification"),
        EffectKind::Navigate => (RiskLevel::R1, "navigate classification"),
        EffectKind::LocalEdit => (RiskLevel::R2, "local edit classification"),
        EffectKind::ExternalCommunication
        | EffectKind::ExternalSubmit
        | EffectKind::Destructive => (RiskLevel::R3, "external or irreversible consequence"),
        EffectKind::PermissionChange | EffectKind::Financial | EffectKind::Credential => {
            (RiskLevel::R4, "permission/finance/credential consequence")
        }
        EffectKind::Unknown => return None,
    })
}
