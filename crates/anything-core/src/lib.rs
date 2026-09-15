//! AnythingUse shared core — platform-neutral contracts for every endpoint.
//!
//! macOS (`lcu`), Android (`lau`), and future endpoints all depend on this
//! crate. It must never depend on platform backends and must never carry
//! platform-specific evidence logic; platform crates implement the
//! `EffectGuard` trait on top of `effect_policy` with their own
//! platform-normalized evidence (see `lcu-core`'s `StaticEffectGuard` for the
//! macOS example, and `docs/lau-android-plan.md` §7 for the Android rule).
//!
//! Historical type names (`LcuError`, `LcuResult`, `lcu_core::` paths via the
//! `lcu-core` re-export shell) keep the LCU codename — see the README naming
//! note.

pub mod action;
pub mod approval;
pub mod capability;
pub mod effect_guard;
pub mod error;
pub mod observation;
pub mod protocol;
pub mod risk;
pub mod schema;
pub mod surface;
pub mod task;
pub mod types;

pub use action::{
    Action, ActionKind, EffectClaim, EffectKind, ProposedAction, SemanticAction, TargetedInput,
};
pub use approval::{
    AppAccessDecision, AppPermission, ConsequenceGrant, ConsequenceIdentity, GateKind, GateRequest,
    GrantId, GrantStatus, ScreenshotEvidence,
};
pub use capability::CapabilityLevel;
pub use effect_guard::{EffectContext, EffectGuard, EffectJudgement};
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
