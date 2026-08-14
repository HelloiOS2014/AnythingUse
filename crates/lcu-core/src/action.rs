//! Structured actions proposed by the model and validated by Runtime.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::observation::ObservationId;

/// High-level action categories.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Observe,
    Semantic,
    TargetedInput,
    Wait,
    Done,
    Fail,
    RequestUser,
}

/// Closed-set consequence classification shared by both decision actors
/// (external Agent and local VLM use the same enum and parser).
///
/// The actor's `effect` is a structured safety classification, never an
/// authorization: user goal, app access and consequence grants are the
/// authorization. Runtime computes an independent risk floor that the actor
/// declaration can never lower.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    /// Screenshot, wait.
    Observe,
    /// Open, search, select, scroll, switch page.
    Navigate,
    /// Local draft/document non-sensitive input.
    LocalEdit,
    /// Send message, email, publish content.
    ExternalCommunication,
    /// Submit form, create record, upload file.
    ExternalSubmit,
    /// Delete, overwrite, clear, revoke access.
    Destructive,
    /// Modify sharing, account, system permission.
    PermissionChange,
    /// Purchase, pay, transfer.
    Financial,
    /// Password, verification code, secret, security verification.
    Credential,
    /// Actor cannot determine the real consequence.
    Unknown,
}

/// Actor-declared consequence claim for one executable proposal.
///
/// `summary` is for user explanation and audit only — it never participates in
/// grant matching and must not contain credentials or full sensitive text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectClaim {
    pub kind: EffectKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl EffectClaim {
    pub fn new(kind: EffectKind, summary: impl Into<String>) -> Self {
        Self {
            kind,
            summary: Some(summary.into()),
        }
    }
}

/// Semantic accessibility-style action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SemanticAction {
    Navigate {
        url: String,
    },
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

/// Basic product-boundary check for browser navigation.
///
/// Only explicit HTTP(S) URLs with a non-empty authority are accepted. Chrome
/// remains responsible for full URL parsing and navigation semantics.
pub fn is_http_navigation_url(url: &str) -> bool {
    if url.is_empty()
        || url.len() > 4096
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return false;
    }
    let host_port = authority;
    if let Some(ipv6) = host_port.strip_prefix('[') {
        return ipv6.find(']').is_some_and(|end| end > 0);
    }
    !host_port.split(':').next().unwrap_or_default().is_empty()
}

/// Window- or PID-targeted low-level input (still not global by default).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TargetedInput {
    Click {
        /// Normalized [0,1] coordinates inside the target window image.
        x: f64,
        y: f64,
        #[serde(default)]
        button: MouseButton,
    },
    TypeText {
        text: String,
        /// Optional normalized target point. When present, click + type execute
        /// atomically in one target-only input session.
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
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

/// Model proposal before effect guard and policy re-evaluation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProposedAction {
    pub observation_id: ObservationId,
    pub action: Action,
    /// Actor-declared closed-set consequence; never trusted as final risk.
    /// Executable actions (semantic/targeted) must declare one; missing or
    /// invalid values are rejected at the trust boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<EffectClaim>,
    #[serde(default)]
    pub confidence: f32,
}
