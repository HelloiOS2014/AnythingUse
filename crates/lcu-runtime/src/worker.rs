//! Desktop-owned product loop: observe → decide → EffectGuard → act → re-observe.
//!
//! Real OS actions may only execute through [`Runtime::perform_gated_action`].

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration as StdDuration;

use lcu_core::action::Action;
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::{AppSelector, AppTarget};
use lcu_core::risk::RiskLevel;
use lcu_core::task::{TaskCommand, TaskId, TaskState};
use lcu_model::{
    ensure_observation_binding, validate_action, LoopGuard, LoopGuardConfig, ModelObservation,
    ModelTaskContext, SubprocessVisionActor, VisionActor,
};

use crate::{ExecutionGrant, Runtime};

/// Scope guard: always releases agent control on drop (success, fail, ? paths).
struct AgentSessionGuard<'a> {
    backend: &'a dyn lcu_platform::PlatformBackend,
    target: AppTarget,
    active: bool,
}

impl<'a> AgentSessionGuard<'a> {
    fn begin(
        backend: &'a dyn lcu_platform::PlatformBackend,
        target: &AppTarget,
    ) -> LcuResult<Self> {
        backend.set_agent_session(target, true)?;
        Ok(Self {
            backend,
            target: target.clone(),
            active: true,
        })
    }
}

impl Drop for AgentSessionGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.backend.set_agent_session(&self.target, false);
            self.active = false;
        }
    }
}

/// Queued work item for the background product worker.
#[derive(Debug, Clone)]
pub struct WorkItem {
    pub task_id: TaskId,
}

/// Pending high-risk action waiting for GUI approval before gated execution.
#[derive(Debug, Clone)]
pub struct PendingAction {
    pub approval_id: String,
    pub action: Action,
    pub observation: lcu_core::observation::AppObservation,
    pub target: AppTarget,
    pub risk: RiskLevel,
    /// R4 only: user has started human takeover but has not yet marked it done.
    pub takeover_started: bool,
}

/// Outcome of a single product step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// Continue the loop immediately.
    Continue,
    /// Park until approval / resume / cancel changes state.
    WaitExternal,
    /// Task reached a terminal state.
    Terminal,
}

/// Background scheduler: serial product worker owned by desktop Runtime.
pub struct TaskScheduler {
    tx: Mutex<Option<Sender<WorkItem>>>,
}

impl Default for TaskScheduler {
    fn default() -> Self {
        Self {
            tx: Mutex::new(None),
        }
    }
}

impl TaskScheduler {
    pub fn enqueue(&self, task_id: TaskId) -> LcuResult<()> {
        let guard = self.tx.lock().expect("scheduler lock");
        let Some(tx) = guard.as_ref() else {
            return Err(LcuError::coded(
                ErrorCode::RuntimeUnavailable,
                "task scheduler not started; start lcu-desktop (product path)",
            ));
        };
        tx.send(WorkItem { task_id }).map_err(|_| {
            LcuError::coded(ErrorCode::RuntimeUnavailable, "task scheduler worker died")
        })
    }

    pub fn is_started(&self) -> bool {
        self.tx.lock().expect("scheduler lock").is_some()
    }
}

impl Runtime {
    /// Start the single background product worker. Must be called once with `Arc` ownership.
    pub fn start_scheduler(self: &Arc<Self>) {
        let mut guard = self.scheduler.tx.lock().expect("scheduler lock");
        if guard.is_some() {
            return;
        }
        let (tx, rx): (Sender<WorkItem>, Receiver<WorkItem>) = mpsc::channel();
        *guard = Some(tx);
        drop(guard);

        let runtime = Arc::clone(self);
        thread::Builder::new()
            .name("lcu-task-worker".into())
            .spawn(move || runtime.worker_loop(rx))
            .expect("spawn lcu-task-worker");
        tracing::info!("lcu product task scheduler started");
    }

    fn worker_loop(self: Arc<Self>, rx: Receiver<WorkItem>) {
        while let Ok(item) = rx.recv() {
            if let Err(err) = self.run_task_to_completion(&item.task_id) {
                tracing::warn!(
                    task_id = %item.task_id.0,
                    error = %err,
                    "product worker task ended with error"
                );
                let _ = self.fail_task(&item.task_id, err.to_string());
            }
        }
    }

    /// Drive one task until terminal, or until it parks on approval/user pause.
    pub fn run_task_to_completion(&self, task_id: &TaskId) -> LcuResult<()> {
        let mut loop_guard = LoopGuard::new(LoopGuardConfig::default());
        let mut last_summary: Option<String> = None;

        loop {
            let task = self.get_task(task_id)?;
            if task.state.is_terminal() {
                return Ok(());
            }

            match task.state {
                TaskState::PausedByUser => {
                    return Ok(());
                }
                TaskState::WaitingApproval => match self.try_execute_pending(task_id)? {
                    Some(StepOutcome::Continue) => continue,
                    Some(StepOutcome::Terminal) => return Ok(()),
                    Some(StepOutcome::WaitExternal) | None => return Ok(()),
                },
                TaskState::Queued => {
                    let _ =
                        self.apply_command(task_id, TaskCommand::Start, "worker recovered start");
                }
                TaskState::Running => {}
                TaskState::Succeeded | TaskState::Failed | TaskState::Cancelled => return Ok(()),
            }

            match self.run_one_product_step(task_id, &mut loop_guard, &mut last_summary)? {
                StepOutcome::Continue => continue,
                StepOutcome::WaitExternal => return Ok(()),
                StepOutcome::Terminal => return Ok(()),
            }
        }
    }

    /// One observe → decide → guard → (approve?) → act cycle on the product path.
    pub fn run_one_product_step(
        &self,
        task_id: &TaskId,
        loop_guard: &mut LoopGuard,
        last_summary: &mut Option<String>,
    ) -> LcuResult<StepOutcome> {
        let task = self.get_task(task_id)?;
        if task.state.is_terminal() {
            return Ok(StepOutcome::Terminal);
        }
        if task.state != TaskState::Running {
            return Ok(StepOutcome::WaitExternal);
        }

        if let Some(outcome) = self.try_execute_pending(task_id)? {
            return Ok(outcome);
        }

        self.check_step_budget(task_id)?;

        self.backend
            .bind_task_context(&task.goal, Some(task_id.0.as_str()));

        let selector = resolve_selector(&task.goal, task.app_selector.clone())?;
        let target = self.backend.resolve_target(&selector).map_err(|e| {
            LcuError::coded(
                e.code(),
                format!(
                    "resolve target failed ({:?}): {e}; pass --app with a running app id",
                    selector.app_id
                ),
            )
        })?;
        self.set_current_target(target.clone());

        // Backend control state only (taken_over / target_lost).
        if let Some(outcome) = self.apply_control_gate(task_id, &target, "pre-observe")? {
            return Ok(outcome);
        }

        let _agent = AgentSessionGuard::begin(self.backend.as_ref(), &target)?;

        let observation = match self.backend.observe(&target) {
            Ok(o) => o,
            Err(e) if e.code() == ErrorCode::WaitingUser => {
                let _ = self.apply_command(
                    task_id,
                    TaskCommand::PauseByUser,
                    format!("paused (observe): {e}"),
                );
                return Ok(StepOutcome::WaitExternal);
            }
            Err(e) => return Err(e),
        };
        // Same-window user takeover can land between resolve and observe.
        if let Some(outcome) = self.apply_control_gate(task_id, &target, "post-observe")? {
            return Ok(outcome);
        }
        {
            let mut rec = self.get_task(task_id)?;
            rec.last_observation_id = Some(observation.observation_id.clone());
            rec.updated_at = chrono::Utc::now();
            self.persist_task(&rec, None);
        }

        let model_obs = ModelObservation::from(&observation);

        let step = task.step_count;
        let ctx = ModelTaskContext {
            goal: task.goal.clone(),
            step,
            last_action_summary: last_summary.clone(),
        };
        let propose_t0 = std::time::Instant::now();
        let proposal = match self.actor.propose_action(&model_obs, &ctx) {
            Ok(p) => p,
            Err(e) => {
                // Prefer pause-on-takeover over failing mid-propose when the user owns the window.
                if let Some(outcome) =
                    self.apply_control_gate(task_id, &target, "propose-failed")?
                {
                    return Ok(outcome);
                }
                tracing::warn!(
                    task_id = %task_id.0,
                    error = %e,
                    actor = self.actor.name(),
                    propose_ms = propose_t0.elapsed().as_millis() as u64,
                    "vision propose failed; task FAILED (no heuristic auto-fallback)"
                );
                self.fail_task(
                    task_id,
                    format!("VLM propose failed ({}); queue continues", e),
                )?;
                return Ok(StepOutcome::Terminal);
            }
        };
        let propose_ms = propose_t0.elapsed().as_millis() as u64;

        tracing::info!(
            task_id = %task_id.0,
            step,
            propose_ms,
            action = ?proposal.action,
            last_action_summary = ?last_summary,
            actor = self.actor.name(),
            "product step model proposal"
        );

        // Only an explicit Action::Done may complete the product task.
        // Repeated set_value / observe / wait go through LoopGuard below — never
        // forge success from "model re-proposed the same write".

        match &proposal.action {
            Action::Done { summary } => {
                // Lightweight deterministic check: re-observe must succeed (target still present).
                match self.backend.observe(&target) {
                    Ok(_) => {
                        self.succeed_task(task_id, Some(summary.clone()))?;
                        return Ok(StepOutcome::Terminal);
                    }
                    Err(e) => {
                        self.fail_task(
                            task_id,
                            format!("done but re-observe failed: {e} (model said: {summary})"),
                        )?;
                        return Ok(StepOutcome::Terminal);
                    }
                }
            }
            Action::Fail { reason } => {
                self.fail_task(task_id, reason.clone())?;
                return Ok(StepOutcome::Terminal);
            }
            Action::RequestUser { reason } => {
                let _ = self.apply_command(
                    task_id,
                    TaskCommand::PauseByUser,
                    format!("request user: {reason}"),
                );
                let mut rec = self.get_task(task_id)?;
                rec.summary = Some(format!("waiting_user: {reason}"));
                self.persist_task(&rec, None);
                return Ok(StepOutcome::WaitExternal);
            }
            Action::Wait { milliseconds } => {
                // Wait still counts toward loop detection (repeated no-op waits).
                if let Err(e) = loop_guard.record_and_check(&proposal.action) {
                    self.fail_task(task_id, format!("loop guard: {e}"))?;
                    return Ok(StepOutcome::Terminal);
                }
                thread::sleep(StdDuration::from_millis((*milliseconds).min(10_000)));
                *last_summary = Some(format!("wait {milliseconds}ms"));
                self.record_step(task_id)?;
                thread::sleep(StdDuration::from_millis(50));
                return Ok(StepOutcome::Continue);
            }
            Action::Observe => {
                // Observe must enter LoopGuard. Previously it returned before
                // record_and_check, so the model could burn the full step budget
                // on empty re-observes.
                if let Err(e) = loop_guard.record_and_check(&proposal.action) {
                    self.fail_task(task_id, format!("loop guard: {e}"))?;
                    return Ok(StepOutcome::Terminal);
                }
                *last_summary = Some("observe (no side effects)".into());
                self.record_step(task_id)?;
                thread::sleep(StdDuration::from_millis(50));
                return Ok(StepOutcome::Continue);
            }
            Action::Exclusive(_) => {
                self.fail_task(
                    task_id,
                    "product loop refuses exclusive input without exclusive consent",
                )?;
                return Ok(StepOutcome::Terminal);
            }
            Action::Targeted(_) | Action::Semantic(_) => {}
        }

        // Re-check after VLM (propose can take tens of seconds). User takeover of the
        // same window must pause before any further validate/act.
        if let Some(outcome) = self.apply_control_gate(task_id, &target, "post-propose")? {
            return Ok(outcome);
        }

        validate_action(&observation, &proposal.action)?;
        ensure_observation_binding(&observation, &proposal.observation_id)?;
        loop_guard.record_and_check(&proposal.action)?;

        let evaluated = self.evaluate_action_for_task(
            Some(task_id),
            &observation,
            &proposal.action,
            proposal.effect_claim.as_deref(),
            RiskLevel::R4,
            Some(&task.caller),
        )?;

        if evaluated.requires_takeover {
            self.store_pending(
                task_id,
                PendingAction {
                    approval_id: evaluated
                        .approval_id
                        .clone()
                        .unwrap_or_else(|| "takeover".into()),
                    action: proposal.action.clone(),
                    observation: observation.clone(),
                    target: target.clone(),
                    risk: evaluated.risk,
                    takeover_started: false,
                },
            );
            let _ = self.apply_command(
                task_id,
                TaskCommand::RequireApproval,
                format!("R4 user takeover required: {}", evaluated.rationale),
            );
            return Ok(StepOutcome::WaitExternal);
        }

        if evaluated.requires_approval {
            let approval_id = evaluated.approval_id.clone().ok_or_else(|| {
                LcuError::coded(ErrorCode::InternalError, "approval required without id")
            })?;
            self.store_pending(
                task_id,
                PendingAction {
                    approval_id: approval_id.clone(),
                    action: proposal.action.clone(),
                    observation: observation.clone(),
                    target: target.clone(),
                    risk: evaluated.risk,
                    takeover_started: false,
                },
            );
            let _ = self.apply_command(
                task_id,
                TaskCommand::RequireApproval,
                format!("approval required ({approval_id}): {}", evaluated.rationale),
            );
            return Ok(StepOutcome::WaitExternal);
        }

        let receipt = match self.perform_gated_action(
            task_id,
            &target,
            &observation,
            &proposal.action,
            None,
        ) {
            Ok(r) => r,
            Err(e) if e.code() == ErrorCode::WaitingUser => {
                // Act-time same-window takeover → pause, do not fail the task.
                let _ = self.apply_command(
                    task_id,
                    TaskCommand::PauseByUser,
                    format!("paused (act-time): {e}"),
                );
                return Ok(StepOutcome::WaitExternal);
            }
            Err(e) => return Err(e),
        };

        *last_summary = Some(match &proposal.action {
            Action::Semantic(lcu_core::action::SemanticAction::SetValue { element_id, value }) => {
                format!(
                    "SUCCESS set_value {element_id} value_len={}",
                    value.len()
                )
            }
            _ => receipt
                .message
                .clone()
                .unwrap_or_else(|| format!("acted step={}", task.step_count + 1)),
        });
        {
            let mut rec = self.get_task(task_id)?;
            rec.last_action_hash = Some(proposal.action.action_hash());
            self.persist_task(&rec, None);
        }

        thread::sleep(StdDuration::from_millis(250));
        Ok(StepOutcome::Continue)
    }

    fn try_execute_pending(&self, task_id: &TaskId) -> LcuResult<Option<StepOutcome>> {
        let pending = {
            let map = self.pending.lock().expect("pending lock");
            map.get(&task_id.0).cloned()
        };
        let Some(pending) = pending else {
            return Ok(None);
        };

        let approvals = self.approvals.lock().expect("approvals lock");
        let status = approvals
            .get(&pending.approval_id)
            .map(|r| r.status)
            .ok_or_else(|| {
                LcuError::coded(ErrorCode::ApprovalInvalid, "pending approval missing")
            })?;
        drop(approvals);

        use lcu_core::approval::ApprovalStatus;
        match status {
            ApprovalStatus::Pending => return Ok(Some(StepOutcome::WaitExternal)),
            ApprovalStatus::Denied
            | ApprovalStatus::Expired
            | ApprovalStatus::Consumed
            | ApprovalStatus::Invalidated => {
                self.clear_pending(task_id);
                self.fail_task(
                    task_id,
                    format!("approval {} not usable: {status:?}", pending.approval_id),
                )?;
                return Ok(Some(StepOutcome::Terminal));
            }
            ApprovalStatus::Approved => {}
        }

        if pending.risk.requires_user_takeover() {
            if !pending.takeover_started {
                return Ok(Some(StepOutcome::WaitExternal));
            }
            self.clear_pending(task_id);
            {
                let mut rec = self.get_task(task_id)?;
                rec.summary = Some(format!(
                    "takeover_complete: user handled R4 action (approval {}); re-observe",
                    pending.approval_id
                ));
                self.persist_task(&rec, None);
            }
            if let Ok(task) = self.get_task(task_id) {
                if task.state == TaskState::WaitingApproval {
                    let _ = self.apply_command(
                        task_id,
                        TaskCommand::Approve,
                        "R4 takeover completed by user; resume without auto-exec",
                    );
                }
            }
            thread::sleep(StdDuration::from_millis(250));
            return Ok(Some(StepOutcome::Continue));
        }

        let grant = self.consume_approval_for_execution(
            &pending.approval_id,
            &pending.action,
            &pending.observation,
            task_id,
        )?;

        let fresh = match self.backend.observe(&pending.target) {
            Ok(o) => o,
            Err(e) => {
                self.clear_pending(task_id);
                self.fail_task(task_id, format!("post-approval re-observe failed: {e}"))?;
                return Ok(Some(StepOutcome::Terminal));
            }
        };
        if fresh.target.app_id != pending.target.app_id
            || fresh.target.pid != pending.target.pid
            || fresh.target.window_id != pending.target.window_id
        {
            self.clear_pending(task_id);
            self.fail_task(
                task_id,
                "post-approval target changed; approval invalidated — re-submit if needed",
            )?;
            return Ok(Some(StepOutcome::Terminal));
        }
        if let Err(e) = validate_action(&fresh, &pending.action) {
            self.clear_pending(task_id);
            self.fail_task(
                task_id,
                format!("post-approval action no longer valid: {e}"),
            )?;
            return Ok(Some(StepOutcome::Terminal));
        }

        let task = self.get_task(task_id)?;
        let reeval = self.evaluate_action_for_task(
            Some(task_id),
            &fresh,
            &pending.action,
            None,
            RiskLevel::R4,
            Some(&task.caller),
        )?;
        if reeval.requires_takeover || reeval.risk.requires_user_takeover() {
            self.clear_pending(task_id);
            let _ = self.apply_command(
                task_id,
                TaskCommand::RequireApproval,
                format!(
                    "post-approval re-risk requires user takeover: {}",
                    reeval.rationale
                ),
            );
            return Ok(Some(StepOutcome::WaitExternal));
        }

        if let Some(outcome) =
            self.apply_control_gate(task_id, &pending.target, "post-approval")?
        {
            if matches!(outcome, StepOutcome::Terminal) {
                self.clear_pending(task_id);
            }
            return Ok(Some(outcome));
        }
        let _agent = AgentSessionGuard::begin(self.backend.as_ref(), &pending.target)?;
        let _receipt = self.perform_gated_action(
            task_id,
            &pending.target,
            &fresh,
            &pending.action,
            Some(&grant),
        )?;
        self.clear_pending(task_id);
        thread::sleep(StdDuration::from_millis(250));
        Ok(Some(StepOutcome::Continue))
    }

    /// Product control gate: only backend `taken_over` / `target_lost` stop work.
    fn apply_control_gate(
        &self,
        task_id: &TaskId,
        app_target: &AppTarget,
        phase: &str,
    ) -> LcuResult<Option<StepOutcome>> {
        let state = self.backend.detect_user_conflict(app_target)?;
        tracing::info!(
            task_id = %task_id.0,
            phase,
            app_id = %app_target.app_id,
            pid = app_target.pid,
            window_id = app_target.window_id,
            ?state,
            "control gate"
        );
        match state {
            lcu_core::surface::ControlState::TargetLost => {
                self.fail_task(
                    task_id,
                    format!("target lost ({phase}): application/window/tab gone"),
                )?;
                Ok(Some(StepOutcome::Terminal))
            }
            lcu_core::surface::ControlState::TakenOver => {
                match self.apply_command(
                    task_id,
                    TaskCommand::PauseByUser,
                    format!("paused ({phase}): control taken_over"),
                ) {
                    Ok(_) => tracing::info!(task_id = %task_id.0, phase, "paused by user takeover"),
                    Err(e) => tracing::warn!(
                        task_id = %task_id.0,
                        phase,
                        error = %e,
                        "PauseByUser command failed"
                    ),
                }
                Ok(Some(StepOutcome::WaitExternal))
            }
            lcu_core::surface::ControlState::None => Ok(None),
        }
    }

    fn store_pending(&self, task_id: &TaskId, pending: PendingAction) {
        self.pending
            .lock()
            .expect("pending lock")
            .insert(task_id.0.clone(), pending);
    }

    fn clear_pending(&self, task_id: &TaskId) {
        self.pending
            .lock()
            .expect("pending lock")
            .remove(&task_id.0);
    }

    /// Sole production entry for real OS side effects (crate-private; not IPC-exposed).
    pub(crate) fn perform_gated_action(
        &self,
        task_id: &TaskId,
        target: &AppTarget,
        observation: &lcu_core::observation::AppObservation,
        action: &Action,
        grant: Option<&ExecutionGrant>,
    ) -> LcuResult<lcu_core::task::ActionReceipt> {
        let task = self.get_task(task_id)?;
        if task.state != TaskState::Running {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                format!("cannot act in state {:?}", task.state),
            ));
        }

        let judgement = self
            .effect_guard
            .judge(&lcu_core::effect_guard::EffectContext {
                observation,
                action,
                model_effect_claim: None,
                task_authorized_max_risk: RiskLevel::R4,
            });
        let _ = task;

        if judgement.risk.requires_user_takeover() {
            return Err(LcuError::coded(
                ErrorCode::PermissionDenied,
                format!(
                    "R4 actions require user takeover; Runtime will not perform them ({})",
                    judgement.rationale
                ),
            ));
        }

        if judgement.risk.requires_per_action_approval() {
            let grant = grant.ok_or_else(|| {
                LcuError::coded(
                    ErrorCode::PermissionDenied,
                    format!(
                        "R3 action requires consumed GUI ExecutionGrant ({})",
                        judgement.rationale
                    ),
                )
            })?;
            if grant.task_id != *task_id || grant.action_hash != action.action_hash() {
                return Err(LcuError::coded(
                    ErrorCode::ApprovalInvalid,
                    "execution grant does not match action/task",
                ));
            }
        }

        match self.backend.detect_user_conflict(target)? {
            lcu_core::surface::ControlState::TargetLost => {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    "target lost at act time",
                ));
            }
            lcu_core::surface::ControlState::TakenOver => {
                return Err(LcuError::coded(
                    ErrorCode::WaitingUser,
                    "conflict: taken_over at act time",
                ));
            }
            lcu_core::surface::ControlState::None => {}
        }

        if let Some(eid) = action.referenced_element_id() {
            if eid.starts_with("nav_") || !observation.contains_element(eid) {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    format!("element {eid} missing or forbidden at act time"),
                ));
            }
        }

        let mut receipt = match action {
            Action::Semantic(sem) => self.backend.perform_semantic_action(target, sem)?,
            Action::Targeted(input) => self.backend.perform_targeted_input(target, input)?,
            other => {
                return Err(LcuError::coded(
                    ErrorCode::PermissionDenied,
                    format!(
                        "perform_gated_action refuses {:?}; only semantic/targeted after guard",
                        other.kind()
                    ),
                ));
            }
        };
        receipt.risk_level = judgement.risk;
        receipt.action_hash = action.action_hash();

        self.record_step(task_id)?;

        if !receipt.success {
            return Err(LcuError::coded(
                ErrorCode::InternalError,
                receipt
                    .message
                    .clone()
                    .unwrap_or_else(|| "platform action failed".into()),
            ));
        }
        Ok(receipt)
    }
}

/// Infer AppSelector when the caller did not pass `--app`.
pub fn resolve_selector(goal: &str, explicit: Option<AppSelector>) -> LcuResult<AppSelector> {
    if let Some(s) = explicit {
        if s.app_id.is_some() || s.pid.is_some() || s.window_title_contains.is_some() {
            return Ok(s);
        }
    }
    let g = goal.to_lowercase();
    let app_id = if g.contains("chrome") || g.contains("浏览器") || g.contains("browser") {
        Some("com.google.Chrome".into())
    } else if g.contains("finder") || g.contains("访达") {
        Some("com.apple.finder".into())
    } else if g.contains("textedit") || g.contains("文本编辑") {
        Some("com.apple.TextEdit".into())
    } else if g.contains("safari") {
        Some("com.apple.Safari".into())
    } else if g.contains("terminal") || g.contains("终端") {
        Some("com.apple.Terminal".into())
    } else {
        None
    };

    if app_id.is_none() {
        return Err(LcuError::coded(
            ErrorCode::InvalidRequest,
            "no app selector: pass --app <bundle_id> or name the app in the goal (Chrome/Finder/TextEdit)",
        ));
    }
    Ok(AppSelector {
        app_id,
        pid: None,
        window_title_contains: None,
    })
}

/// Default product actor: Qwen subprocess only (no heuristic auto-fallback).
///
/// - `LCU_VISION_ACTOR=auto` (default) / `qwen` / `vlm`: subprocess Qwen3-VL
pub fn default_product_actor() -> Arc<dyn VisionActor> {
    let _choice = std::env::var("LCU_VISION_ACTOR")
        .unwrap_or_else(|_| "auto".into())
        .to_lowercase();
    let repo = resolve_repo_root();
    if !qwen_assets_ready(&repo) {
        tracing::error!(
            repo = %repo.display(),
            "product actor: qwen assets missing; tasks will FAIL until model/worker ready"
        );
    } else {
        tracing::info!(
            repo = %repo.display(),
            "product actor: qwen subprocess"
        );
    }
    Arc::new(SubprocessVisionActor::from_repo_root(&repo))
}

fn resolve_repo_root() -> std::path::PathBuf {
    if let Some(p) = std::env::var_os("LCU_REPO_ROOT") {
        return std::path::PathBuf::from(p);
    }
    if let Ok(mut dir) = std::env::current_dir() {
        for _ in 0..6 {
            if dir.join("scripts/qwen3_vl_worker.py").exists() {
                return dir;
            }
            if !dir.pop() {
                break;
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
}

fn qwen_assets_ready(repo: &std::path::Path) -> bool {
    let worker = repo.join("scripts/qwen3_vl_worker.py");
    let model = std::env::var_os("LCU_MODEL_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repo.join("models/Qwen3-VL-4B-Instruct"));
    let python = repo.join(".venv/bin/python");
    let python_ok = python.exists() || which_python3();
    worker.is_file() && model.is_dir() && python_ok
}

fn which_python3() -> bool {
    std::process::Command::new("python3")
        .arg("-c")
        .arg("import sys; sys.exit(0)")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::RuntimePaths;
    use lcu_core::action::SemanticAction;
    use lcu_core::observation::{
        AppObservation, ElementNode, ModelSize, ObservationId, Rect, TransformId,
    };
    use lcu_core::task::ActionReceipt;
    use lcu_core::types::{CallerIdentity, Frame};
    use lcu_core::surface::ControlState;
    use lcu_platform::{PermissionFlag, PermissionState, PlatformBackend};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;
    use tempfile::tempdir;

    struct MockBackend {
        acts: AtomicU32,
        conflict: Mutex<ControlState>,
    }

    impl MockBackend {
        fn new(conflict: ControlState) -> Self {
            Self {
                acts: AtomicU32::new(0),
                conflict: Mutex::new(conflict),
            }
        }
    }

    impl PlatformBackend for MockBackend {
        fn resolve_target(&self, selector: &AppSelector) -> LcuResult<AppTarget> {
            Ok(AppTarget {
                app_id: selector
                    .app_id
                    .clone()
                    .unwrap_or_else(|| "com.apple.finder".into()),
                pid: 42,
                window_id: 7,
                window_title: "Mock".into(),
            })
        }

        fn observe(&self, target: &AppTarget) -> LcuResult<AppObservation> {
            let n = self.acts.load(Ordering::SeqCst);
            let elements = if n == 0 {
                vec![ElementNode {
                    id: "e1".into(),
                    role: "button".into(),
                    label: Some("Open".into()),
                    value: None,
                    frame: Rect {
                        x: 0.1,
                        y: 0.1,
                        width: 0.2,
                        height: 0.1,
                    },
                    actions: vec!["invoke".into()],
                }]
            } else {
                vec![]
            };
            Ok(AppObservation {
                observation_id: ObservationId(format!("obs_{n}")),
                timestamp_ms: 0,
                target: target.clone(),
                window_frame: Frame {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                model_size: ModelSize {
                    width: 100,
                    height: 100,
                },
                elements,
                transform_id: TransformId("t".into()),
                image_hash: None,
                capture_backend: None,
                image_png: None,
            })
        }

        fn perform_semantic_action(
            &self,
            _target: &AppTarget,
            action: &SemanticAction,
        ) -> LcuResult<ActionReceipt> {
            self.acts.fetch_add(1, Ordering::SeqCst);
            Ok(ActionReceipt {
                action: Action::Semantic(action.clone()),
                action_hash: Action::Semantic(action.clone()).action_hash(),
                capability_used: lcu_core::capability::CapabilityLevel::Semantic,
                risk_level: RiskLevel::R1,
                success: true,
                message: Some("mock ok".into()),
                executed_at: chrono::Utc::now(),
            })
        }

        fn perform_targeted_input(
            &self,
            _target: &AppTarget,
            action: &lcu_core::action::TargetedInput,
        ) -> LcuResult<ActionReceipt> {
            self.acts.fetch_add(1, Ordering::SeqCst);
            Ok(ActionReceipt {
                action: Action::Targeted(action.clone()),
                action_hash: Action::Targeted(action.clone()).action_hash(),
                capability_used: lcu_core::capability::CapabilityLevel::Targeted,
                risk_level: RiskLevel::R2,
                success: true,
                message: Some("mock targeted ok".into()),
                executed_at: chrono::Utc::now(),
            })
        }

        fn perform_exclusive_input(
            &self,
            _target: &AppTarget,
            _action: &lcu_core::action::TargetedInput,
        ) -> LcuResult<ActionReceipt> {
            Err(LcuError::coded(ErrorCode::NotImplemented, "no exclusive"))
        }

        fn detect_user_conflict(&self, _target: &AppTarget) -> LcuResult<ControlState> {
            Ok(*self.conflict.lock().expect("conflict"))
        }

        fn permission_state(&self) -> LcuResult<PermissionState> {
            Ok(PermissionState {
                screen_recording: PermissionFlag::Granted,
                accessibility: PermissionFlag::Granted,
                input_monitoring: PermissionFlag::Granted,
            })
        }
    }

    #[test]
    fn product_loop_executes_semantic_action_via_runtime() {
        let dir = tempdir().unwrap();
        let paths = RuntimePaths::from_root(dir.path());
        let rt = Arc::new(
            Runtime::new_for_test(paths, Arc::new(MockBackend::new(ControlState::None))).unwrap(),
        );
        let task = rt
            .submit_task(
                "click Open in Finder",
                CallerIdentity::HumanCli,
                Some(AppSelector {
                    app_id: Some("com.apple.finder".into()),
                    pid: None,
                    window_title_contains: None,
                }),
            )
            .unwrap();
        rt.run_task_to_completion(&task.task_id).unwrap();
        let done = rt.get_task(&task.task_id).unwrap();
        assert!(
            done.step_count >= 1 || done.state.is_terminal(),
            "expected progress, got state={:?} steps={}",
            done.state,
            done.step_count
        );
        assert!(done.step_count >= 1, "gated action must record a step");
    }


    #[test]
    fn control_gate_pauses_on_same_window_user_active() {
        let dir = tempdir().unwrap();
        let paths = RuntimePaths::from_root(dir.path());
        let rt = Arc::new(
            Runtime::new_for_test(
                paths,
                Arc::new(MockBackend::new(ControlState::TakenOver)),
            )
            .unwrap(),
        );
        let task = rt
            .submit_task(
                "type in Finder",
                CallerIdentity::HumanCli,
                Some(AppSelector {
                    app_id: Some("com.apple.finder".into()),
                    pid: None,
                    window_title_contains: None,
                }),
            )
            .unwrap();
        rt.run_task_to_completion(&task.task_id).unwrap();
        let done = rt.get_task(&task.task_id).unwrap();
        assert_eq!(
            done.state,
            TaskState::PausedByUser,
            "TakenOver must pause automation"
        );
    }

    #[test]
    fn control_gate_fails_on_target_lost() {
        let dir = tempdir().unwrap();
        let paths = RuntimePaths::from_root(dir.path());
        let rt = Arc::new(
            Runtime::new_for_test(
                paths,
                Arc::new(MockBackend::new(ControlState::TargetLost)),
            )
            .unwrap(),
        );
        let task = rt
            .submit_task(
                "click Open in Finder",
                CallerIdentity::HumanCli,
                Some(AppSelector {
                    app_id: Some("com.apple.finder".into()),
                    pid: None,
                    window_title_contains: None,
                }),
            )
            .unwrap();
        rt.run_task_to_completion(&task.task_id).unwrap();
        let done = rt.get_task(&task.task_id).unwrap();
        assert_eq!(done.state, TaskState::Failed);
    }
}
