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
/// Wire names match product language: `waiting_actor` is the unified park for
/// Agent decisions, app access, and consequence confirmation — the old
/// proposal is never retained for replay. Deserialize aliases
/// keep previously persisted `waiting_user` / `waiting_approval` rows readable.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    /// Waiting for an Agent decision or a human app/consequence gate, followed
    /// by continuation on a fresh observation.
    #[serde(alias = "waiting_user", alias = "waiting_approval")]
    WaitingActor,
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
    /// Park the task in `waiting_actor` for an Agent decision or human gate.
    WaitActor,
    /// A gate was decided in the GUI; resume toward the actor continuation.
    Approve,
    /// External Agent submitted a decision for the current observation.
    ActorReady,
    PauseByUser,
    Resume,
    Succeed,
    Fail,
    Cancel,
}

/// Task-level control mode (realignment §3.2): user/task choice, never an
/// app hardcode.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ControlMode {
    /// Background first, then disclosed foreground fallback when required.
    #[default]
    Auto,
    /// Never activate; `foreground_required` is an explicit error.
    BackgroundOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    AgentDecision,
    AppAccess,
    Consequence,
}

/// Durable task metadata (no screenshots).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskRecord {
    pub task_id: TaskId,
    pub goal: String,
    pub state: TaskState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_reason: Option<WaitReason>,
    pub caller: CallerIdentity,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_selector: Option<AppSelector>,
    /// Per-task decision maker override: `vlm` (local model) or `agent`
    /// (external agent via lcu decide/act). None = follow the Runtime default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// Task control mode (auto / background_only).
    #[serde(default)]
    pub control_mode: ControlMode,
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
            wait_reason: None,
            caller,
            created_at: now,
            updated_at: now,
            app_selector,
            actor: None,
            control_mode: ControlMode::Auto,
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

            (TaskState::Running, TaskCommand::WaitActor) => TaskState::WaitingActor,
            (TaskState::Running, TaskCommand::PauseByUser) => TaskState::PausedByUser,
            (TaskState::Running, TaskCommand::Succeed) => TaskState::Succeeded,
            (TaskState::Running, TaskCommand::Fail) => TaskState::Failed,
            (TaskState::Running, TaskCommand::Cancel) => TaskState::Cancelled,

            (TaskState::WaitingActor, TaskCommand::Approve) => TaskState::Running,
            (TaskState::WaitingActor, TaskCommand::ActorReady) => TaskState::Running,
            (TaskState::WaitingActor, TaskCommand::Fail) => TaskState::Failed,
            (TaskState::WaitingActor, TaskCommand::Cancel) => TaskState::Cancelled,
            (TaskState::WaitingActor, TaskCommand::PauseByUser) => TaskState::PausedByUser,

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
    fn happy_path_and_actor_gate() {
        let mut task = TaskRecord::new("demo", CallerIdentity::HumanCli, None);
        assert_eq!(task.state, TaskState::Queued);
        TaskStateMachine::apply(&mut task, TaskCommand::Start).unwrap();
        assert_eq!(task.state, TaskState::Running);
        TaskStateMachine::apply(&mut task, TaskCommand::WaitActor).unwrap();
        assert_eq!(task.state, TaskState::WaitingActor);
        TaskStateMachine::apply(&mut task, TaskCommand::Approve).unwrap();
        assert_eq!(task.state, TaskState::Running);
        TaskStateMachine::apply(&mut task, TaskCommand::WaitActor).unwrap();
        TaskStateMachine::apply(&mut task, TaskCommand::ActorReady).unwrap();
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
