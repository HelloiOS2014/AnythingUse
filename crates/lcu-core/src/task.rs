//! Task records, events, receipts, and the task state machine.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::action::Action;
use crate::capability::CapabilityLevel;
use crate::error::{ErrorCode, LcuError, LcuResult};
use crate::observation::{AppSelector, ObservationId};
use crate::risk::RiskLevel;
use crate::types::CallerIdentity;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new() -> Self {
        Self(format!("task_{}", Uuid::new_v4()))
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

/// Lifecycle states for a single task.
///
/// Wire names match product language: `waiting_user` covers approval + takeover parks
/// (`WaitingApproval` is kept as the Rust variant for existing call sites).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    /// Waiting for human confirm / takeover (JSON: `waiting_user`).
    #[serde(rename = "waiting_user", alias = "waiting_approval")]
    WaitingApproval,
    /// User paused / same-app takeover (JSON: `paused`).
    #[serde(rename = "paused", alias = "paused_by_user")]
    PausedByUser,
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// Commands that mutate task lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCommand {
    Start,
    RequireApproval,
    Approve,
    PauseByUser,
    Resume,
    Succeed,
    Fail,
    Cancel,
}

/// Durable task metadata (no screenshots).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskRecord {
    pub task_id: TaskId,
    pub goal: String,
    pub state: TaskState,
    pub caller: CallerIdentity,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_selector: Option<AppSelector>,
    /// Per-task decision maker override: `vlm` (local model) or `agent`
    /// (external agent via lcu decide/act). None = follow the Runtime default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    pub step_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_observation_id: Option<ObservationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_action_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl TaskRecord {
    pub fn new(
        goal: impl Into<String>,
        caller: CallerIdentity,
        app_selector: Option<AppSelector>,
    ) -> Self {
        let now = Utc::now();
        Self {
            task_id: TaskId::new(),
            goal: goal.into(),
            state: TaskState::Queued,
            caller,
            created_at: now,
            updated_at: now,
            app_selector,
            actor: None,
            step_count: 0,
            last_observation_id: None,
            last_action_hash: None,
            summary: None,
            error: None,
        }
    }
}

/// Compact event stream item for `lcu watch --jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskEvent {
    pub task_id: TaskId,
    pub state: TaskState,
    pub at: DateTime<Utc>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
}

/// Receipt returned after an attempted platform action.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionReceipt {
    pub action: Action,
    pub action_hash: String,
    pub capability_used: CapabilityLevel,
    pub risk_level: RiskLevel,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub executed_at: DateTime<Utc>,
}

/// Enforces legal task state transitions.
#[derive(Debug, Default)]
pub struct TaskStateMachine;

impl TaskStateMachine {
    pub fn transition(current: TaskState, command: TaskCommand) -> LcuResult<TaskState> {
        let next = match (current, command) {
            (TaskState::Queued, TaskCommand::Start) => TaskState::Running,
            (TaskState::Queued, TaskCommand::Cancel) => TaskState::Cancelled,
            // Crash recovery: a task persisted in Queued has no worker behind it
            // after a process restart; pause it for the user to decide.
            (TaskState::Queued, TaskCommand::PauseByUser) => TaskState::PausedByUser,

            (TaskState::Running, TaskCommand::RequireApproval) => TaskState::WaitingApproval,
            (TaskState::Running, TaskCommand::PauseByUser) => TaskState::PausedByUser,
            (TaskState::Running, TaskCommand::Succeed) => TaskState::Succeeded,
            (TaskState::Running, TaskCommand::Fail) => TaskState::Failed,
            (TaskState::Running, TaskCommand::Cancel) => TaskState::Cancelled,

            (TaskState::WaitingApproval, TaskCommand::Approve) => TaskState::Running,
            (TaskState::WaitingApproval, TaskCommand::Fail) => TaskState::Failed,
            (TaskState::WaitingApproval, TaskCommand::Cancel) => TaskState::Cancelled,
            (TaskState::WaitingApproval, TaskCommand::PauseByUser) => TaskState::PausedByUser,

            (TaskState::PausedByUser, TaskCommand::Resume) => TaskState::Running,
            (TaskState::PausedByUser, TaskCommand::Cancel) => TaskState::Cancelled,
            (TaskState::PausedByUser, TaskCommand::Fail) => TaskState::Failed,

            (state, cmd) if state.is_terminal() => {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    format!("task already terminal ({state:?}); cannot apply {cmd:?}"),
                ));
            }
            (state, cmd) => {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    format!("illegal transition {state:?} + {cmd:?}"),
                ));
            }
        };
        Ok(next)
    }

    pub fn apply(record: &mut TaskRecord, command: TaskCommand) -> LcuResult<TaskState> {
        let next = Self::transition(record.state, command)?;
        record.state = next;
        record.updated_at = Utc::now();
        Ok(next)
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::types::CallerIdentity;

    
    #[test]
    fn happy_path_and_approval_gate() {
        let mut task = TaskRecord::new("demo", CallerIdentity::HumanCli, None);
        assert_eq!(task.state, TaskState::Queued);
        TaskStateMachine::apply(&mut task, TaskCommand::Start).unwrap();
        assert_eq!(task.state, TaskState::Running);
        TaskStateMachine::apply(&mut task, TaskCommand::RequireApproval).unwrap();
        assert_eq!(task.state, TaskState::WaitingApproval);
        TaskStateMachine::apply(&mut task, TaskCommand::Approve).unwrap();
        assert_eq!(task.state, TaskState::Running);
        TaskStateMachine::apply(&mut task, TaskCommand::Succeed).unwrap();
        assert_eq!(task.state, TaskState::Succeeded);
        assert!(TaskStateMachine::apply(&mut task, TaskCommand::Start).is_err());
    }
    #[test]
    fn illegal_transition_is_rejected() {
        let err =
            TaskStateMachine::transition(TaskState::Queued, TaskCommand::Approve).unwrap_err();
        assert_eq!(err.code(), ErrorCode::InvalidRequest);
    }
}

