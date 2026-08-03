//! One-time approval bindings that cannot be completed from the CLI alone.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{ErrorCode, LcuError, LcuResult};
use crate::observation::ObservationId;
use crate::task::TaskId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ApprovalId(pub String);

impl ApprovalId {
    pub fn new() -> Self {
        Self(format!("appr_{}", Uuid::new_v4()))
    }
}

impl Default for ApprovalId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Denied,
    Expired,
    Consumed,
    Invalidated,
}

/// Cryptographic binding for a single high-risk action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalBinding {
    pub task_id: TaskId,
    pub observation_id: ObservationId,
    pub action_hash: String,
    pub target_app: String,
    pub expires_at: DateTime<Utc>,
    pub one_time_nonce: String,
}

impl ApprovalBinding {
    pub fn new(
        task_id: TaskId,
        observation_id: ObservationId,
        action_hash: impl Into<String>,
        target_app: impl Into<String>,
        ttl: Duration,
    ) -> Self {
        Self {
            task_id,
            observation_id,
            action_hash: action_hash.into(),
            target_app: target_app.into(),
            expires_at: Utc::now() + ttl,
            one_time_nonce: Uuid::new_v4().to_string(),
        }
    }

    pub fn binding_hash(&self) -> String {
        let payload = serde_json::json!({
            "task_id": self.task_id.0,
            "observation_id": self.observation_id.0,
            "action_hash": self.action_hash,
            "target_app": self.target_app,
            "expires_at": self.expires_at.to_rfc3339(),
            "one_time_nonce": self.one_time_nonce,
        });
        let bytes = serde_json::to_vec(&payload).expect("binding json");
        format!("bind_{}", hex::encode(Sha256::digest(bytes)))
    }

    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }
}

/// Pending approval request visible to the GUI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApprovalRequest {
    pub approval_id: ApprovalId,
    pub binding: ApprovalBinding,
    pub binding_hash: String,
    pub status: ApprovalStatus,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

impl ApprovalRequest {
    pub fn new(binding: ApprovalBinding, reason: impl Into<String>) -> Self {
        let binding_hash = binding.binding_hash();
        Self {
            approval_id: ApprovalId::new(),
            binding,
            binding_hash,
            status: ApprovalStatus::Pending,
            reason: reason.into(),
            created_at: Utc::now(),
        }
    }

    /// GUI-only finalization. CLI must never call a path that bypasses presence checks.
    pub fn approve_in_gui(
        &mut self,
        expected: &ApprovalBinding,
        now: DateTime<Utc>,
    ) -> LcuResult<()> {
        if self.status != ApprovalStatus::Pending {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                format!("approval not pending: {:?}", self.status),
            ));
        }
        if self.binding.is_expired(now) {
            self.status = ApprovalStatus::Expired;
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "approval expired",
            ));
        }
        if self.binding.binding_hash() != expected.binding_hash() {
            self.status = ApprovalStatus::Invalidated;
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "approval binding mismatch",
            ));
        }
        if self.binding.action_hash != expected.action_hash
            || self.binding.observation_id != expected.observation_id
            || self.binding.task_id != expected.task_id
            || self.binding.target_app != expected.target_app
        {
            self.status = ApprovalStatus::Invalidated;
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "approval fields changed",
            ));
        }
        self.status = ApprovalStatus::Approved;
        Ok(())
    }

    /// GUI-only deny path. CLI/agents must never call this.
    pub fn deny_in_gui(&mut self) -> LcuResult<()> {
        if self.status != ApprovalStatus::Pending {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                format!("approval not pending: {:?}", self.status),
            ));
        }
        self.status = ApprovalStatus::Denied;
        Ok(())
    }

    pub fn consume(&mut self) -> LcuResult<()> {
        if self.status != ApprovalStatus::Approved {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "approval not approved",
            ));
        }
        self.status = ApprovalStatus::Consumed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::ObservationId;
    use crate::task::TaskId;

    fn sample_binding() -> ApprovalBinding {
        ApprovalBinding::new(
            TaskId("task_1".into()),
            ObservationId("obs_1".into()),
            "act_abc",
            "com.google.Chrome",
            Duration::minutes(5),
        )
    }

    #[test]
    fn gui_approve_then_consume_once() {
        let binding = sample_binding();
        let mut req = ApprovalRequest::new(binding.clone(), "submit form");
        req.approve_in_gui(&binding, Utc::now()).unwrap();
        assert_eq!(req.status, ApprovalStatus::Approved);
        req.consume().unwrap();
        assert_eq!(req.status, ApprovalStatus::Consumed);
        assert!(req.consume().is_err());
    }
}

