//! Single observe → decide → act → re-observe step.
//!
//! **Product rule:** real OS actions only execute through [`Runtime::perform_gated_action`].
//! Prefer the product worker (`worker.rs`) for `lcu run`.

use lcu_core::action::{Action, ProposedAction};
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::{AppObservation, AppTarget};
use lcu_core::risk::RiskLevel;
use lcu_core::task::{ActionReceipt, TaskId};
use lcu_core::types::CallerIdentity;
use lcu_model::{ModelObservation, ModelTaskContext, VisionActor};
use lcu_platform::PlatformBackend;
use serde::{Deserialize, Serialize};

use crate::Runtime;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    pub before: AppObservation,
    pub proposal: ProposedAction,
    pub receipt: Option<ActionReceipt>,
    pub after: Option<AppObservation>,
    pub actor: String,
}

/// Run one controlled step **without** Runtime gating.
///
/// Sealed in release builds (dev bypass is not compiled in). Debug builds may set
/// `LCU_ALLOW_DEV_BYPASS=1` for explicit spikes (legacy M1 bins). Production must
/// use [`run_gated_step`] or `lcu run`.
pub fn run_single_step(
    backend: &dyn PlatformBackend,
    actor: &dyn VisionActor,
    target: &AppTarget,
    goal: &str,
    step: u32,
) -> LcuResult<StepResult> {
    #[cfg(not(debug_assertions))]
    {
        let _ = (backend, actor, target, goal, step);
        return Err(LcuError::coded(
            ErrorCode::PermissionDenied,
            "run_single_step is sealed in release: real actions must go through \
             Runtime product path (lcu run → desktop worker).",
        ));
    }
    #[cfg(debug_assertions)]
    {
        if !dev_bypass_allowed() {
            return Err(LcuError::coded(
                ErrorCode::PermissionDenied,
                "run_single_step is sealed: real actions must go through Runtime product path \
                 (lcu run → desktop worker). Debug only: LCU_ALLOW_DEV_BYPASS=1.",
            ));
        }
        run_single_step_dev_bypass(backend, actor, target, goal, step)
    }
}

#[cfg(debug_assertions)]
fn dev_bypass_allowed() -> bool {
    matches!(
        std::env::var("LCU_ALLOW_DEV_BYPASS").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

/// Dev-only bypass (`LCU_ALLOW_DEV_BYPASS=1`). Not compiled into release builds.
#[cfg(debug_assertions)]
pub fn run_single_step_dev_bypass(
    backend: &dyn PlatformBackend,
    actor: &dyn VisionActor,
    target: &AppTarget,
    goal: &str,
    step: u32,
) -> LcuResult<StepResult> {
    let before = backend.observe(target)?;
    let mut model_obs = ModelObservation::from(&before);
    let _ = model_obs.image_png.take();

    let proposal = actor.propose_action(
        &model_obs,
        &ModelTaskContext {
            goal: goal.to_string(),
            step,
            last_action_summary: None,
        },
    )?;

    lcu_model::validate_action(&before, &proposal.action)?;
    lcu_model::ensure_observation_binding(&before, &proposal.observation_id)?;

    // Frontmost/user-active is not a stop signal; only target_lost (or explicit
    // taken_over, handled on the product path).
    if matches!(
        backend.detect_user_conflict(target)?,
        lcu_core::surface::ControlState::TargetLost
    ) {
        return Err(LcuError::coded(
            ErrorCode::WaitingUser,
            "conflict: target lost",
        ));
    }

    let receipt = match &proposal.action {
        Action::Semantic(sem) => Some(backend.perform_semantic_action(target, sem)?),
        Action::Targeted(input) => Some(backend.perform_targeted_input(target, input)?),
        Action::Observe | Action::Wait { .. } => None,
        Action::Done { .. } | Action::Fail { .. } | Action::RequestUser { .. } => None,
        Action::Exclusive(_) => {
            return Err(LcuError::coded(
                ErrorCode::PermissionDenied,
                "single-step loop refuses exclusive input; use semantic/targeted directed actions",
            ));
        }
    };

    std::thread::sleep(std::time::Duration::from_millis(300));
    let after = backend.observe(target).ok();

    Ok(StepResult {
        before,
        proposal,
        receipt,
        after,
        actor: actor.name().to_string(),
    })
}

/// Preferred single-step entry: policy via Runtime; act only through gated executor.
pub fn run_gated_step(
    runtime: &Runtime,
    task_id: &TaskId,
    actor: &dyn VisionActor,
    target: &AppTarget,
    goal: &str,
    step: u32,
    caller: &CallerIdentity,
) -> LcuResult<StepResult> {
    let before = runtime.observe_target(target)?;
    let mut model_obs = ModelObservation::from(&before);
    let _ = model_obs.image_png.take();

    let proposal = actor.propose_action(
        &model_obs,
        &ModelTaskContext {
            goal: goal.to_string(),
            step,
            last_action_summary: None,
        },
    )?;

    lcu_model::validate_action(&before, &proposal.action)?;
    lcu_model::ensure_observation_binding(&before, &proposal.observation_id)?;

    let evaluated = runtime.evaluate_action_for_task(
        Some(task_id),
        &before,
        &proposal.action,
        proposal.effect_claim.as_deref(),
        RiskLevel::R4,
        Some(caller),
    )?;

    let receipt = match &proposal.action {
        Action::Semantic(_) | Action::Targeted(_) => {
            if evaluated.requires_takeover || evaluated.requires_approval {
                return Err(LcuError::coded(
                    ErrorCode::WaitingUser,
                    format!(
                        "action requires GUI approval/takeover (risk {:?}): {}",
                        evaluated.risk, evaluated.rationale
                    ),
                ));
            }
            Some(runtime.perform_gated_action(task_id, target, &before, &proposal.action, None)?)
        }
        Action::Observe | Action::Wait { .. } => None,
        Action::Done { .. } | Action::Fail { .. } | Action::RequestUser { .. } => None,
        Action::Exclusive(_) => {
            return Err(LcuError::coded(
                ErrorCode::PermissionDenied,
                "gated step refuses exclusive without exclusive consent",
            ));
        }
    };

    std::thread::sleep(std::time::Duration::from_millis(300));
    let after = runtime.observe_target(target).ok();

    Ok(StepResult {
        before,
        proposal,
        receipt,
        after,
        actor: actor.name().to_string(),
    })
}
