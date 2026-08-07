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
        // Per-task decision maker (--actor override or Runtime default).
        let decision_actor = self.decision_actor(task.actor.as_deref());
        // One automatic retry: propose failures are frequently transient on MPS
        // (budget-cut generation, first-inference compile, momentary system
        // load). A single failed propose must not kill a task that would
        // succeed one attempt later. The worker-level budget and the Rust hard
        // timeout still bound total time.
        let mut proposal = None;
        for attempt in 0..2 {
            // A cancel/pause during propose (agent mode can park for minutes)
            // must not re-enter propose with a fresh decision request.
            if !self.task_is_running(task_id) {
                return Ok(StepOutcome::Continue);
            }
            match decision_actor.propose_action(&model_obs, &ctx) {
                Ok(p) => {
                    proposal = Some(p);
                    break;
                }
                Err(e) => {
                    if attempt == 0 {
                        // Abort (WaitingUser) is expected control flow, not a
                        // retryable failure.
                        if e.code() == ErrorCode::WaitingUser {
                            return Ok(StepOutcome::Continue);
                        }
                        tracing::warn!(
                            task_id = %task_id.0,
                            error = %e,
                            actor = decision_actor.name(),
                            propose_ms = propose_t0.elapsed().as_millis() as u64,
                            "vision propose failed; retrying once"
                        );
                        // Prefer pause-on-takeover over retrying against a user-owned window.
                        if let Some(outcome) =
                            self.apply_control_gate(task_id, &target, "propose-failed")?
                        {
                            return Ok(outcome);
                        }
                        continue;
                    }
                    tracing::warn!(
                        task_id = %task_id.0,
                        error = %e,
                        actor = decision_actor.name(),
                        propose_ms = propose_t0.elapsed().as_millis() as u64,
                        "vision propose failed twice; task FAILED (no heuristic auto-fallback)"
                    );
                    self.fail_task(
                        task_id,
                        format!("VLM propose failed twice ({}); queue continues", e),
                    )?;
                    return Ok(StepOutcome::Terminal);
                }
            }
        }
        let proposal = proposal.expect("proposal set by retry loop");
        let propose_ms = propose_t0.elapsed().as_millis() as u64;

        // Action payloads may embed model-typed content (set_value value,
        // type_text text); log only the redacted shape so credentials never
        // land in runtime logs verbatim.
        let redacted_action = serde_json::to_value(&proposal.action)
            .map(|v| crate::redact::redact_value(&v))
            .ok();
        tracing::info!(
            task_id = %task_id.0,
            step,
            propose_ms,
            action = ?redacted_action,
            last_action_summary = ?last_summary,
            actor = decision_actor.name(),
            "product step model proposal"
        );

        // The model proposal can take minutes (VLM); the user may have paused
        // or cancelled meanwhile. Drop the proposal entirely — proceeding would
        // register an approval / pending state for a task that must not act,
        // and RequireApproval from PausedByUser is an illegal transition that
        // left the task stuck Running with nobody driving it.
        if !self.task_is_running(task_id) {
            return Ok(StepOutcome::Continue);
        }

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

        ensure_observation_binding(&observation, &proposal.observation_id)?;
        if let Err(e) = validate_action(&observation, &proposal.action) {
            // Invalid model output is recoverable feedback, not a product crash.
            // Existing LoopGuard stops a model that repeats the same invalid action.
            loop_guard.record_and_check(&proposal.action)?;
            let valid_ids = observation.element_ids().take(16).collect::<Vec<_>>().join(",");
            *last_summary = Some(format!(
                "REJECTED action: {e}; valid_element_ids=[{valid_ids}]. Choose a valid current action"
            ));
            self.record_step(task_id)?;
            return Ok(StepOutcome::Continue);
        }
        loop_guard.record_and_check(&proposal.action)?;

        let evaluated = self.evaluate_action_for_task(
            Some(task_id),
            &observation,
            &proposal.action,
            proposal.effect_claim.as_deref(),
            RiskLevel::R4,
            Some(&task.caller),
        )?;

        // The control gate above is an RPC; the user may have paused during it.
        // Remove the approval this evaluation just registered (nobody would
        // consume it) and let the worker loop re-check the state.
        if !self.task_is_running(task_id) {
            if let Some(approval_id) = &evaluated.approval_id {
                self.approvals.lock().expect("approvals lock").remove(approval_id);
            }
            return Ok(StepOutcome::Continue);
        }

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

        use lcu_core::approval::ApprovalStatus;
        let status = {
            let mut approvals = self.approvals.lock().expect("approvals lock");
            let request = approvals
                .get_mut(&pending.approval_id)
                .ok_or_else(|| {
                    LcuError::coded(ErrorCode::ApprovalInvalid, "pending approval missing")
                })?;
            // A pending approval that outlived its 5-minute binding must not keep
            // the task stuck in WaitingApproval forever.
            if request.status == ApprovalStatus::Pending
                && request.binding.is_expired(chrono::Utc::now())
            {
                request.status = ApprovalStatus::Expired;
            }
            request.status
        };

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
        // Risk-only re-evaluation: no approval is registered here. Registering
        // one would leave a ghost approval (nobody consumes it) after the
        // original grant was already consumed.
        let reeval = self.reevaluate_action_for_task(
            Some(task_id),
            &fresh,
            &pending.action,
            RiskLevel::R4,
            Some(&task.caller),
        )?;
        if reeval.requires_takeover || reeval.risk.requires_user_takeover() {
            // R4 escalation: the fresh observation changed the picture after the
            // user approved. Bind a brand-new pending approval so the GUI's
            // begin/complete_takeover can find it — otherwise the escalated
            // action would be silently dropped and the task stuck.
            let approval_id = self.insert_approval(
                Some(task_id),
                &fresh,
                pending.action.action_hash(),
                format!("post-approval re-risk requires user takeover: {}", reeval.rationale),
            );
            self.store_pending(
                task_id,
                PendingAction {
                    approval_id: approval_id.clone(),
                    action: pending.action.clone(),
                    observation: fresh,
                    target: pending.target.clone(),
                    risk: reeval.risk,
                    takeover_started: false,
                },
            );
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

    /// Pick the decision maker for a task: per-task `--actor` override, else
    /// the Runtime process default.
    fn decision_actor(&self, task_actor: Option<&str>) -> Arc<dyn VisionActor> {
        match task_actor {
            Some("agent") => self.agent_actor.clone(),
            Some("vlm") | Some("qwen") => self.actor.clone(),
            Some(other) => {
                tracing::warn!(actor = other, "unknown task actor; using runtime default");
                self.default_actor_arc()
            }
            None => self.default_actor_arc(),
        }
    }

    fn default_actor_arc(&self) -> Arc<dyn VisionActor> {
        match self.default_actor {
            crate::DecisionActor::Vlm => self.actor.clone(),
            crate::DecisionActor::Agent => self.agent_actor.clone(),
        }
    }

    fn task_is_running(&self, task_id: &TaskId) -> bool {
        matches!(
            self.get_task(task_id).map(|t| t.state),
            Ok(TaskState::Running)
        )
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
        // The pending entry is the only consumer of the task's approvals; once it
        // is gone (executed, denied, invalidated, failed) every approval of this
        // task is dead weight in the map. Purge them so the map cannot grow
        // unboundedly and stale approvals never surface in the GUI.
        let mut approvals = self.approvals.lock().expect("approvals lock");
        approvals.retain(|_, r| r.binding.task_id != *task_id);
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

/// Default product actors: the agent decision actor is the DEFAULT decision
/// maker; the local VLM subprocess is an optional fallback (device-bound,
/// often too slow on consumer hardware). Both are always available; the
/// process-level default is chosen from `LCU_VISION_ACTOR` and tasks may
/// override per-task via `--actor`.
///
/// - `LCU_VISION_ACTOR=auto` (default) / `agent`: tasks default to the
///   external agent (lcu decide / lcu act)
/// - `LCU_VISION_ACTOR=vlm` / `qwen`: tasks default to the local VLM
/// - unknown values warn and default to agent
///
/// Returns `(vlm_actor, agent_actor, default)`.
pub fn default_product_actor() -> (
    Arc<dyn VisionActor>,
    Arc<lcu_model::AgentActor>,
    crate::DecisionActor,
) {
    let choice = std::env::var("LCU_VISION_ACTOR")
        .unwrap_or_else(|_| "auto".into())
        .to_lowercase();
    let default = match choice.as_str() {
        "vlm" | "qwen" => {
            tracing::info!("product actor: default = vlm (local model)");
            crate::DecisionActor::Vlm
        }
        "agent" | "auto" => {
            tracing::info!("product actor: default = agent (lcu decide / lcu act)");
            crate::DecisionActor::Agent
        }
        other => {
            tracing::warn!(actor = other, "unknown LCU_VISION_ACTOR; defaulting to agent");
            crate::DecisionActor::Agent
        }
    };
    let repo = resolve_repo_root();
    if !qwen_assets_ready(&repo) {
        tracing::error!(
            repo = %repo.display(),
            "product actor: qwen assets missing; vlm tasks will FAIL until model/worker ready"
        );
    }
    let vlm: Arc<dyn VisionActor> = Arc::new(SubprocessVisionActor::from_repo_root(&repo));
    let agent = Arc::new(lcu_model::AgentActor::new());
    (vlm, agent, default)
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
    use crate::{InternalRequest, InternalResponse};
    use lcu_core::task::TaskRecord;
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

    fn agent_test_runtime() -> (Arc<Runtime>, Arc<lcu_model::AgentActor>) {
        let dir = tempdir().unwrap();
        let paths = RuntimePaths::from_root(dir.path());
        let mut rt = Runtime::new_for_test(paths, Arc::new(MockBackend::new(ControlState::None)))
            .unwrap();
        let agent = Arc::new(lcu_model::AgentActor::with_timeout(std::time::Duration::from_secs(30)));
        rt.set_agent_actor(agent.clone());
        (Arc::new(rt), agent)
    }

    fn agent_task(rt: &Runtime) -> TaskRecord {
        rt.submit_task_with_limits(
            "click Open in Finder",
            CallerIdentity::HumanCli,
            Some(AppSelector {
                app_id: Some("com.apple.finder".into()),
                pid: None,
                window_title_contains: None,
            }),
            None,
            Some("agent".into()),
        )
        .unwrap()
    }

    #[test]
    fn agent_actor_full_loop_via_ipc() {
        let (rt, _agent) = agent_test_runtime();
        let task = agent_task(&rt);
        let tid = task.task_id.clone();
        let rt2 = rt.clone();
        let worker = std::thread::spawn(move || rt2.run_task_to_completion(&tid));

        // Poll for the decision, then submit an invoke via IPC.
        let mut submitted = false;
        for _ in 0..300 {
            match rt.handle_internal(InternalRequest::GetDecision {
                task_id: task.task_id.0.clone(),
            }) {
                InternalResponse::Decision {
                    observation, ..
                } => {
                    let resp = rt.handle_internal(InternalRequest::SubmitDecision {
                        task_id: task.task_id.0.clone(),
                        observation_id: observation.observation_id.clone(),
                        action: serde_json::json!({
                            "kind": "semantic",
                            "type": "invoke",
                            "element_id": "e1"
                        }),
                        effect_claim: None,
                        expected_effect: None,
                        confidence: None,
                    });
                    assert!(
                        matches!(resp, InternalResponse::Submitted { .. }),
                        "submit rejected: {resp:?}"
                    );
                    submitted = true;
                    break;
                }
                // "no observation yet" is the normal early state while the
                // worker is still resolving/observing; keep polling.
                InternalResponse::Error {
                    code: ErrorCode::InvalidRequest,
                    ..
                } => {}
                other => {
                    panic!("get_decision errored: {other:?}");
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(submitted, "never got a decision to submit");

        // The worker consumes it and executes the action.
        let mut stepped = false;
        for _ in 0..300 {
            if rt.get_task(&task.task_id).unwrap().step_count >= 1 {
                stepped = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(stepped, "worker did not advance after submit");

        // Stop the loop; cancel aborts the next parked propose.
        let _ = rt.cancel_task(&task.task_id);
        worker.join().unwrap();
        assert_eq!(
            rt.get_task(&task.task_id).unwrap().state,
            TaskState::Cancelled
        );
    }

    #[test]
    fn agent_actor_cancel_interrupts_parked_propose() {
        let (rt, _agent) = agent_test_runtime();
        let task = agent_task(&rt);
        let tid = task.task_id.clone();
        let rt2 = rt.clone();
        let worker = std::thread::spawn(move || rt2.run_task_to_completion(&tid));

        // Wait until the worker is parked on a decision.
        let mut parked = false;
        for _ in 0..300 {
            if matches!(
                rt.handle_internal(InternalRequest::GetDecision {
                    task_id: task.task_id.0.clone(),
                }),
                InternalResponse::Decision { .. }
            ) {
                parked = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(parked, "worker never parked");

        let _ = rt.cancel_task(&task.task_id);
        // run_task_to_completion returns promptly (abort wakes the waiter).
        worker.join().unwrap();
        assert_eq!(
            rt.get_task(&task.task_id).unwrap().state,
            TaskState::Cancelled
        );
    }

    #[test]
    fn agent_actor_stale_observation_rejected() {
        let (rt, _agent) = agent_test_runtime();
        let task = agent_task(&rt);
        // No observation yet: submit must be rejected, not silently parked.
        let resp = rt.handle_internal(InternalRequest::SubmitDecision {
            task_id: task.task_id.0.clone(),
            observation_id: "obs_nonexistent".into(),
            action: serde_json::json!({"kind":"wait","milliseconds":1}),
            effect_claim: None,
            expected_effect: None,
            confidence: None,
        });
        assert!(
            matches!(resp, InternalResponse::Error { code: ErrorCode::InvalidRequest, .. }),
            "stale submit must error, got {resp:?}"
        );
    }

    #[test]
    fn task_level_actor_mixing_vlm_and_agent() {
        // Same runtime serves both decision makers: a default (VLM) task runs
        // to completion, then an --actor agent task parks for lcu decide.
        let (rt, _agent) = agent_test_runtime();
        // rt.set_actor is FakeActor in new_for_test (VLM side).
        let vlm_task = rt
            .submit_task_with_limits(
                "click Open in Finder",
                CallerIdentity::HumanCli,
                None,
                None,
                None, // default → VLM (FakeActor)
            )
            .unwrap();
        let tid = vlm_task.task_id.clone();
        let rt2 = rt.clone();
        let worker = std::thread::spawn(move || rt2.run_task_to_completion(&tid));
        // FakeActor emits wait/done quickly; task should terminate without
        // any agent decision being involved.
        worker.join().unwrap();
        assert!(
            rt.get_task(&vlm_task.task_id).unwrap().state.is_terminal(),
            "vlm task must not park on the agent actor"
        );

        // Now an agent task on the same runtime parks for a decision.
        let agent_task = rt
            .submit_task_with_limits(
                "click Open in Finder",
                CallerIdentity::HumanCli,
                None,
                None,
                Some("agent".into()),
            )
            .unwrap();
        let tid2 = agent_task.task_id.clone();
        let rt3 = rt.clone();
        let worker2 = std::thread::spawn(move || rt3.run_task_to_completion(&tid2));
        let mut parked = false;
        for _ in 0..300 {
            if matches!(
                rt.handle_internal(InternalRequest::GetDecision {
                    task_id: agent_task.task_id.0.clone(),
                }),
                InternalResponse::Decision { .. }
            ) {
                parked = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(parked, "agent task must park for a decision");
        rt.cancel_task(&agent_task.task_id).unwrap();
        worker2.join().unwrap();
        assert_eq!(
            rt.get_task(&agent_task.task_id).unwrap().state,
            TaskState::Cancelled
        );
    }
}
