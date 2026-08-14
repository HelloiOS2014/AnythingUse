//! Authorization grants (realignment contract §3.2–§3.4).
//!
//! The old single `ApprovalBinding` (observation_id + action_hash, replayed
//! after approval) is split into three task-scoped grants:
//!
//! - `AppPermission`: first-control gate for a stable app identity
//!   (`allow_once` / `always_allow` / `deny`; always_allow persists locally);
//! - `ForegroundGrant`: one-time, task-scoped permission to bring the exact
//!   target window to the front; never persists, never authorizes business
//!   consequences;
//! - `ConsequenceGrant`: one-time confirmation of a real consequence
//!   (send/submit/delete/…), bound to the Runtime-extracted consequence
//!   identity or exact screenshot evidence. Runtime never stores a replayable
//!   Action.
//!
//! A pending gate request (`GateRequest`) is what the desktop UI sees; CLI and
//! Agents can never complete one.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::action::{EffectClaim, EffectKind};
use crate::error::{ErrorCode, LcuError, LcuResult};
use crate::observation::ObservationId;
use crate::task::TaskId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct GrantId(pub String);

impl GrantId {
    pub fn new() -> Self {
        Self(format!("grant_{}", Uuid::new_v4()))
    }
}

impl Default for GrantId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GrantStatus {
    Pending,
    Approved,
    Denied,
    Expired,
    Consumed,
    Invalidated,
}

/// Which gate a pending request belongs to. Every gate parks the task in the
/// same `waiting_actor` state; the old action is never retained for replay.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GateKind {
    AppAccess,
    Foreground,
    Consequence,
    Takeover,
}

/// App-access decision for the first control of a stable app identity.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppAccessDecision {
    /// Valid for the current task only.
    AllowOnce,
    /// Persisted locally; revocable in settings.
    AlwaysAllow,
    /// The current task cannot control this app.
    Deny,
}

/// Stable app-access permission record. `app_key` is the stable app identity
/// (bundle identity on macOS; connected extension + profile for Chrome).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppPermission {
    pub app_key: String,
    pub decision: AppAccessDecision,
    pub created_at: DateTime<Utc>,
}

impl AppPermission {
    pub fn new(app_key: impl Into<String>, decision: AppAccessDecision) -> Self {
        Self {
            app_key: app_key.into(),
            decision,
            created_at: Utc::now(),
        }
    }
}

/// One-time task-scoped foreground grant: activates the exact target window
/// once. Never persists; never authorizes any business consequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForegroundGrant {
    pub grant_id: GrantId,
    pub task_id: TaskId,
    pub app_key: String,
    pub expires_at: DateTime<Utc>,
    pub nonce: String,
    pub status: GrantStatus,
}

impl ForegroundGrant {
    pub fn new(task_id: TaskId, app_key: impl Into<String>, ttl: Duration) -> Self {
        Self {
            grant_id: GrantId::new(),
            task_id,
            app_key: app_key.into(),
            expires_at: Utc::now() + ttl,
            nonce: Uuid::new_v4().to_string(),
            status: GrantStatus::Pending,
        }
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

/// Runtime-extracted consequence identity. Any field that could change the
/// user's confirmation judgment participates in the hash; a changed field
/// means the grant no longer matches and must not be consumed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConsequenceIdentity {
    /// `invoke` | `set_value` | `navigate` | `click` | `type` | `keys` | `scroll`
    pub operation: String,
    /// Element label/id for semantic actions (stored normalized, truncated).
    pub object: Option<String>,
    /// Destination URL origin for navigation.
    pub destination: Option<String>,
    /// SHA-256 digest of typed/set content (never the raw content).
    pub content_digest: Option<String>,
    /// Amount for financial actions (not extracted today; reserved).
    pub amount: Option<String>,
    /// Account/origin for communication actions (not extracted today; reserved).
    pub account: Option<String>,
}

impl ConsequenceIdentity {
    pub fn hash(&self) -> String {
        let payload = serde_json::json!({
            "operation": self.operation,
            "object": self.object,
            "destination": self.destination,
            "content_digest": self.content_digest,
            "amount": self.amount,
            "account": self.account,
        });
        format!("conseq_{}", hex::encode(Sha256::digest(
            serde_json::to_vec(&payload).expect("identity json")
        )))
    }
}

/// Exact-match screenshot evidence kept alongside a consequence grant
/// (realignment §3.4): after the required fresh observation,
/// `image_hash + action_hash` may prove "picture completely unchanged";
/// `observation_id` remains audit evidence only. Runtime never stores a
/// replayable Action. The candidate fields identify the original high-risk
/// candidate so a jittered coordinate or downgraded effect on the same
/// screenshot is still treated as the same candidate (re-confirm, never
/// ordinary allowed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScreenshotEvidence {
    pub observation_id: ObservationId,
    pub image_hash: Option<String>,
    pub action_hash: String,
    /// Semantic element of the original candidate, when present.
    pub element_id: Option<String>,
    /// Input kind of the original candidate (`click` | `type` | `keys`).
    pub input_kind: Option<String>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub effect_kind: EffectKind,
}

/// One-time consequence confirmation grant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsequenceGrant {
    pub grant_id: GrantId,
    pub task_id: TaskId,
    pub app_key: String,
    pub effect_kind: EffectKind,
    pub identity: ConsequenceIdentity,
    pub identity_hash: String,
    /// Display + audit only; never participates in matching.
    pub summary: String,
    pub expires_at: DateTime<Utc>,
    pub nonce: String,
    pub status: GrantStatus,
    /// Exact-match screenshot evidence when Runtime cannot extract a semantic
    /// consequence identity (screenshot-only proposals).
    pub screenshot_evidence: Option<ScreenshotEvidence>,
}

impl ConsequenceGrant {
    pub fn new(
        task_id: TaskId,
        app_key: impl Into<String>,
        effect: &EffectClaim,
        identity: ConsequenceIdentity,
        summary: impl Into<String>,
        screenshot_evidence: Option<ScreenshotEvidence>,
        ttl: Duration,
    ) -> Self {
        let identity_hash = identity.hash();
        Self {
            grant_id: GrantId::new(),
            task_id,
            app_key: app_key.into(),
            effect_kind: effect.kind,
            identity,
            identity_hash,
            summary: summary.into(),
            expires_at: Utc::now() + ttl,
            nonce: Uuid::new_v4().to_string(),
            status: GrantStatus::Pending,
            screenshot_evidence,
        }
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

/// Pending gate request visible to the desktop confirmation UI.
///
/// `binding_hash` covers the fields that must be byte-stable between the
/// request and the GUI finalization; the GUI shows `summary`/`risk_note`
/// (display only, never matched).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateRequest {
    pub grant_id: GrantId,
    pub kind: GateKind,
    pub task_id: TaskId,
    pub app_key: String,
    pub binding_hash: String,
    pub status: GrantStatus,
    pub reason: String,
    /// Consequence display summary (task, app, effect, object/destination).
    pub summary: String,
    pub risk_note: String,
    pub requires_takeover: bool,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// App access decision chosen in the GUI (only for `kind == AppAccess`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_decision: Option<AppAccessDecision>,
}

impl GateRequest {
    pub fn new(
        kind: GateKind,
        task_id: TaskId,
        app_key: impl Into<String>,
        reason: impl Into<String>,
        summary: impl Into<String>,
        risk_note: impl Into<String>,
        requires_takeover: bool,
        ttl: Duration,
    ) -> Self {
        let app_key = app_key.into();
        let request = Self {
            grant_id: GrantId::new(),
            kind,
            task_id,
            app_key,
            binding_hash: String::new(),
            status: GrantStatus::Pending,
            reason: reason.into(),
            summary: summary.into(),
            risk_note: risk_note.into(),
            requires_takeover,
            created_at: Utc::now(),
            expires_at: Utc::now() + ttl,
            app_decision: None,
        };
        Self {
            binding_hash: request.binding_hash(),
            ..request
        }
    }

    /// Covers every field that must be identical when the GUI finalizes.
    pub fn binding_hash(&self) -> String {
        let payload = serde_json::json!({
            "grant_id": self.grant_id.0,
            "kind": self.kind,
            "task_id": self.task_id.0,
            "app_key": self.app_key,
            "expires_at": self.expires_at.to_rfc3339(),
            "requires_takeover": self.requires_takeover,
        });
        format!(
            "bind_{}",
            hex::encode(Sha256::digest(serde_json::to_vec(&payload).expect("binding json")))
        )
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// GUI-only finalization. CLI/Agents must never call a path that bypasses
    /// GUI presence checks.
    pub fn approve_in_gui(
        &mut self,
        expected: &GateRequest,
        now: DateTime<Utc>,
    ) -> LcuResult<()> {
        if self.status != GrantStatus::Pending {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                format!("gate not pending: {:?}", self.status),
            ));
        }
        if self.is_expired(now) {
            self.status = GrantStatus::Expired;
            return Err(LcuError::coded(ErrorCode::ApprovalInvalid, "gate expired"));
        }
        if self.binding_hash != expected.binding_hash {
            self.status = GrantStatus::Invalidated;
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "gate binding mismatch",
            ));
        }
        self.status = GrantStatus::Approved;
        Ok(())
    }

    /// GUI-only deny path. CLI/agents must never call this.
    pub fn deny_in_gui(&mut self) -> LcuResult<()> {
        if self.status != GrantStatus::Pending {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                format!("gate not pending: {:?}", self.status),
            ));
        }
        self.status = GrantStatus::Denied;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consequence_identity_hash_changes_with_any_field() {
        let base = ConsequenceIdentity {
            operation: "invoke".into(),
            object: Some("发送".into()),
            destination: None,
            content_digest: None,
            amount: None,
            account: None,
        };
        let hash = base.hash();
        assert_eq!(base.hash(), hash, "hash is deterministic");
        assert_ne!(
            ConsequenceIdentity {
                object: Some("Send".into()),
                ..base.clone()
            }
            .hash(),
            hash,
            "object change must change the identity hash"
        );
        assert_ne!(
            ConsequenceIdentity {
                destination: Some("https://example.com".into()),
                ..base.clone()
            }
            .hash(),
            hash,
            "destination change must change the identity hash"
        );
    }

    #[test]
    fn gui_approve_then_gate_finalizes_once() {
        let mut req = GateRequest::new(
            GateKind::Consequence,
            TaskId("task_1".into()),
            "com.example.app",
            "reason",
            "summary",
            "R3",
            false,
            Duration::minutes(5),
        );
        let expected = req.clone();
        req.approve_in_gui(&expected, Utc::now()).unwrap();
        assert_eq!(req.status, GrantStatus::Approved);
        assert!(req.approve_in_gui(&expected, Utc::now()).is_err());
        req.deny_in_gui().unwrap_err();
    }

    #[test]
    fn app_access_decision_roundtrip() {
        let perm = AppPermission::new("com.example.app", AppAccessDecision::AlwaysAllow);
        let json = serde_json::to_string(&perm).unwrap();
        let back: AppPermission = serde_json::from_str(&json).unwrap();
        assert_eq!(back.decision, AppAccessDecision::AlwaysAllow);
        assert_eq!(back.app_key, "com.example.app");
    }
}
