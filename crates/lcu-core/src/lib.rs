//! Shared contracts for Local Computer Use.
//!
//! This crate must never depend on platform backends. Platform crates depend on
//! these types; the reverse dependency is forbidden.

pub mod action;
pub mod approval;
pub mod capability;
pub mod effect_guard;
pub mod error;
pub mod observation;
pub mod protocol;
pub mod risk;
pub mod schema;
pub mod security_set;
pub mod surface;
pub mod task;
pub mod types;

pub use action::{
    Action, ActionKind, EffectClaim, EffectKind, ProposedAction, SemanticAction, TargetedInput,
};
pub use approval::{
    AppAccessDecision, AppPermission, ConsequenceGrant, ConsequenceIdentity, ForegroundGrant,
    GateKind, GateRequest, GrantId, GrantStatus, ScreenshotEvidence,
};
pub use capability::CapabilityLevel;
pub use effect_guard::{EffectContext, EffectGuard, EffectJudgement, StaticEffectGuard};
pub use error::{ErrorCode, LcuError, LcuResult};
pub use observation::{
    AppObservation, AppSelector, AppTarget, ElementNode, ObservationId, Rect, TransformId,
};
pub use protocol::{
    DoctorReport, ExitCode, InternalProtocolVersion, JsonEnvelope, JsonError, JsonStatus,
    PROTOCOL_SCHEMA_VERSION,
};
pub use risk::RiskLevel;
pub use schema::SchemaDocument;
pub use surface::{
    control_target_from_app_target, control_target_from_observation, ChromeTab, ControlState,
    ControlTarget, MacWindow,
};
pub use task::{
    ActionReceipt, TaskCommand, TaskEvent, TaskId, TaskRecord, TaskState, TaskStateMachine,
};
pub use types::{CallerIdentity, Frame, Point};
