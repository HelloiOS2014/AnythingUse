//! Desktop-owned product loop: gate → observe → decide → effect guard → act → re-observe.
//!
//! Real OS actions may only execute through [`Runtime::perform_gated_action`].
//!
//! App access and consequence confirmation park the task in `waiting_actor`;
//! after the gate the worker makes a fresh observation and hands
//! `transition_result` to the same actor kind. Foreground fallback also
//! discards the old proposal before re-observing.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration as StdDuration;

use lcu_core::action::{Action, EffectClaim, SemanticAction, TargetedInput};
use lcu_core::approval::{ConsequenceGrant, ConsequenceIdentity, GateKind, GrantStatus, ScreenshotEvidence};
use lcu_core::effect_guard::EffectContext;
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::{AppObservation, AppSelector, AppTarget};
use lcu_core::risk::RiskLevel;
use lcu_core::task::{ControlMode, TaskCommand, TaskId, TaskState};
use lcu_model::{
    ensure_observation_binding, validate_action, validate_effect, LoopGuard, LoopGuardConfig,
    ModelElement, ModelObservation, ModelTaskContext, SubprocessVisionActor, VisionActor,
};

use crate::{consequence_identity_for, screenshot_evidence_for, AppAccessOutcome, Runtime};

/// Queued work item for the background product worker.
#[derive(Debug, Clone)]
pub struct WorkItem {
    pub task_id: TaskId,
    /// Set only by the external-Agent decision TTL timer.
    pub agent_timeout_observation: Option<String>,
}

/// A parked gate for one task. Only the confirmation request plus the runtime
/// consequence identity / exact-match screenshot evidence are kept — never an
/// executable Action (realignment §3.4).
#[derive(Debug, Clone)]
pub struct PendingGate {
    pub grant_id: String,
    pub kind: GateKind,
    pub task_id: TaskId,
    pub app_key: String,
    pub target: AppTarget,
    /// Runtime-extracted consequence identity (consequence gates).
    pub consequence: Option<ConsequenceIdentity>,
    /// Exact-match screenshot evidence (screenshot-only proposals).
    pub evidence: Option<ScreenshotEvidence>,
    /// transition_result handed to the actor after the gate passes.
    pub transition: String,
    pub takeover_started: bool,
}

/// Outcome of a single product step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// Continue the loop immediately.
    Continue,
    /// Park until a gate decision / resume / cancel changes state.
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
        tx.send(WorkItem {
            task_id,
            agent_timeout_observation: None,
        })
        .map_err(|_| {
            LcuError::coded(ErrorCode::RuntimeUnavailable, "task scheduler worker died")
        })
    }

    pub fn schedule_agent_timeout(
        &self,
        task_id: TaskId,
        observation_id: String,
        timeout: StdDuration,
    ) -> LcuResult<()> {
        let tx = self
            .tx
            .lock()
            .expect("scheduler lock")
            .as_ref()
            .cloned()
            .ok_or_else(|| {
                LcuError::coded(
                    ErrorCode::RuntimeUnavailable,
                    "task scheduler not started; start lcu-desktop (product path)",
                )
            })?;
        thread::spawn(move || {
            thread::sleep(timeout);
            let _ = tx.send(WorkItem {
                task_id,
                agent_timeout_observation: Some(observation_id),
            });
        });
        Ok(())
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
            if let Some(observation_id) = item.agent_timeout_observation {
                let task = self.get_task(&item.task_id);
                let still_waiting = task.as_ref().is_ok_and(|task| {
                    task.state == TaskState::WaitingActor
                        && task.last_observation_id.as_ref().is_some_and(|id| id.0 == observation_id)
                });
                if still_waiting && self.agent_actor.expire(&observation_id) {
                    let _ = self.apply_command(
                        &item.task_id,
                        TaskCommand::PauseByUser,
                        "external Agent decision timed out",
                    );
                }
                continue;
            }
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

    /// Drive one task until terminal, or until it parks on a gate / user pause.
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
                TaskState::Queued => {
                    let _ =
                        self.apply_command(task_id, TaskCommand::Start, "worker recovered start");
                }
                TaskState::Running | TaskState::WaitingActor => {}
                TaskState::Succeeded | TaskState::Failed | TaskState::Cancelled => return Ok(()),
            }

            match self.run_one_product_step(task_id, &mut loop_guard, &mut last_summary)? {
                StepOutcome::Continue => continue,
                StepOutcome::WaitExternal => return Ok(()),
                StepOutcome::Terminal => return Ok(()),
            }
        }
    }

    /// One gate → observe → decide → guard → (gate?) → act cycle on the product path.
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
        if task.state != TaskState::Running && task.state != TaskState::WaitingActor {
            return Ok(StepOutcome::WaitExternal);
        }
        if self.refresh_control_epoch(task_id)? {
            return Ok(StepOutcome::WaitExternal);
        }

        // Advance an approved gate first: no old action is replayed; the step
        // continues to fresh observe → actor proposal with the transition result.
        let mut transition: Option<String> = None;
        if let Some(outcome) = self.advance_gate(task_id, &mut transition)? {
            return Ok(outcome);
        }
        if task.state == TaskState::WaitingActor {
            // A waiting_actor task without a pending gate has no driver; park.
            return Ok(StepOutcome::WaitExternal);
        }

        self.check_step_budget(task_id)?;

        let selector = resolve_selector(&task.goal, task.app_selector.clone())?;
        if !self.reserve_serial_surface(task_id, &selector) {
            let mut rec = self.get_task(task_id)?;
            rec.summary = Some("waiting_surface: backend capacity is reserved by another task".into());
            self.persist_task(&rec, None);
            return Ok(StepOutcome::WaitExternal);
        }
        self.backend
            .bind_task_context(&task.goal, Some(task_id.0.as_str()));
        let target = match self.current_target_for(task_id) {
            Some(target) => target,
            None => {
                let target = self.backend.resolve_target(&selector).map_err(|e| {
                    self.backend.clear_task_context();
                    self.release_serial_surface(task_id);
                    LcuError::coded(
                        e.code(),
                        format!(
                            "resolve target failed ({:?}): {e}; pass --app with a running app id",
                            selector.app_id
                        ),
                    )
                })?;
                if !self.set_current_target(task_id, target.clone())? {
                    let mut rec = self.get_task(task_id)?;
                    rec.summary = Some(format!(
                        "waiting_target: {} pid={} window={}",
                        target.app_id, target.pid, target.window_id
                    ));
                    self.persist_task(&rec, None);
                    self.backend.clear_task_context();
                    return Ok(StepOutcome::WaitExternal);
                }
                target
            }
        };

        // App access gate (first control of a stable app identity).
        let app_key = self.stable_app_key(&target)?;
        match self.app_access_state(task_id, &app_key) {
            AppAccessOutcome::Allowed => {}
            AppAccessOutcome::Denied => {
                self.fail_task(
                    task_id,
                    format!("app access denied by user decision for {app_key}"),
                )?;
                return Ok(StepOutcome::Terminal);
            }
            AppAccessOutcome::RequestDecision => {
                let grant_id = self.insert_app_access_gate(task_id, &app_key, &target);
                self.park_for_gate(
                    task_id,
                    PendingGate {
                        grant_id: grant_id.clone(),
                        kind: GateKind::AppAccess,
                        task_id: task_id.clone(),
                        app_key: app_key.clone(),
                        target: target.clone(),
                        consequence: None,
                        evidence: None,
                        transition: format!("app access granted for {app_key}"),
                        takeover_started: false,
                    },
                    format!("app access required ({grant_id}): {app_key}"),
                )?;
                return Ok(StepOutcome::WaitExternal);
            }
        }

        // Backend control state only (taken_over / target_lost).
        if let Some(outcome) = self.apply_control_gate(task_id, &target, "pre-observe")? {
            return Ok(outcome);
        }

        let uses_agent = self.uses_agent_actor(task.actor.as_deref());
        let resumed_agent = if uses_agent {
            match task.last_observation_id.as_ref() {
                Some(id) => match self.agent_actor.take_submitted(&id.0) {
                    Ok(ready) => ready,
                    Err(e) => {
                        let _ = self.apply_command(
                            task_id,
                            TaskCommand::PauseByUser,
                            format!("external Agent decision ended: {e}"),
                        );
                        return Ok(StepOutcome::WaitExternal);
                    }
                },
                None => None,
            }
        } else {
            None
        };

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
            transition_result: transition.take(),
        };
        let propose_t0 = std::time::Instant::now();
        let (proposal, actor_name) = if uses_agent {
            match resumed_agent {
                Some((previous, previous_ctx, mut proposal)) => {
                    if !same_decision_surface(&previous, &model_obs, &proposal.action) {
                        self.agent_actor.begin_decision(&model_obs, &previous_ctx)?;
                        self.scheduler.schedule_agent_timeout(
                            task_id.clone(),
                            model_obs.observation_id.clone(),
                            self.agent_actor.timeout(),
                        )?;
                        self.apply_command(
                            task_id,
                            TaskCommand::WaitActor,
                            "Agent proposal became stale; waiting on fresh observation",
                        )?;
                        self.set_wait_reason(task_id, lcu_core::task::WaitReason::AgentDecision)?;
                        return Ok(StepOutcome::WaitExternal);
                    }
                    proposal.observation_id = observation.observation_id.clone();
                    (proposal, "agent".to_string())
                }
                None => {
                    self.agent_actor.begin_decision(&model_obs, &ctx)?;
                    self.scheduler.schedule_agent_timeout(
                        task_id.clone(),
                        model_obs.observation_id.clone(),
                        self.agent_actor.timeout(),
                    )?;
                    self.apply_command(
                        task_id,
                        TaskCommand::WaitActor,
                        "waiting for external Agent decision",
                    )?;
                    self.set_wait_reason(task_id, lcu_core::task::WaitReason::AgentDecision)?;
                    return Ok(StepOutcome::WaitExternal);
                }
            }
        } else {
            let decision_actor = self.decision_actor(task.actor.as_deref());
            let mut proposal = None;
            for attempt in 0..2 {
                if !self.task_is_running(task_id) {
                    return Ok(StepOutcome::Continue);
                }
                match decision_actor.propose_action(&model_obs, &ctx) {
                    Ok(p) => {
                        proposal = Some(p);
                        break;
                    }
                    Err(e) if attempt == 0 => {
                        tracing::warn!(
                            task_id = %task_id.0,
                            error = %e,
                            actor = decision_actor.name(),
                            propose_ms = propose_t0.elapsed().as_millis() as u64,
                            "vision propose failed; retrying once"
                        );
                        if let Some(outcome) =
                            self.apply_control_gate(task_id, &target, "propose-failed")?
                        {
                            return Ok(outcome);
                        }
                    }
                    Err(e) => {
                        self.fail_task(
                            task_id,
                            format!("VLM propose failed twice ({e}); queue continues"),
                        )?;
                        return Ok(StepOutcome::Terminal);
                    }
                }
            }
            (
                proposal.expect("proposal set by retry loop"),
                decision_actor.name().to_string(),
            )
        };
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
            effect = ?proposal.effect.as_ref().map(|e| e.kind),
            last_action_summary = ?last_summary,
            actor = actor_name,
            "product step model proposal"
        );

        // The model proposal can take minutes (VLM); the user may have paused
        // or cancelled meanwhile. Drop the proposal entirely — proceeding would
        // register a gate / pending state for a task that must not act.
        if !self.task_is_running(task_id) {
            return Ok(StepOutcome::Continue);
        }

        // Only an explicit Action::Done may complete the product task.
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
                if let Err(e) = loop_guard.record_and_check(&proposal.action) {
                    self.fail_task(task_id, format!("loop guard: {e}"))?;
                    return Ok(StepOutcome::Terminal);
                }
                *last_summary = Some("observe (no side effects)".into());
                self.record_step(task_id)?;
                thread::sleep(StdDuration::from_millis(50));
                return Ok(StepOutcome::Continue);
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
            loop_guard.record_and_check(&proposal.action)?;
            let valid_ids = observation.element_ids().take(16).collect::<Vec<_>>().join(",");
            *last_summary = Some(format!(
                "REJECTED action: {e}; valid_element_ids=[{valid_ids}]. Choose a valid current action"
            ));
            self.record_step(task_id)?;
            return Ok(StepOutcome::Continue);
        }
        // Trust boundary: executable proposals need a closed-set effect.
        if let Err(e) = validate_effect(&proposal.action, proposal.effect.as_ref()) {
            *last_summary = Some(format!("REJECTED effect: {e}"));
            self.record_step(task_id)?;
            return Ok(StepOutcome::Continue);
        }
        loop_guard.record_and_check(&proposal.action)?;

        let effect = proposal.effect.as_ref().expect("effect validated above");
        let evaluated = self.evaluate_action_for_task(
            Some(task_id),
            &observation,
            &proposal.action,
            Some(effect),
            RiskLevel::R4,
            Some(&task.caller),
        )?;

        // The control gate above is an RPC; the user may have paused during it.
        if !self.task_is_running(task_id) {
            return Ok(StepOutcome::Continue);
        }

        // Consequence gate: match an approved grant (§3.4), else park a fresh
        // gate — never replay the old action.
        if let Some(outcome) = self.consequence_step(
            task_id,
            &target,
            &app_key,
            &observation,
            &proposal.action,
            effect,
            &evaluated,
            last_summary,
        )? {
            return Ok(outcome);
        }

        thread::sleep(StdDuration::from_millis(250));
        Ok(StepOutcome::Continue)
    }

    /// Match the new proposal against an approved consequence grant, or park a
    /// fresh consequence/takeover gate. Returns `Some(outcome)` when the step
    /// ended (gate parked / executed / terminal).
    fn consequence_step(
        &self,
        task_id: &TaskId,
        target: &AppTarget,
        app_key: &str,
        observation: &AppObservation,
        action: &Action,
        effect: &EffectClaim,
        evaluated: &crate::EvaluatedAction,
        last_summary: &mut Option<String>,
    ) -> LcuResult<Option<StepOutcome>> {
        let grant = self.pending_consequence_grant(task_id, app_key);
        let Some(grant) = grant else {
            return self.park_or_execute(
                task_id,
                target,
                app_key,
                observation,
                action,
                effect,
                evaluated,
                last_summary,
                None,
            );
        };

        match self.match_grant(&grant, observation, action, effect) {
            // Exact match: consume the grant once and execute.
            Some(consumed) => {
                return self.park_or_execute(
                    task_id,
                    target,
                    app_key,
                    observation,
                    action,
                    effect,
                    evaluated,
                    last_summary,
                    Some(consumed),
                );
            }
            None => {
                // Same-candidate or different proposal: the old grant must not
                // wait for later consumption (§3.4.7). Same-candidate proposals
                // re-confirm at the grant's own risk even if the actor lowered
                // the effect kind; truly different proposals are judged anew.
                let same_candidate = grant
                    .screenshot_evidence
                    .as_ref()
                    .map(|_| is_same_candidate(&grant, observation, action))
                    .unwrap_or(false);
                self.invalidate_consequence_grant(&grant.grant_id.0);
                let grant_risk = if same_candidate {
                    if grant.effect_kind
                        == lcu_core::action::EffectKind::Credential
                        || grant.effect_kind == lcu_core::action::EffectKind::Financial
                        || grant.effect_kind == lcu_core::action::EffectKind::PermissionChange
                    {
                        RiskLevel::R4
                    } else {
                        RiskLevel::R3
                    }
                } else {
                    evaluated.risk
                };
                let evaluated = if same_candidate {
                    crate::EvaluatedAction {
                        risk: grant_risk,
                        requires_takeover: grant_risk.requires_user_takeover(),
                        unknown: false,
                        rationale: "same high-risk candidate as the confirmed proposal; re-confirm or takeover required".into(),
                    }
                } else {
                    evaluated.clone()
                };
                return self.park_or_execute(
                    task_id,
                    target,
                    app_key,
                    observation,
                    action,
                    effect,
                    &evaluated,
                    last_summary,
                    None,
                );
            }
        }
    }

    /// Consume or reject a grant against the current proposal (§3.4):
    /// - Runtime consequence identity equal → consume;
    /// - screenshot-only: exact `image_hash + action_hash` after re-observe → consume;
    /// - otherwise `None` (grant invalidated by the caller).
    fn match_grant(
        &self,
        grant: &ConsequenceGrant,
        observation: &AppObservation,
        action: &Action,
        effect: &EffectClaim,
    ) -> Option<ConsequenceGrant> {
        if proposal_matches_grant(grant, observation, action, effect) {
            return self.consume_consequence_grant(&grant.grant_id.0).ok();
        }
        None
    }

    /// Park a fresh gate (or handoff) for this proposal, or execute when
    /// allowed. With `consumed_grant` the proposal already matched an approved
    /// grant and executes.
    fn park_or_execute(
        &self,
        task_id: &TaskId,
        target: &AppTarget,
        app_key: &str,
        observation: &AppObservation,
        action: &Action,
        effect: &EffectClaim,
        evaluated: &crate::EvaluatedAction,
        last_summary: &mut Option<String>,
        consumed_grant: Option<ConsequenceGrant>,
    ) -> LcuResult<Option<StepOutcome>> {
        if evaluated.unknown {
            // Actor cannot classify the consequence: stop and ask the user.
            let _ = self.apply_command(
                task_id,
                TaskCommand::PauseByUser,
                format!("request user: actor cannot classify consequence ({})", evaluated.rationale),
            );
            let mut rec = self.get_task(task_id)?;
            rec.summary = Some(format!(
                "waiting_user: actor cannot classify consequence: {}",
                evaluated.rationale
            ));
            self.persist_task(&rec, None);
            return Ok(Some(StepOutcome::WaitExternal));
        }

        if evaluated.requires_takeover {
            let grant_id = self.insert_consequence_gate(
                task_id,
                app_key,
                observation,
                action,
                effect,
                &lcu_core::effect_guard::EffectJudgement {
                    risk: evaluated.risk,
                    rationale: evaluated.rationale.clone(),
                    model_claim_overridden: false,
                    unknown: false,
                },
            );
            self.park_for_gate(
                task_id,
                PendingGate {
                    grant_id: grant_id.clone(),
                    kind: GateKind::Takeover,
                    task_id: task_id.clone(),
                    app_key: app_key.to_string(),
                    target: target.clone(),
                    consequence: Some(consequence_identity_for(observation, action)),
                    evidence: screenshot_evidence_for(observation, action, effect),
                    transition: "takeover complete; re-observe".into(),
                    takeover_started: false,
                },
                format!("R4 user takeover required ({grant_id}): {}", evaluated.rationale),
            )?;
            return Ok(Some(StepOutcome::WaitExternal));
        }

        if evaluated.risk.requires_per_action_approval() {
            let grant_id = self.insert_consequence_gate(
                task_id,
                app_key,
                observation,
                action,
                effect,
                &lcu_core::effect_guard::EffectJudgement {
                    risk: evaluated.risk,
                    rationale: evaluated.rationale.clone(),
                    model_claim_overridden: false,
                    unknown: false,
                },
            );
            self.park_for_gate(
                task_id,
                PendingGate {
                    grant_id: grant_id.clone(),
                    kind: GateKind::Consequence,
                    task_id: task_id.clone(),
                    app_key: app_key.to_string(),
                    target: target.clone(),
                    consequence: Some(consequence_identity_for(observation, action)),
                    evidence: screenshot_evidence_for(observation, action, effect),
                    transition: "consequence confirmed; re-observe".into(),
                    takeover_started: false,
                },
                format!("confirmation required ({grant_id}): {}", evaluated.rationale),
            )?;
            return Ok(Some(StepOutcome::WaitExternal));
        }

        let receipt = match self.perform_gated_action(
            task_id,
            target,
            observation,
            action,
            Some(effect),
            consumed_grant.as_ref(),
        ) {
            Ok(r) => r,
            Err(e) if e.code() == ErrorCode::WaitingUser => {
                // Act-time same-window takeover → pause, do not fail the task.
                let _ = self.apply_command(
                    task_id,
                    TaskCommand::PauseByUser,
                    format!("paused (act-time): {e}"),
                );
                return Ok(Some(StepOutcome::WaitExternal));
            }
            Err(e) if e.code() == ErrorCode::ForegroundRequired => {
                // Background path unavailable. App access already disclosed the
                // foreground fallback. Activation invalidates this proposal;
                // the next loop re-observes before the Actor proposes again.
                let task = self.get_task(task_id)?;
                if task.control_mode == ControlMode::BackgroundOnly {
                    self.fail_task(
                        task_id,
                        format!(
                            "background_only control mode: background delivery unavailable ({e})"
                        ),
                    )?;
                    return Ok(Some(StepOutcome::Terminal));
                }
                self.backend.activate_target(target)?;
                *last_summary = Some(
                    "target activated; discarded pre-activation proposal and re-observing".into(),
                );
                self.record_step(task_id)?;
                return Ok(Some(StepOutcome::Continue));
            }
            Err(e) if is_recoverable_action_error(&e) => {
                *last_summary = Some(format!(
                    "ACTION_REJECTED by execution layer: {e}; re-observe and choose a current element"
                ));
                self.record_step(task_id)?;
                return Ok(Some(StepOutcome::Continue));
            }
            Err(e) => return Err(e),
        };

        *last_summary = Some(proposal_action_summary(action, &receipt));
        {
            let mut rec = self.get_task(task_id)?;
            rec.last_action_hash = Some(action.action_hash());
            self.persist_task(&rec, None);
        }
        Ok(None)
    }

    /// Advance an approved gate. Successful gates return `None` so this same
    /// step continues to fresh observe and hands the transition to the Actor.
    fn advance_gate(
        &self,
        task_id: &TaskId,
        transition: &mut Option<String>,
    ) -> LcuResult<Option<StepOutcome>> {
        let pending = {
            let map = self.pending.lock().expect("pending lock");
            map.get(&task_id.0).cloned()
        };
        let Some(pending) = pending else {
            return Ok(None);
        };

        let status = {
            let mut gates = self.gates.lock().expect("gates lock");
            let request = gates.get_mut(&pending.grant_id).ok_or_else(|| {
                LcuError::coded(ErrorCode::ApprovalInvalid, "pending gate request missing")
            })?;
            // A pending gate that outlived its TTL must not keep the task in
            // WaitingActor forever.
            if request.status == GrantStatus::Pending && request.is_expired(chrono::Utc::now()) {
                request.status = GrantStatus::Expired;
            }
            request.status
        };

        match status {
            GrantStatus::Pending => return Ok(Some(StepOutcome::WaitExternal)),
            GrantStatus::Denied
            | GrantStatus::Expired
            | GrantStatus::Consumed
            | GrantStatus::Invalidated => {
                self.clear_task_pending(task_id);
                self.fail_task(
                    task_id,
                    format!("gate {} not usable: {status:?}", pending.grant_id),
                )?;
                return Ok(Some(StepOutcome::Terminal));
            }
            GrantStatus::Approved => {}
        }

        match pending.kind {
            GateKind::AppAccess => {
                // The decision was recorded at GUI time (allow_once / always_allow /
                // deny); deny already failed the task via the gate status read.
                *transition = Some(pending.transition.clone());
                self.clear_task_gate(task_id);
                Ok(None)
            }
            GateKind::Consequence => {
                // The grant stays Approved-unconsumed; the new proposal matches
                // it in `consequence_step`. Only the pending marker is dropped.
                *transition = Some(pending.transition.clone());
                self.pending.lock().expect("pending lock").remove(&task_id.0);
                Ok(None)
            }
            GateKind::Takeover => {
                // Human completed the takeover; never auto-execute anything.
                *transition = Some(pending.transition.clone());
                self.clear_task_gate(task_id);
                Ok(None)
            }
        }
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
                    Ok(_) => {
                        self.release_current_target(task_id);
                        tracing::info!(task_id = %task_id.0, phase, "paused by user takeover");
                    }
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

    fn uses_agent_actor(&self, task_actor: Option<&str>) -> bool {
        match task_actor {
            Some("agent") => true,
            Some("vlm") | Some("qwen") => false,
            Some(_) | None => self.default_actor == crate::DecisionActor::Agent,
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

    fn park_for_gate(
        &self,
        task_id: &TaskId,
        pending: PendingGate,
        message: impl Into<String>,
    ) -> LcuResult<()> {
        let reason = match pending.kind {
            GateKind::AppAccess => lcu_core::task::WaitReason::AppAccess,
            GateKind::Consequence | GateKind::Takeover => {
                lcu_core::task::WaitReason::Consequence
            }
        };
        self.apply_command(task_id, TaskCommand::WaitActor, message)?;
        self.set_wait_reason(task_id, reason)?;
        self.pending
            .lock()
            .expect("pending lock")
            .insert(task_id.0.clone(), pending);
        Ok(())
    }

    /// Stable signed app identity for permissions/grants. Chrome binds the
    /// connected extension instance + profile, not the bundle id alone (§3.2).
    fn stable_app_key(&self, target: &AppTarget) -> LcuResult<String> {
        self.backend.stable_app_identity(target)
    }

    /// Sole production entry for real OS side effects (crate-private; not IPC-exposed).
    pub(crate) fn perform_gated_action(
        &self,
        task_id: &TaskId,
        target: &AppTarget,
        observation: &AppObservation,
        action: &Action,
        effect: Option<&EffectClaim>,
        grant: Option<&ConsequenceGrant>,
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
            .judge(&EffectContext {
                observation,
                action,
                effect,
                task_authorized_max_risk: RiskLevel::R4,
            });
        let _ = task;

        if judgement.unknown {
            return Err(LcuError::coded(
                ErrorCode::PermissionDenied,
                "action has unknown consequence; Runtime will not perform it",
            ));
        }
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
                        "R3 action requires a consumed ConsequenceGrant ({})",
                        judgement.rationale
                    ),
                )
            })?;
            if grant.task_id != *task_id
                || grant.app_key != self.stable_app_key(target)?
                || grant.effect_kind != effect.map(|e| e.kind).unwrap_or(lcu_core::action::EffectKind::Unknown)
            {
                return Err(LcuError::coded(
                    ErrorCode::ApprovalInvalid,
                    "consumed consequence grant does not match this action/task",
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

        // Execution ladder: background semantic → provably isolated background
        // targeted → disclosed exact-target foreground fallback → explicit
        // failure. Native returns foreground_required only before
        // any input occurred, so the worker's gate handling never repeats a
        // side effect.
        let mut receipt = match action {
            Action::Semantic(sem) => self.backend.perform_semantic_action(target, sem),
            Action::Targeted(input) => self.backend.perform_targeted_input(target, input),
            other => {
                return Err(LcuError::coded(
                    ErrorCode::PermissionDenied,
                    format!(
                        "perform_gated_action refuses {:?}; only semantic/targeted after guard",
                        other.kind()
                    ),
                ));
            }
        }?;
        receipt.risk_level = judgement.risk;
        receipt.action_hash = action.action_hash();

        if !receipt.success {
            let message = receipt
                .message
                .clone()
                .unwrap_or_else(|| "platform action failed".into());
            return Err(LcuError::coded(
                if is_recoverable_action_message(&message) {
                    ErrorCode::InvalidRequest
                } else {
                    ErrorCode::InternalError
                },
                message,
            ));
        }
        self.record_step(task_id)?;
        Ok(receipt)
    }
}

/// Same-candidate detection (§3.4.7): on the same screenshot, the same
/// semantic element, or the same-kind input within the conservative hit range
/// of the original coordinates, still is the original high-risk candidate —
/// even if the actor changed coordinates or downgraded the effect kind.
fn is_same_candidate(grant: &ConsequenceGrant, observation: &AppObservation, action: &Action) -> bool {
    let Some(evidence) = grant.screenshot_evidence.as_ref() else {
        return false;
    };
    if !present_hashes_match(&evidence.image_hash, &observation.image_hash) {
        return false;
    }
    match (evidence.element_id.clone(), action) {
        (Some(id), Action::Semantic(SemanticAction::Invoke { element_id })) => {
            if id == *element_id {
                return true;
            }
            let identity = consequence_identity_for(observation, action);
            return grant.identity.operation == identity.operation
                && grant.identity.object.is_some()
                && grant.identity.object == identity.object;
        }
        _ => {}
    }
    let (kind, x, y) = match action {
        Action::Targeted(TargetedInput::Click { x, y, .. }) => ("click", Some(*x), Some(*y)),
        Action::Targeted(TargetedInput::TypeText { x, y, .. }) => ("type", *x, *y),
        Action::Targeted(TargetedInput::KeyCombo { .. }) => ("keys", None, None),
        _ => return false,
    };
    if evidence.input_kind.as_deref() != Some(kind) {
        return false;
    }
    // Conservative hit range: 2% of the window around the original point.
    const HIT_RANGE: f64 = 0.02;
    match (evidence.x, evidence.y, x, y) {
        (Some(ex), Some(ey), Some(nx), Some(ny)) => {
            (nx - ex).abs() <= HIT_RANGE && (ny - ey).abs() <= HIT_RANGE
        }
        (None, None, None, None) => true,
        _ => false,
    }
}

fn proposal_matches_grant(
    grant: &ConsequenceGrant,
    observation: &AppObservation,
    action: &Action,
    effect: &EffectClaim,
) -> bool {
    // Targeted input has no stable semantic object identity. A generic
    // "click" identity must never authorize a different coordinate.
    if !matches!(action, Action::Targeted(_))
        && grant.effect_kind == effect.kind
        && grant.identity == consequence_identity_for(observation, action)
    {
        return true;
    }
    grant.screenshot_evidence.as_ref().is_some_and(|ev| {
        grant.effect_kind == effect.kind
            && present_hashes_match(&ev.image_hash, &observation.image_hash)
            && ev.action_hash == action.action_hash()
    })
}

fn present_hashes_match(left: &Option<String>, right: &Option<String>) -> bool {
    matches!((left.as_deref(), right.as_deref()), (Some(a), Some(b)) if !a.is_empty() && a == b)
}

fn same_decision_surface(
    previous: &ModelObservation,
    current: &ModelObservation,
    action: &Action,
) -> bool {
    let same_target = previous.app_id == current.app_id
        && previous.pid == current.pid
        && previous.window_id == current.window_id
        && previous.window_frame == current.window_frame
        && previous.image_width == current.image_width
        && previous.image_height == current.image_height
        && previous.display_scale == current.display_scale;
    if !same_target {
        return false;
    }

    match action {
        // Coordinates are meaningful only for the exact pixels the Agent saw.
        // Compare the semantic blocker at that coordinate rather than the whole
        // tree: unrelated offscreen AX frame jitter must not cause an infinite
        // stale-decision loop, while an empty/changed blocker still fails closed.
        Action::Targeted(TargetedInput::Click { x, y, .. }) => {
            present_hashes_match(&previous.image_hash, &current.image_hash)
                && previous.elements.is_empty() == current.elements.is_empty()
                && semantic_model_element_at(previous, *x, *y, "invoke")
                    == semantic_model_element_at(current, *x, *y, "invoke")
        }
        Action::Targeted(TargetedInput::TypeText {
            x: Some(x),
            y: Some(y),
            ..
        }) => {
            present_hashes_match(&previous.image_hash, &current.image_hash)
                && previous.elements.is_empty() == current.elements.is_empty()
                && semantic_model_element_at(previous, *x, *y, "set_value")
                    == semantic_model_element_at(current, *x, *y, "set_value")
        }
        Action::Targeted(_) => {
            previous.elements == current.elements
                && present_hashes_match(&previous.image_hash, &current.image_hash)
        }
        // Done is a side-effect-free terminal claim. Re-observe and bind it to
        // the same resolved window/title, but tolerate dynamic pixel and
        // element drift inside that window. AX availability may not disappear
        // between the decision and the terminal verification.
        Action::Done { .. } => {
            previous.window_title == current.window_title
                && previous.elements.is_empty() == current.elements.is_empty()
        }
        // Element ids are observation-local. Preserve a semantic proposal only
        // when its referenced element and advertised capabilities are unchanged;
        // unrelated pixel or offscreen element drift does not invalidate it.
        Action::Semantic(_) => action.referenced_element_id().map_or_else(
            || previous.elements == current.elements,
            |id| {
                previous.elements.iter().find(|element| element.id == id)
                    == current.elements.iter().find(|element| element.id == id)
            },
        ),
        Action::Observe
        | Action::Wait { .. }
        | Action::Fail { .. }
        | Action::RequestUser { .. } => true,
    }
}

fn semantic_model_element_at<'a>(
    observation: &'a ModelObservation,
    x: f64,
    y: f64,
    required: &str,
) -> Option<&'a ModelElement> {
    observation
        .elements
        .iter()
        .filter(|element| {
            let [left, top, width, height] = element.frame;
            width > 0.0
                && height > 0.0
                && x >= left
                && x <= left + width
                && y >= top
                && y <= top + height
                && element
                    .capabilities
                    .iter()
                    .any(|capability| capability == required)
        })
        .min_by(|left, right| {
            let area = |element: &ModelElement| element.frame[2] * element.frame[3];
            area(left)
                .partial_cmp(&area(right))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn proposal_action_summary(
    action: &Action,
    receipt: &lcu_core::task::ActionReceipt,
) -> String {
    match action {
        Action::Semantic(SemanticAction::SetValue { element_id, value }) => {
            format!("SUCCESS set_value {element_id} value_len={}", value.len())
        }
        _ => receipt
            .message
            .clone()
            .unwrap_or_else(|| format!("acted step={}", receipt.executed_at.timestamp_millis())),
    }
}

fn is_recoverable_action_error(error: &LcuError) -> bool {
    matches!(
        error.code(),
        ErrorCode::InvalidRequest | ErrorCode::TaskFailed
    ) && is_recoverable_action_message(&error.to_string())
}

fn is_recoverable_action_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    (message.contains("element")
        && (message.contains("stale")
            || message.contains("unknown")
            || message.contains("not in last observation")
            || message.contains("missing")))
        || message.contains("selector not found")
        || message.trim_end().ends_with(": not found")
}

/// Resolve the AppSelector for a task. An explicit `--app`/`--pid` selector is
/// required: never guess an app from goal text, and never fall back to the
/// largest or frontmost window.
pub fn resolve_selector(goal: &str, explicit: Option<AppSelector>) -> LcuResult<AppSelector> {
    let _ = goal;
    if let Some(s) = explicit {
        if s.app_id.is_some() || s.pid.is_some() || s.window_title_contains.is_some() {
            return Ok(s);
        }
    }
    Err(LcuError::coded(
        ErrorCode::InvalidRequest,
        "no app selector: pass --app <bundle_id> (or --app pid:NNNN); explicit targeting is required",
    ))
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
    use crate::{AppAccessDecision, RuntimePaths};
    use chrono::Duration;
    use lcu_core::action::{EffectKind, MouseButton};
    use lcu_core::observation::{ElementNode, ModelSize, ObservationId, Rect, TransformId};
    use lcu_core::types::{CallerIdentity, Frame};
    use lcu_platform::NullBackend;
    use tempfile::tempdir;

    fn screenshot_observation(id: &str) -> AppObservation {
        AppObservation {
            observation_id: ObservationId(id.into()),
            timestamp_ms: 0,
            target: AppTarget {
                app_id: "example.app".into(),
                pid: 1,
                window_id: 1,
                window_title: "Example".into(),
            },
            window_frame: Frame { x: 0.0, y: 0.0, width: 100.0, height: 100.0 },
            model_size: ModelSize { width: 100, height: 100 },
            elements: vec![ElementNode {
                id: "e1".into(),
                role: "button".into(),
                label: Some("Send".into()),
                value: None,
                frame: Rect { x: 0.1, y: 0.1, width: 0.2, height: 0.1 },
                actions: vec!["invoke".into()],
            }],
            transform_id: TransformId("transform".into()),
            surface_scope: None,
            image_hash: Some("same-frame".into()),
            capture_backend: None,
            image_png: None,
        }
    }

    #[test]
    fn pending_gate_is_visible_only_after_task_is_parked() {
        let dir = tempdir().unwrap();
        let rt = Runtime::new_for_test(
            RuntimePaths::from_root(dir.path()),
            Arc::new(NullBackend),
        )
        .unwrap();
        let task = rt
            .submit_task("demo", CallerIdentity::HumanCli, None)
            .unwrap();
        rt.apply_command(&task.task_id, TaskCommand::Start, "start")
            .unwrap();
        let target = screenshot_observation("gate").target;
        let grant_id = rt.insert_app_access_gate(&task.task_id, "example.app", &target);
        assert!(rt.list_pending_gates().is_empty());

        rt.park_for_gate(
            &task.task_id,
            PendingGate {
                grant_id: grant_id.clone(),
                kind: GateKind::AppAccess,
                task_id: task.task_id.clone(),
                app_key: "example.app".into(),
                target,
                consequence: None,
                evidence: None,
                transition: "allowed".into(),
                takeover_started: false,
            },
            "gate",
        )
        .unwrap();

        assert_eq!(rt.get_task(&task.task_id).unwrap().state, TaskState::WaitingActor);
        assert!(rt.pending.lock().unwrap().contains_key(&task.task_id.0));
        assert_eq!(rt.list_pending_gates().len(), 1);
        rt.app_access_in_gui(&grant_id, AppAccessDecision::AllowOnce)
            .unwrap();
        assert_eq!(rt.get_task(&task.task_id).unwrap().state, TaskState::Running);
        let mut transition = None;
        assert_eq!(rt.advance_gate(&task.task_id, &mut transition).unwrap(), None);
        assert_eq!(
            rt.app_access_state(&task.task_id, "example.app"),
            AppAccessOutcome::Allowed
        );
    }

    #[test]
    fn screenshot_grant_accepts_fresh_id_but_not_a_different_coordinate() {
        let original = screenshot_observation("old");
        let fresh = screenshot_observation("fresh");
        let effect = EffectClaim::new(EffectKind::ExternalCommunication, "send");
        let action = Action::Targeted(TargetedInput::Click {
            x: 0.2,
            y: 0.3,
            button: MouseButton::Left,
        });
        let grant = ConsequenceGrant::new(
            TaskId::new(),
            original.target.app_id.clone(),
            &effect,
            consequence_identity_for(&original, &action),
            "send",
            screenshot_evidence_for(&original, &action, &effect),
            Duration::minutes(1),
        );

        assert!(proposal_matches_grant(&grant, &fresh, &action, &effect));
        let downgraded = EffectClaim::new(EffectKind::Navigate, "open");
        assert!(!proposal_matches_grant(&grant, &fresh, &action, &downgraded));
        let jittered = Action::Targeted(TargetedInput::Click {
            x: 0.21,
            y: 0.31,
            button: MouseButton::Left,
        });
        assert!(is_same_candidate(&grant, &fresh, &jittered));
        let moved = Action::Targeted(TargetedInput::Click {
            x: 0.8,
            y: 0.3,
            button: MouseButton::Left,
        });
        assert!(!proposal_matches_grant(&grant, &fresh, &moved, &effect));

        let mut no_hash = original.clone();
        no_hash.image_hash = None;
        let no_hash_grant = ConsequenceGrant::new(
            TaskId::new(),
            no_hash.target.app_id.clone(),
            &effect,
            consequence_identity_for(&no_hash, &action),
            "send",
            screenshot_evidence_for(&no_hash, &action, &effect),
            Duration::minutes(1),
        );
        assert!(!proposal_matches_grant(&no_hash_grant, &no_hash, &action, &effect));

        let semantic = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let semantic_grant = ConsequenceGrant::new(
            TaskId::new(),
            original.target.app_id.clone(),
            &effect,
            consequence_identity_for(&original, &semantic),
            "send",
            screenshot_evidence_for(&original, &semantic, &effect),
            Duration::minutes(1),
        );
        let mut renumbered = fresh.clone();
        renumbered.elements[0].id = "e2".into();
        let renumbered_action = Action::Semantic(SemanticAction::Invoke {
            element_id: "e2".into(),
        });
        assert!(is_same_candidate(
            &semantic_grant,
            &renumbered,
            &renumbered_action,
        ));
    }

    #[test]
    fn agent_proposal_is_bound_to_the_current_action_surface() {
        let previous_obs = screenshot_observation("previous");
        let mut current_obs = previous_obs.clone();
        current_obs.observation_id = ObservationId("current".into());
        current_obs.image_hash = Some("pixel-drift".into());
        let previous = ModelObservation::from(&previous_obs);
        let mut current = ModelObservation::from(&current_obs);
        let targeted = Action::Targeted(TargetedInput::Click {
            x: 0.2,
            y: 0.15,
            button: MouseButton::Left,
        });
        let semantic = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let done = Action::Done {
            summary: "Goal verified complete".into(),
        };

        assert!(!same_decision_surface(&previous, &current, &targeted));
        assert!(same_decision_surface(&previous, &current, &semantic));
        assert!(same_decision_surface(&previous, &current, &done));

        current.image_hash = previous.image_hash.clone();
        current.elements.push(ModelElement {
            id: "offscreen".into(),
            role: "AXImage".into(),
            label: Some("unrelated".into()),
            frame: [0.9, 1.0, 0.02, 0.02],
            capabilities: vec!["invoke".into()],
        });
        assert!(same_decision_surface(&previous, &current, &targeted));
        assert!(same_decision_surface(&previous, &current, &semantic));

        current.elements.clear();
        assert!(!same_decision_surface(&previous, &current, &semantic));
        assert!(!same_decision_surface(&previous, &current, &targeted));
        assert!(!same_decision_surface(&previous, &current, &done));

        current.elements = previous.elements.clone();
        current.window_title = "Different".into();
        assert!(!same_decision_surface(&previous, &current, &done));

        current.window_id += 1;
        assert!(!same_decision_surface(&previous, &current, &targeted));
    }
}
