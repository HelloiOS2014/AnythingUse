//! Local Computer Use runtime.
//!
//! Owns task state, private local IPC (no TCP), the global serial task queue,
//! and the only path through which platform actions may execute.

pub mod ipc;
pub mod limits;
pub mod loop_step;
pub mod paths;
pub mod private_entry;
pub mod redact;
pub mod single_instance;
pub mod sqlite_store;
pub mod worker;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use lcu_core::action::Action;
use lcu_core::approval::{ApprovalBinding, ApprovalRequest, ApprovalStatus};
use lcu_core::effect_guard::{EffectContext, EffectGuard, StaticEffectGuard};
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::{AppObservation, AppSelector, AppTarget};
use lcu_core::protocol::{
    DoctorReport, InternalProtocolVersion, PermissionCheck, PROTOCOL_SCHEMA_VERSION,
};
use lcu_core::risk::RiskLevel;
use lcu_core::task::{
    TaskCommand, TaskEvent, TaskId, TaskRecord, TaskState, TaskStateMachine,
};
use lcu_core::types::CallerIdentity;
use lcu_model::VisionActor;
use lcu_platform::{PermissionFlag, PlatformBackend};
use serde::{Deserialize, Serialize};

use crate::limits::{TaskBudget, TaskLimits};
use crate::paths::RuntimePaths;
use crate::private_entry::{PrivateEntry, PrivateEntryConfig};
use crate::sqlite_store::SqliteTaskStore;
use crate::worker::{default_product_actor, PendingAction, TaskScheduler};

/// Handle to the single-instance runtime owned by the desktop app.
pub struct Runtime {
    paths: RuntimePaths,
    entry: PrivateEntry,
    /// Sole task store (SQLite; tests use in-memory).
    store: Mutex<SqliteTaskStore>,
    approvals: Mutex<HashMap<String, ApprovalRequest>>,
    budgets: Mutex<HashMap<String, TaskBudget>>,
    limits: TaskLimits,
    backend: Arc<dyn PlatformBackend>,
    effect_guard: Arc<dyn EffectGuard>,
    actor: Arc<dyn VisionActor>,
    scheduler: TaskScheduler,
    /// High-risk actions waiting for GUI approval before gated execution.
    pending: Mutex<HashMap<String, PendingAction>>,
    /// Current task target for release on complete/cancel/fail/takeover.
    current_target: Mutex<Option<AppTarget>>,
}

impl Runtime {
    /// Production constructor: SQLite under `paths.root`, product VLM actor.
    pub fn new(paths: RuntimePaths, backend: Arc<dyn PlatformBackend>) -> LcuResult<Self> {
        paths.ensure_layout()?;
        let entry = PrivateEntry::prepare(PrivateEntryConfig {
            root: paths.root.clone(),
            protocol_version: InternalProtocolVersion::CURRENT,
        })?;
        let db_path = paths.root.join("tasks.db");
        let store = SqliteTaskStore::open(&db_path).map_err(|e| {
            LcuError::coded(
                ErrorCode::InternalError,
                format!("sqlite open failed: {e}"),
            )
        })?;
        if let Err(e) = backend.ensure_surfaces() {
            tracing::warn!(error = %e, "ensure_surfaces at runtime start");
        }
        let runtime = Self {
            paths,
            entry,
            store: Mutex::new(store),
            approvals: Mutex::new(HashMap::new()),
            budgets: Mutex::new(HashMap::new()),
            limits: TaskLimits::default(),
            backend,
            effect_guard: Arc::new(StaticEffectGuard),
            actor: default_product_actor(),
            scheduler: TaskScheduler::default(),
            pending: Mutex::new(HashMap::new()),
            current_target: Mutex::new(None),
        };
        // A previous process may have crashed with tasks in flight. They have no
        // worker and no budget behind them after restart; pause them so the user
        // decides (resume/cancel) instead of letting them squat queue slots.
        runtime.recover_stale_tasks();
        Ok(runtime)
    }

    /// Test constructor: in-memory SQLite + FakeActor (no real VLM).
    pub fn new_for_test(paths: RuntimePaths, backend: Arc<dyn PlatformBackend>) -> LcuResult<Self> {
        paths.ensure_layout()?;
        let entry = PrivateEntry::prepare(PrivateEntryConfig {
            root: paths.root.clone(),
            protocol_version: InternalProtocolVersion::CURRENT,
        })?;
        Ok(Self {
            paths,
            entry,
            store: Mutex::new(SqliteTaskStore::open_in_memory()?),
            approvals: Mutex::new(HashMap::new()),
            budgets: Mutex::new(HashMap::new()),
            limits: TaskLimits::default(),
            backend,
            effect_guard: Arc::new(StaticEffectGuard),
            actor: Arc::new(lcu_model::FakeActor),
            scheduler: TaskScheduler::default(),
            pending: Mutex::new(HashMap::new()),
            current_target: Mutex::new(None),
        })
    }

    pub fn backend(&self) -> &dyn PlatformBackend {
        self.backend.as_ref()
    }

    /// Override the product vision actor (tests).
    pub fn set_actor(&mut self, actor: Arc<dyn VisionActor>) {
        self.actor = actor;
    }

    /// Preload VLM weights (desktop background).
    pub fn warm_vision_actor(&self) -> LcuResult<()> {
        self.actor.warm_up()
    }

    /// Override step/duration limits (catalog items / tests).
    pub fn set_limits(&mut self, limits: TaskLimits) {
        self.limits = limits;
    }

    fn persist_task(&self, record: &TaskRecord, event: Option<&TaskEvent>) {
        if let Ok(store) = self.store.lock() {
            let _ = store.upsert_task(record);
            if let Some(ev) = event {
                let _ = store.push_event(ev);
            }
        }
    }

    fn release_current_target(&self) {
        let target = self.current_target.lock().expect("current_target").take();
        if let Some(t) = target {
            let _ = self.backend.release(&t);
        }
        self.backend.clear_task_context();
    }

    pub(crate) fn set_current_target(&self, target: AppTarget) {
        *self.current_target.lock().expect("current_target") = Some(target);
    }

    pub fn paths(&self) -> &RuntimePaths {
        &self.paths
    }

    pub fn private_entry(&self) -> &PrivateEntry {
        &self.entry
    }

    /// Observe a target via the Runtime-owned backend (no side effects).
    pub fn observe_target(&self, target: &AppTarget) -> LcuResult<AppObservation> {
        self.backend.observe(target)
    }

    /// Resolve a target via the Runtime-owned backend.
    pub fn resolve_target(&self, selector: &AppSelector) -> LcuResult<AppTarget> {
        self.backend.resolve_target(selector)
    }

    /// Submit a high-level natural-language task and enqueue the product worker.
    ///
    /// Task stays [`TaskState::Queued`] until the serial worker claims it. Caller is
    /// display-only (`source` / `source_name`); no multi-tenant auth.
    pub fn submit_task(
        &self,
        goal: impl Into<String>,
        caller: CallerIdentity,
        app_selector: Option<AppSelector>,
    ) -> LcuResult<TaskRecord> {
        self.submit_task_with_limits(goal, caller, app_selector, None)
    }

    /// Submit with optional per-task step budget (CLI `--max-steps`).
    pub fn submit_task_with_limits(
        &self,
        goal: impl Into<String>,
        caller: CallerIdentity,
        app_selector: Option<AppSelector>,
        max_steps: Option<u32>,
    ) -> LcuResult<TaskRecord> {
        let active = self
            .list_tasks()
            .into_iter()
            // PausedByUser tasks release the execution slot and do not consume
            // worker time; excluding them keeps recovered/paused tasks from
            // permanently filling the queue.
            .filter(|t| !t.state.is_terminal() && t.state != TaskState::PausedByUser)
            .count() as u32;
        if active >= self.limits.max_queue_depth {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                format!(
                    "global queue full ({active}/{}); cancel or wait for tasks to finish",
                    self.limits.max_queue_depth
                ),
            ));
        }
        let record = TaskRecord::new(goal, caller, app_selector);
        let event = TaskEvent {
            task_id: record.task_id.clone(),
            state: record.state,
            at: Utc::now(),
            message: match max_steps {
                Some(n) => format!("task queued (max_steps={n})"),
                None => "task queued for serial product worker".into(),
            },
            step: Some(0),
        };
        self.persist_task(&record, Some(&event));
        let mut limits = self.limits.clone();
        if let Some(n) = max_steps {
            limits.max_steps = n.max(1);
        }
        self.budgets
            .lock()
            .expect("budgets")
            .insert(record.task_id.0.clone(), TaskBudget::new(limits));
        if self.scheduler.is_started() {
            self.scheduler.enqueue(record.task_id.clone())?;
        }
        Ok(record)
    }

    /// Pending GUI approvals (desktop process only — not via socket).
    pub fn list_pending_approvals(&self) -> Vec<ApprovalUiLaunch> {
        let approvals = self.approvals.lock().expect("approvals lock");
        let pending_map = self.pending.lock().expect("pending lock");
        let tasks = self.list_tasks();
        approvals
            .values()
            .filter(|r| r.status == ApprovalStatus::Pending)
            .map(|r| {
                let task_id = r.binding.task_id.0.clone();
                let goal = tasks
                    .iter()
                    .find(|t| t.task_id == r.binding.task_id)
                    .map(|t| t.goal.clone())
                    .unwrap_or_default();
                let action_summary = pending_map
                    .values()
                    .find(|p| p.approval_id == r.approval_id.0)
                    .map(|p| describe_action_for_ui(&p.action))
                    .unwrap_or_else(|| format!("action_hash={}", r.binding.action_hash));
                let pending = pending_map
                    .values()
                    .find(|p| p.approval_id == r.approval_id.0);
                let risk = pending.map(|p| p.risk).unwrap_or(RiskLevel::R3);
                let risk_note = format!("{risk:?}");
                let requires_takeover = risk.requires_user_takeover();
                let takeover_started = pending.map(|p| p.takeover_started).unwrap_or(false);
                ApprovalUiLaunch {
                    approval_id: r.approval_id.0.clone(),
                    gui_only: true,
                    message: if goal.is_empty() {
                        r.reason.clone()
                    } else {
                        format!("goal: {goal}")
                    },
                    binding_hash: r.binding_hash.clone(),
                    task_id,
                    target_app: r.binding.target_app.clone(),
                    action_summary,
                    impact: format!("{} [{}]", r.reason, risk_note),
                    requires_takeover,
                    takeover_started,
                }
            })
            .collect()
    }

    /// Desktop GUI: approve the pending binding (R3 only — R4 uses takeover).
    pub fn approve_pending_in_gui(&self, approval_id: &str) -> LcuResult<()> {
        let (is_takeover, has_pending) = {
            let pending = self.pending.lock().expect("pending lock");
            let entry = pending.values().find(|p| p.approval_id == approval_id);
            (
                entry.map(|p| p.risk.requires_user_takeover()),
                entry.is_some(),
            )
        };
        if !has_pending {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "no pending action bound to this approval; approval is stale or already handled",
            ));
        }
        if is_takeover == Some(true) {
            return Err(LcuError::coded(
                ErrorCode::PermissionDenied,
                "R4 requires begin_takeover then complete_takeover; not plain approve",
            ));
        }
        let binding = self.approval_binding(approval_id)?;
        self.approve_in_gui(approval_id, &binding)
    }

    /// Desktop GUI: deny a pending approval.
    pub fn deny_pending_in_gui(&self, approval_id: &str) -> LcuResult<()> {
        let task_id = {
            let mut approvals = self.approvals.lock().expect("approvals lock");
            let request = approvals.get_mut(approval_id).ok_or_else(|| {
                LcuError::coded(ErrorCode::ApprovalInvalid, "unknown approval id")
            })?;
            request.deny_in_gui()?;
            request.binding.task_id.clone()
        };
        {
            let mut pending = self.pending.lock().expect("pending lock");
            pending.retain(|_, p| p.approval_id != approval_id);
        }
        if task_id.0 != "pending" {
            let _ = self.fail_task(
                &task_id,
                format!("approval {approval_id} denied by user in GUI"),
            );
        }
        Ok(())
    }

    /// Desktop GUI: user begins R4 human takeover.
    pub fn begin_takeover_in_gui(&self, approval_id: &str) -> LcuResult<()> {
        let mut pending = self.pending.lock().expect("pending lock");
        let entry = pending
            .values_mut()
            .find(|p| p.approval_id == approval_id)
            .ok_or_else(|| {
                LcuError::coded(ErrorCode::ApprovalInvalid, "unknown pending takeover")
            })?;
        if !entry.risk.requires_user_takeover() {
            return Err(LcuError::coded(
                ErrorCode::PermissionDenied,
                "begin_takeover is only for R4 actions",
            ));
        }
        entry.takeover_started = true;
        Ok(())
    }

    /// Desktop GUI: mark R4 takeover complete (Runtime never executes the R4 action).
    pub fn complete_takeover_in_gui(&self, approval_id: &str) -> LcuResult<()> {
        {
            let pending = self.pending.lock().expect("pending lock");
            let entry = pending
                .values()
                .find(|p| p.approval_id == approval_id)
                .ok_or_else(|| {
                    LcuError::coded(ErrorCode::ApprovalInvalid, "unknown pending takeover")
                })?;
            if !entry.risk.requires_user_takeover() {
                return Err(LcuError::coded(
                    ErrorCode::PermissionDenied,
                    "complete_takeover is only for R4 actions",
                ));
            }
            if !entry.takeover_started {
                return Err(LcuError::coded(
                    ErrorCode::ApprovalInvalid,
                    "R4 takeover not started; call begin_takeover first",
                ));
            }
        }
        let binding = self.approval_binding(approval_id)?;
        self.approve_in_gui(approval_id, &binding)
    }

    pub fn get_task(&self, task_id: &TaskId) -> LcuResult<TaskRecord> {
        let store = self.store.lock().expect("store lock");
        store
            .get_task(task_id)?
            .ok_or_else(|| LcuError::coded(ErrorCode::TaskNotFound, "task not found"))
    }

    pub fn list_tasks(&self) -> Vec<TaskRecord> {
        self.store
            .lock()
            .expect("store lock")
            .list_tasks()
            .unwrap_or_default()
    }

    /// After a crash / kill, tasks left in Queued/Running/WaitingApproval have
    /// no worker and no budget behind them. Pause them for the user to decide,
    /// rebuild in-memory budgets (seeded from the persisted step count so a task
    /// that already ran 40 steps cannot run a fresh 100), and release any
    /// backend context (e.g. a Chrome tab lease) the dead process held.
    fn recover_stale_tasks(&self) {
        let stale: Vec<TaskRecord> = self
            .list_tasks()
            .into_iter()
            .filter(|t| !t.state.is_terminal())
            .collect();
        let recovered = stale.len();
        for mut record in stale {
            let was_paused = record.state == TaskState::PausedByUser;
            if !was_paused {
                if let Err(e) = self.apply_command(
                    &record.task_id,
                    TaskCommand::PauseByUser,
                    "recovered after previous process exit; paused for user to decide",
                ) {
                    tracing::warn!(task_id = %record.task_id.0, error = %e, "recovery transition failed");
                    continue;
                }
                record = match self.get_task(&record.task_id) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::warn!(task_id = %record.task_id.0, error = %e, "recovery re-read failed");
                        continue;
                    }
                };
            }
            let mut budget = TaskBudget::new(self.limits.clone());
            budget.step_count = record.step_count;
            self.budgets
                .lock()
                .expect("budgets")
                .insert(record.task_id.0.clone(), budget);
        }
        // Release whatever the dead process may have held (Chrome tab lease);
        // no-op for the macOS window backend.
        self.backend.clear_task_context();
        tracing::info!(recovered, "stale tasks recovered to paused");
    }

    pub(crate) fn apply_command(
        &self,
        task_id: &TaskId,
        command: TaskCommand,
        message: impl Into<String>,
    ) -> LcuResult<TaskRecord> {
        let mut record = self.get_task(task_id)?;
        TaskStateMachine::apply(&mut record, command)?;
        let event = TaskEvent {
            task_id: record.task_id.clone(),
            state: record.state,
            at: Utc::now(),
            message: message.into(),
            step: Some(record.step_count),
        };
        if record.state.is_terminal() {
            self.release_current_target();
            self.budgets
                .lock()
                .expect("budgets")
                .remove(&record.task_id.0);
        }
        self.persist_task(&record, Some(&event));
        Ok(record)
    }

    pub fn cancel_task(&self, task_id: &TaskId) -> LcuResult<TaskRecord> {
        self.apply_command(task_id, TaskCommand::Cancel, "task cancelled")
    }

    pub fn pause_task(&self, task_id: &TaskId) -> LcuResult<TaskRecord> {
        self.apply_command(task_id, TaskCommand::PauseByUser, "task paused by user")
    }

    pub fn resume_task(&self, task_id: &TaskId) -> LcuResult<TaskRecord> {
        let rec = self.apply_command(task_id, TaskCommand::Resume, "task resumed")?;
        if self.scheduler.is_started() && rec.state == lcu_core::task::TaskState::Running {
            let _ = self.scheduler.enqueue(rec.task_id.clone());
        }
        Ok(rec)
    }

    pub fn succeed_task(&self, task_id: &TaskId, summary: Option<String>) -> LcuResult<TaskRecord> {
        let mut rec = self.apply_command(task_id, TaskCommand::Succeed, "task succeeded")?;
        if let Some(s) = summary {
            rec.summary = Some(s);
            rec.updated_at = Utc::now();
            self.persist_task(&rec, None);
        }
        Ok(rec)
    }

    pub fn fail_task(&self, task_id: &TaskId, error: impl Into<String>) -> LcuResult<TaskRecord> {
        let err = error.into();
        let mut rec =
            self.apply_command(task_id, TaskCommand::Fail, format!("task failed: {err}"))?;
        rec.error = Some(err);
        rec.updated_at = Utc::now();
        self.persist_task(&rec, None);
        Ok(rec)
    }

    /// Check step budget before an observe/act cycle.
    pub fn check_step_budget(&self, task_id: &TaskId) -> LcuResult<()> {
        let budgets = self.budgets.lock().expect("budgets");
        let budget = budgets
            .get(&task_id.0)
            .ok_or_else(|| LcuError::coded(ErrorCode::TaskNotFound, "no budget for task"))?;
        budget.check_before_step(Utc::now())
    }

    pub fn record_step(&self, task_id: &TaskId) -> LcuResult<()> {
        let mut budgets = self.budgets.lock().expect("budgets");
        let budget = budgets
            .get_mut(&task_id.0)
            .ok_or_else(|| LcuError::coded(ErrorCode::TaskNotFound, "no budget for task"))?;
        budget.record_step(Utc::now());
        let step_count = budget.step_count;
        drop(budgets);
        let mut rec = self.get_task(task_id)?;
        rec.step_count = step_count;
        rec.updated_at = Utc::now();
        self.persist_task(&rec, None);
        Ok(())
    }

    /// Create and store a one-time ApprovalRequest; return its id.
    fn insert_approval(
        &self,
        task_id: Option<&TaskId>,
        observation: &AppObservation,
        action_hash: String,
        reason: String,
    ) -> String {
        let tid = task_id.cloned().unwrap_or_else(|| TaskId("pending".into()));
        let binding = ApprovalBinding::new(
            tid,
            observation.observation_id.clone(),
            action_hash,
            observation.target.app_id.clone(),
            Duration::minutes(5),
        );
        let request = ApprovalRequest::new(binding, reason);
        let approval_id = request.approval_id.0.clone();
        self.approvals
            .lock()
            .expect("approvals lock")
            .insert(approval_id.clone(), request);
        approval_id
    }

    /// Validate an action against observation + EffectGuard. Does not execute OS effects.
    pub fn evaluate_action(
        &self,
        observation: &AppObservation,
        action: &Action,
        model_effect_claim: Option<&str>,
        task_authorized_max_risk: RiskLevel,
    ) -> LcuResult<EvaluatedAction> {
        self.evaluate_action_for_task(
            None,
            observation,
            action,
            model_effect_claim,
            task_authorized_max_risk,
            None,
        )
    }

    /// Task-bound evaluation: R3/R4 require GUI approval; agents cannot self-approve.
    pub fn evaluate_action_for_task(
        &self,
        task_id: Option<&TaskId>,
        observation: &AppObservation,
        action: &Action,
        model_effect_claim: Option<&str>,
        task_authorized_max_risk: RiskLevel,
        caller: Option<&CallerIdentity>,
    ) -> LcuResult<EvaluatedAction> {
        self.evaluate_action_inner(
            task_id,
            observation,
            action,
            model_effect_claim,
            task_authorized_max_risk,
            caller,
            true,
        )
    }

    /// Risk-only re-evaluation: never registers a new approval. Used by
    /// `try_execute_pending` after the original grant was consumed — a second
    /// inserted approval would be a ghost (nobody consumes it), and an R4
    /// escalation there re-binds a fresh pending approval explicitly instead.
    pub(crate) fn reevaluate_action_for_task(
        &self,
        task_id: Option<&TaskId>,
        observation: &AppObservation,
        action: &Action,
        task_authorized_max_risk: RiskLevel,
        caller: Option<&CallerIdentity>,
    ) -> LcuResult<EvaluatedAction> {
        self.evaluate_action_inner(
            task_id,
            observation,
            action,
            None,
            task_authorized_max_risk,
            caller,
            false,
        )
    }

    fn evaluate_action_inner(
        &self,
        task_id: Option<&TaskId>,
        observation: &AppObservation,
        action: &Action,
        model_effect_claim: Option<&str>,
        task_authorized_max_risk: RiskLevel,
        caller: Option<&CallerIdentity>,
        create_approval: bool,
    ) -> LcuResult<EvaluatedAction> {
        if let Some(element_id) = action.referenced_element_id() {
            if element_id.starts_with("nav_") || !observation.contains_element(element_id) {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    format!("element_id {element_id} not in current observation"),
                ));
            }
        }

        let judgement = self.effect_guard.judge(&EffectContext {
            observation,
            action,
            model_effect_claim,
            task_authorized_max_risk,
        });

        if judgement.risk > task_authorized_max_risk {
            return Err(LcuError::coded(
                ErrorCode::PermissionDenied,
                format!(
                    "risk {:?} exceeds task max {:?}",
                    judgement.risk, task_authorized_max_risk
                ),
            ));
        }

        if judgement.risk.requires_user_takeover() {
            let approval_id = if create_approval {
                Some(self.insert_approval(
                    task_id,
                    observation,
                    action.action_hash(),
                    format!("risk R4 requires user takeover: {}", judgement.rationale),
                ))
            } else {
                None
            };
            let _ = caller;
            return Ok(EvaluatedAction {
                action_hash: action.action_hash(),
                risk: judgement.risk,
                requires_approval: true,
                requires_takeover: true,
                approval_id,
                rationale: judgement.rationale,
            });
        }

        if judgement.risk.requires_per_action_approval() {
            let approval_id = if create_approval {
                Some(self.insert_approval(
                    task_id,
                    observation,
                    action.action_hash(),
                    format!("risk {:?} requires user approval", judgement.risk),
                ))
            } else {
                None
            };
            let _ = caller;
            return Ok(EvaluatedAction {
                action_hash: action.action_hash(),
                risk: judgement.risk,
                requires_approval: true,
                requires_takeover: false,
                approval_id,
                rationale: judgement.rationale,
            });
        }

        Ok(EvaluatedAction {
            action_hash: action.action_hash(),
            risk: judgement.risk,
            requires_approval: false,
            requires_takeover: false,
            approval_id: None,
            rationale: judgement.rationale,
        })
    }

    pub fn approval_binding(&self, approval_id: &str) -> LcuResult<ApprovalBinding> {
        let approvals = self.approvals.lock().expect("approvals lock");
        approvals
            .get(approval_id)
            .map(|r| r.binding.clone())
            .ok_or_else(|| LcuError::coded(ErrorCode::ApprovalInvalid, "unknown approval id"))
    }

    /// `lcu approve` must only open the GUI; it never marks approval complete.
    pub fn request_approval_ui(&self, approval_id: &str) -> LcuResult<ApprovalUiLaunch> {
        let approvals = self.approvals.lock().expect("approvals lock");
        let request = approvals
            .get(approval_id)
            .ok_or_else(|| LcuError::coded(ErrorCode::ApprovalInvalid, "unknown approval id"))?;
        if request.status != ApprovalStatus::Pending {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                format!("approval not pending: {:?}", request.status),
            ));
        }
        let (requires_takeover, takeover_started) = {
            let pending = self.pending.lock().expect("pending lock");
            pending
                .values()
                .find(|p| p.approval_id == approval_id)
                .map(|p| (p.risk.requires_user_takeover(), p.takeover_started))
                .unwrap_or((false, false))
        };
        Ok(ApprovalUiLaunch {
            approval_id: approval_id.to_string(),
            gui_only: true,
            message: "open desktop confirmation UI; CLI cannot complete approval".into(),
            binding_hash: request.binding_hash.clone(),
            task_id: request.binding.task_id.0.clone(),
            target_app: request.binding.target_app.clone(),
            action_summary: format!("action_hash={}", request.binding.action_hash),
            impact: request.reason.clone(),
            requires_takeover,
            takeover_started,
        })
    }

    /// Direct CLI approval is forbidden by contract.
    pub fn approve_from_cli(&self, _approval_id: &str) -> LcuResult<()> {
        Err(LcuError::coded(
            ErrorCode::PermissionDenied,
            "lcu approve cannot complete approval in CLI; GUI presence required",
        ))
    }

    /// Agents must never approve their own high-risk actions.
    pub fn approve_from_agent(&self, _approval_id: &str) -> LcuResult<()> {
        Err(LcuError::coded(
            ErrorCode::PermissionDenied,
            "agent cannot approve own R3/R4 actions; GUI user presence required",
        ))
    }

    /// GUI-only finalization path (desktop shell).
    pub fn approve_in_gui(&self, approval_id: &str, expected: &ApprovalBinding) -> LcuResult<()> {
        let task_id = {
            let mut approvals = self.approvals.lock().expect("approvals lock");
            let request = approvals.get_mut(approval_id).ok_or_else(|| {
                LcuError::coded(ErrorCode::ApprovalInvalid, "unknown approval id")
            })?;
            request.approve_in_gui(expected, Utc::now())?;
            request.binding.task_id.clone()
        };
        if task_id.0 != "pending" {
            if let Ok(task) = self.get_task(&task_id) {
                if task.state == lcu_core::task::TaskState::WaitingApproval {
                    let _ = self.apply_command(
                        &task_id,
                        TaskCommand::Approve,
                        "approval granted in GUI",
                    );
                }
            }
            if self.scheduler.is_started() {
                let _ = self.scheduler.enqueue(task_id);
            }
        }
        Ok(())
    }

    /// Consume a one-time GUI approval and return an execution grant.
    pub fn consume_approval_for_execution(
        &self,
        approval_id: &str,
        action: &Action,
        observation: &AppObservation,
        task_id: &TaskId,
    ) -> LcuResult<ExecutionGrant> {
        let mut approvals = self.approvals.lock().expect("approvals lock");
        let request = approvals
            .get_mut(approval_id)
            .ok_or_else(|| LcuError::coded(ErrorCode::ApprovalInvalid, "unknown approval id"))?;
        if request.status != ApprovalStatus::Approved {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                format!("approval not approved: {:?}", request.status),
            ));
        }
        if request.binding.is_expired(Utc::now()) {
            request.status = ApprovalStatus::Expired;
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "approval expired before consume",
            ));
        }
        if request.binding.task_id != *task_id {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "approval task_id mismatch",
            ));
        }
        if request.binding.observation_id != observation.observation_id {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "approval observation_id mismatch",
            ));
        }
        if request.binding.action_hash != action.action_hash() {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "approval action_hash mismatch",
            ));
        }
        if request.binding.target_app != observation.target.app_id {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "approval target_app mismatch",
            ));
        }
        request.consume()?;
        Ok(ExecutionGrant {
            approval_id: approval_id.to_string(),
            action_hash: action.action_hash(),
            task_id: task_id.clone(),
            granted_at: Utc::now(),
        })
    }

    pub fn doctor_report(&self) -> DoctorReport {
        let perms = self
            .backend
            .permission_state()
            .unwrap_or(lcu_platform::PermissionState {
                screen_recording: PermissionFlag::NotDetermined,
                accessibility: PermissionFlag::NotDetermined,
                input_monitoring: PermissionFlag::NotDetermined,
            });

        let entry_status = self.entry.status();
        let mut blockers = Vec::new();
        if entry_status.listens_tcp {
            blockers.push("private entry must not listen on TCP".into());
        }
        if matches!(perms.accessibility, PermissionFlag::Denied) {
            blockers.push("accessibility permission denied".into());
        }
        if matches!(perms.screen_recording, PermissionFlag::Denied) {
            blockers.push("screen_recording permission denied".into());
        }

        let mut notes = vec![
            format!(
                "vision_actor={} (LCU_VISION_ACTOR=auto|qwen)",
                self.actor.name()
            ),
            if self.scheduler.is_started() {
                "product worker: scheduler running (observe→guard→act)".into()
            } else {
                "product worker: scheduler not started (desktop must call start_scheduler)".into()
            },
            "approvals: desktop tray only; CLI never completes approval".into(),
            "queue: global serial FIFO; waiting_user / paused release the execution slot".into(),
            "control: pause/stop only on taken_over, target_lost, or explicit commands".into(),
            format!("runtime root: {}", self.paths.root.display()),
        ];
        notes.extend(self.backend.doctor_surface_notes());

        DoctorReport {
            schema_version: PROTOCOL_SCHEMA_VERSION.to_string(),
            product: "local-computer-use".to_string(),
            platform: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            runtime_reachable: true,
            private_entry: entry_status,
            permissions: vec![
                PermissionCheck {
                    name: "screen_recording".into(),
                    state: format!("{:?}", perms.screen_recording).to_lowercase(),
                    required_for: vec!["observe".into()],
                },
                PermissionCheck {
                    name: "accessibility".into(),
                    state: format!("{:?}", perms.accessibility).to_lowercase(),
                    required_for: vec!["semantic_action".into(), "targeted_input".into()],
                },
                PermissionCheck {
                    name: "input_monitoring".into(),
                    state: format!("{:?}", perms.input_monitoring).to_lowercase(),
                    required_for: vec!["directed_input_same_window_takeover".into()],
                },
            ],
            blockers,
            notes,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvaluatedAction {
    pub action_hash: String,
    pub risk: RiskLevel,
    pub requires_approval: bool,
    /// R4: human must take over; Runtime must never execute even after GUI ack.
    #[serde(default)]
    pub requires_takeover: bool,
    pub approval_id: Option<String>,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApprovalUiLaunch {
    pub approval_id: String,
    pub gui_only: bool,
    pub message: String,
    pub binding_hash: String,
    #[serde(default)]
    pub task_id: String,
    #[serde(default)]
    pub target_app: String,
    #[serde(default)]
    pub action_summary: String,
    #[serde(default)]
    pub impact: String,
    #[serde(default)]
    pub requires_takeover: bool,
    #[serde(default)]
    pub takeover_started: bool,
}

/// Human-readable action text for the desktop confirmation dialog.
pub fn describe_action_for_ui(action: &Action) -> String {
    use lcu_core::action::{SemanticAction, TargetedInput};
    match action {
        Action::Observe => "observe".into(),
        Action::Wait { milliseconds } => format!("wait {milliseconds}ms"),
        Action::Done { summary } => format!("done: {summary}"),
        Action::Fail { reason } => format!("fail: {reason}"),
        Action::RequestUser { reason } => format!("request user: {reason}"),
        Action::Semantic(SemanticAction::Invoke { element_id }) => {
            format!("invoke element `{element_id}`")
        }
        Action::Semantic(SemanticAction::SetValue { element_id, value }) => {
            let preview: String = value.chars().take(40).collect();
            format!("set_value on `{element_id}` to \"{preview}\"")
        }
        Action::Semantic(SemanticAction::Focus { element_id }) => {
            format!("focus element `{element_id}`")
        }
        Action::Semantic(SemanticAction::Scroll {
            element_id,
            delta_x,
            delta_y,
        }) => format!(
            "scroll dx={delta_x} dy={delta_y} el={}",
            element_id.as_deref().unwrap_or("-")
        ),
        Action::Targeted(t) | Action::Exclusive(t) => match t {
            TargetedInput::Click { x, y, .. } => format!("click ({x:.2},{y:.2})"),
            TargetedInput::TypeText { text } => {
                let preview: String = text.chars().take(40).collect();
                format!("type \"{preview}\"")
            }
            TargetedInput::KeyCombo { keys } => format!("keys {}", keys.join("+")),
        },
    }
}

/// One-time grant returned after consuming a GUI approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionGrant {
    pub approval_id: String,
    pub action_hash: String,
    pub task_id: TaskId,
    pub granted_at: chrono::DateTime<Utc>,
}

/// Internal request frame (not a public Agent protocol).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum InternalRequest {
    Ping {
        protocol_version: u32,
    },
    Doctor,
    SubmitTask {
        goal: String,
        app_id: Option<String>,
        /// Display-only: `human` | `agent` (not auth).
        #[serde(default)]
        source: Option<String>,
        /// Display-only source label (e.g. `codex`, `grok`).
        #[serde(default)]
        source_name: Option<String>,
        #[serde(default)]
        max_steps: Option<u32>,
    },
    Status {
        task_id: String,
    },
    List,
    Cancel {
        task_id: String,
    },
    Pause {
        task_id: String,
    },
    Resume {
        task_id: String,
    },
    Result {
        task_id: String,
    },
    OpenApprovalUi {
        approval_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InternalResponse {
    Pong {
        protocol_version: u32,
    },
    Doctor {
        report: DoctorReport,
    },
    Task {
        task: TaskRecord,
    },
    Tasks {
        tasks: Vec<TaskRecord>,
    },
    ApprovalUi {
        launch: ApprovalUiLaunch,
    },
    Approvals {
        pending: Vec<ApprovalUiLaunch>,
    },
    Error {
        code: ErrorCode,
        message: String,
    },
}

impl Runtime {
    fn map_err_resp(err: LcuError) -> InternalResponse {
        InternalResponse::Error {
            code: err.code(),
            message: err.to_string(),
        }
    }

    /// Map display-only source fields to [`CallerIdentity`] (not credentials).
    fn caller_from_source(source: Option<String>, source_name: Option<String>) -> CallerIdentity {
        let source = source
            .unwrap_or_else(|| "human".into())
            .to_ascii_lowercase();
        match source.as_str() {
            "agent" => CallerIdentity::Agent {
                client_id: source_name.clone().unwrap_or_else(|| "agent".into()),
                name: source_name.unwrap_or_else(|| "agent".into()),
            },
            "gui" | "human_gui" => CallerIdentity::HumanGui,
            _ => CallerIdentity::HumanCli,
        }
    }

    pub fn handle_internal(&self, request: InternalRequest) -> InternalResponse {
        match request {
            InternalRequest::Ping { protocol_version } => {
                let remote = InternalProtocolVersion(protocol_version);
                if !InternalProtocolVersion::CURRENT.is_compatible(remote) {
                    return InternalResponse::Error {
                        code: ErrorCode::ProtocolMismatch,
                        message: format!(
                            "expected protocol {}, got {protocol_version}",
                            InternalProtocolVersion::CURRENT.0
                        ),
                    };
                }
                InternalResponse::Pong {
                    protocol_version: InternalProtocolVersion::CURRENT.0,
                }
            }
            InternalRequest::Doctor => InternalResponse::Doctor {
                report: self.doctor_report(),
            },
            InternalRequest::SubmitTask {
                goal,
                app_id,
                source,
                source_name,
                max_steps,
            } => {
                let caller = Self::caller_from_source(source, source_name);
                let selector = app_id.map(|app_id| {
                    if let Some(rest) = app_id.strip_prefix("pid:") {
                        if let Ok(pid) = rest.trim().parse::<u32>() {
                            return AppSelector {
                                app_id: None,
                                pid: Some(pid),
                                window_title_contains: None,
                            };
                        }
                    }
                    AppSelector {
                        app_id: Some(app_id),
                        pid: None,
                        window_title_contains: None,
                    }
                });
                match self.submit_task_with_limits(goal, caller, selector, max_steps) {
                    Ok(task) => InternalResponse::Task { task },
                    Err(err) => Self::map_err_resp(err),
                }
            }
            InternalRequest::Status { task_id } => match self.get_task(&TaskId(task_id)) {
                Ok(task) => InternalResponse::Task { task },
                Err(err) => Self::map_err_resp(err),
            },
            InternalRequest::List => InternalResponse::Tasks {
                tasks: self.list_tasks(),
            },
            InternalRequest::Cancel { task_id } => match self.cancel_task(&TaskId(task_id)) {
                Ok(task) => InternalResponse::Task { task },
                Err(err) => Self::map_err_resp(err),
            },
            InternalRequest::Pause { task_id } => match self.pause_task(&TaskId(task_id)) {
                Ok(task) => InternalResponse::Task { task },
                Err(err) => Self::map_err_resp(err),
            },
            InternalRequest::Resume { task_id } => match self.resume_task(&TaskId(task_id)) {
                Ok(task) => InternalResponse::Task { task },
                Err(err) => Self::map_err_resp(err),
            },
            InternalRequest::Result { task_id } => match self.get_task(&TaskId(task_id)) {
                Ok(task) => InternalResponse::Task { task },
                Err(err) => Self::map_err_resp(err),
            },
            InternalRequest::OpenApprovalUi { approval_id } => {
                match self.request_approval_ui(&approval_id) {
                    Ok(launch) => InternalResponse::ApprovalUi { launch },
                    Err(err) => Self::map_err_resp(err),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcu_core::action::SemanticAction;
    use lcu_core::observation::{
        AppTarget, ElementNode, ModelSize, ObservationId, Rect, TransformId,
    };
    use lcu_core::types::Frame;
    use lcu_platform::NullBackend;
    use tempfile::tempdir;

    fn test_runtime() -> Runtime {
        let dir = tempdir().unwrap();
        let path = dir.keep();
        let paths = RuntimePaths::from_root(path);
        Runtime::new_for_test(paths, Arc::new(NullBackend)).unwrap()
    }

    fn sample_obs(label: &str) -> AppObservation {
        AppObservation {
            observation_id: ObservationId("obs".into()),
            timestamp_ms: 0,
            target: AppTarget {
                app_id: "com.google.Chrome".into(),
                pid: 1,
                window_id: 1,
                window_title: "t".into(),
            },
            window_frame: Frame {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            model_size: ModelSize {
                width: 10,
                height: 10,
            },
            elements: vec![ElementNode {
                id: "e1".into(),
                role: "button".into(),
                label: Some(label.into()),
                value: None,
                frame: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.1,
                    height: 0.1,
                },
                actions: vec!["invoke".into()],
            }],
            transform_id: TransformId("t".into()),
            image_hash: None,
            capture_backend: None,
            image_png: None,
        }
    }




    #[test]
    fn high_risk_requires_gui_only_approval() {
        let rt = test_runtime();
        let obs = sample_obs("发送");
        let action = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let evaluated = rt
            .evaluate_action(&obs, &action, None, RiskLevel::R4)
            .unwrap();
        assert!(evaluated.requires_approval);
        let id = evaluated.approval_id.unwrap();
        let launch = rt.request_approval_ui(&id).unwrap();
        assert!(launch.gui_only);
        assert!(rt.approve_from_cli(&id).is_err());
    }


    #[test]
    fn cancel_makes_task_terminal_and_clears_context() {
        let rt = test_runtime();
        let task = rt
            .submit_task("demo", CallerIdentity::HumanCli, None)
            .unwrap();
        rt.apply_command(&task.task_id, TaskCommand::Start, "test start")
            .unwrap();
        let cancelled = rt.cancel_task(&task.task_id).unwrap();
        assert_eq!(cancelled.state, lcu_core::task::TaskState::Cancelled);
        assert!(rt.current_target.lock().unwrap().is_none());
    }


    #[test]
    fn global_queue_shared_fifo_and_full() {
        let mut rt = test_runtime();
        rt.set_limits(TaskLimits {
            max_queue_depth: 2,
            ..TaskLimits::default()
        });
        let t1 = rt
            .submit_task("a", CallerIdentity::HumanCli, None)
            .unwrap();
        let t2 = rt
            .submit_task("b", CallerIdentity::HumanCli, None)
            .unwrap();
        assert!(rt
            .submit_task("c", CallerIdentity::HumanCli, None)
            .is_err());
        match rt.handle_internal(InternalRequest::List) {
            InternalResponse::Tasks { tasks } => {
                assert_eq!(tasks.len(), 2);
                assert_eq!(tasks[0].task_id, t1.task_id);
                assert_eq!(tasks[1].task_id, t2.task_id);
            }
            other => panic!("list: {other:?}"),
        }
    }

    #[test]
    fn sqlite_survives_reload() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        let task_id = {
            let paths = RuntimePaths::from_root(&path);
            let rt = Runtime::new(paths, Arc::new(NullBackend)).unwrap();
            let task = rt
                .submit_task("persist me", CallerIdentity::HumanCli, None)
                .unwrap();
            task.task_id
        };
        let paths = RuntimePaths::from_root(&path);
        let rt2 = Runtime::new(paths, Arc::new(NullBackend)).unwrap();
        let got = rt2.get_task(&task_id).unwrap();
        assert_eq!(got.goal, "persist me");
        // Crash recovery: a non-terminal task from a dead process is paused for
        // the user and gets a rebuilt budget, not a silent permanent queue slot.
        assert_eq!(got.state, TaskState::PausedByUser);
        assert!(rt2.budgets.lock().unwrap().contains_key(&task_id.0));
    }

    #[test]
    fn r4_requires_takeover_not_plain_approve() {
        let rt = test_runtime();
        let task = rt
            .submit_task("pay", CallerIdentity::HumanCli, None)
            .unwrap();
        rt.apply_command(&task.task_id, TaskCommand::Start, "test start")
            .unwrap();
        let obs = sample_obs("支付");
        let action = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let evaluated = rt
            .evaluate_action_for_task(
                Some(&task.task_id),
                &obs,
                &action,
                None,
                RiskLevel::R4,
                Some(&CallerIdentity::HumanCli),
            )
            .unwrap();
        assert!(evaluated.requires_takeover);
        let approval_id = evaluated.approval_id.unwrap();
        {
            let mut pending = rt.pending.lock().unwrap();
            pending.insert(
                task.task_id.0.clone(),
                PendingAction {
                    approval_id: approval_id.clone(),
                    action: action.clone(),
                    observation: obs.clone(),
                    target: obs.target.clone(),
                    risk: RiskLevel::R4,
                    takeover_started: false,
                },
            );
        }
        assert!(rt.approve_pending_in_gui(&approval_id).is_err());
        assert!(rt.complete_takeover_in_gui(&approval_id).is_err());
        rt.begin_takeover_in_gui(&approval_id).unwrap();
        rt.complete_takeover_in_gui(&approval_id).unwrap();
    }

    #[test]
    fn two_phase_approval_consume_once_rejects_replay() {
        let rt = test_runtime();
        let task = rt
            .submit_task("submit form", CallerIdentity::HumanCli, None)
            .unwrap();
        let obs = sample_obs("发送");
        let action = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let evaluated = rt
            .evaluate_action_for_task(
                Some(&task.task_id),
                &obs,
                &action,
                None,
                RiskLevel::R4,
                Some(&CallerIdentity::HumanCli),
            )
            .unwrap();
        let approval_id = evaluated.approval_id.unwrap();
        let binding = rt.approval_binding(&approval_id).unwrap();
        rt.approve_in_gui(&approval_id, &binding).unwrap();
        let grant = rt
            .consume_approval_for_execution(&approval_id, &action, &obs, &task.task_id)
            .unwrap();
        assert_eq!(grant.action_hash, action.action_hash());
        assert!(rt
            .consume_approval_for_execution(&approval_id, &action, &obs, &task.task_id)
            .is_err());
    }
}
