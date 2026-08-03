//! Structured actions proposed by the model and validated by Runtime.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::observation::ObservationId;
use crate::risk::RiskLevel;

/// High-level action categories.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Observe,
    Semantic,
    TargetedInput,
    ExclusiveInput,
    Wait,
    Done,
    Fail,
    RequestUser,
}

/// Semantic accessibility-style action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SemanticAction {
    Invoke {
        element_id: String,
    },
    SetValue {
        element_id: String,
        value: String,
    },
    Focus {
        element_id: String,
    },
    Scroll {
        element_id: Option<String>,
        delta_x: f64,
        delta_y: f64,
    },
}

/// Window- or PID-targeted low-level input (still not global by default).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TargetedInput {
    Click {
        /// Normalized [0,1] coordinates inside the target window image.
        x: f64,
        y: f64,
        button: MouseButton,
    },
    TypeText {
        text: String,
    },
    KeyCombo {
        keys: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    #[default]
    Left,
    Right,
    Middle,
}

/// Canonical action validated by Runtime before execution.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Observe,
    Semantic(SemanticAction),
    Targeted(TargetedInput),
    Exclusive(TargetedInput),
    Wait { milliseconds: u64 },
    Done { summary: String },
    Fail { reason: String },
    RequestUser { reason: String },
}

impl Action {
    pub fn kind(&self) -> ActionKind {
        match self {
            Self::Observe => ActionKind::Observe,
            Self::Semantic(_) => ActionKind::Semantic,
            Self::Targeted(_) => ActionKind::TargetedInput,
            Self::Exclusive(_) => ActionKind::ExclusiveInput,
            Self::Wait { .. } => ActionKind::Wait,
            Self::Done { .. } => ActionKind::Done,
            Self::Fail { .. } => ActionKind::Fail,
            Self::RequestUser { .. } => ActionKind::RequestUser,
        }
    }

    /// Stable hash used to bind one-time approvals.
    pub fn action_hash(&self) -> String {
        let json = serde_json::to_vec(self).expect("action serialization");
        let digest = Sha256::digest(json);
        format!("act_{}", hex::encode(digest))
    }

    pub fn referenced_element_id(&self) -> Option<&str> {
        match self {
            Self::Semantic(SemanticAction::Invoke { element_id })
            | Self::Semantic(SemanticAction::SetValue { element_id, .. })
            | Self::Semantic(SemanticAction::Focus { element_id }) => Some(element_id),
            Self::Semantic(SemanticAction::Scroll {
                element_id: Some(id),
                ..
            }) => Some(id),
            _ => None,
        }
    }
}

/// Model proposal before EffectGuard and policy re-evaluation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProposedAction {
    pub observation_id: ObservationId,
    pub action: Action,
    /// Model-declared effect; never trusted as final risk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_claim: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_effect: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_claimed_risk: Option<RiskLevel>,
    #[serde(default)]
    pub confidence: f32,
}


