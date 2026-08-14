//! Local Computer Use runtime.
//!
//! Owns task state, private local IPC (no TCP), the global serial task queue,
//! and the only path through which platform actions may execute.

pub mod ipc;
pub mod limits;
pub mod paths;
pub mod private_entry;
pub mod redact;
pub mod single_instance;
pub mod sqlite_store;
pub mod worker;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{Duration, Utc};
use lcu_core::action::{Action, EffectClaim, EffectKind, SemanticAction, TargetedInput};
use lcu_core::approval::{
    AppAccessDecision, AppPermission, ConsequenceGrant, ConsequenceIdentity, ForegroundGrant,
    GateKind, GateRequest, GrantId, GrantStatus, ScreenshotEvidence,
};
use lcu_core::effect_guard::{EffectContext, EffectGuard, StaticEffectGuard};
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::{AppObservation, AppSelector, AppTarget};
use lcu_core::protocol::{
    DoctorReport, InternalProtocolVersion, PermissionCheck, PROTOCOL_SCHEMA_VERSION,
};
use lcu_core::risk::RiskLevel;
use lcu_core::task::{
    ControlMode, TaskCommand, TaskEvent, TaskId, TaskRecord, TaskState, TaskStateMachine,
};
use lcu_core::types::CallerIdentity;
use lcu_model::VisionActor;
use lcu_platform::{PermissionFlag, PlatformBackend};
use serde::{Deserialize, Serialize};

use crate::limits::{TaskBudget, TaskLimits};
use crate::paths::RuntimePaths;
use crate::private_entry::{PrivateEntry, PrivateEntryConfig};
use crate::sqlite_store::SqliteTaskStore;
use crate::worker::{default_product_actor, PendingGate, TaskScheduler};

/// Which decision maker a task uses when it does not override via `--actor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionActor {
    /// Local Qwen3-VL subprocess.
    Vlm,
    /// External agent via lcu decide / lcu act.
    Agent,
}

/// Handle to the single-instance runtime owned by the desktop app.
pub struct Runtime {
    paths: RuntimePaths,
    entry: PrivateEntry,
    /// Sole task store (SQLite; tests use in-memory).
    store: Mutex<SqliteTaskStore>,
    /// Pending gate requests visible to the desktop GUI. CLI/Agents can never
    /// finalize them; a gate carries no executable Action to replay.
    gates: Mutex<HashMap<String, GateRequest>>,
    /// Approved, not-yet-consumed grants (app access / foreground / consequence).
    grants: Mutex<HashMap<String, Grant>>,
    /// Persistent app permissions (always_allow only; revocable in settings).
    app_permissions: Mutex<HashMap<String, AppPermission>>,
    /// Task-scoped allow_once app access keys (released at task terminal).
    allow_once: Mutex<std::collections::HashSet<(String, String)>>,
    budgets: Mutex<HashMap<String, TaskBudget>>,
    limits: TaskLimits,
    backend: Arc<dyn PlatformBackend>,
    effect_guard: Arc<dyn EffectGuard>,
    actor: Arc<dyn VisionActor>,
    /// Present whenever agent decision mode is possible (task-level --actor
    /// agent). The process-level default is `default_actor`.
    agent_actor: Arc<lcu_model::AgentActor>,
    /// Process-level default decision maker when a task does not override.
    default_actor: DecisionActor,
    scheduler: TaskScheduler,
    /// Task → pending gate. Only the confirmation request is retained: the
    /// executable Action is never stored for replay (realignment §3.4).
    pending: Mutex<HashMap<String, PendingGate>>,
    /// Non-executable strict target reservations, keyed by task id. A task may
    /// wait on its Actor without blocking execution on other targets.
    current_target: Mutex<HashMap<String, AppTarget>>,
    /// Tasks parked because another task already reserved the same PID/window.
    target_waiters: Mutex<Vec<(TaskId, u32, u64)>>,
    /// Capacity-one backend surfaces (currently the connected Chrome extension).
    surface_owners: Mutex<HashMap<String, TaskId>>,
    surface_waiters: Mutex<HashMap<String, Vec<TaskId>>>,
    /// GUI-approved foreground session: task + exact target. Single slot (serial
    /// FIFO). Starts only after a ForegroundGrant; cleared on terminal / pause /
    /// target change / release / runtime recovery. Never restored to a previous app.
    foreground: Mutex<Option<(TaskId, AppTarget)>>,
    /// Task-scoped foreground authorization retained while the native session
    /// is suspended for an external Agent decision.
    foreground_authorized: Mutex<HashMap<String, AppTarget>>,
    control_epoch: AtomicU64,
}

/// Approved grant waiting to be consumed exactly once. App access is recorded
/// as a permission at GUI decision time and never sits in this map.
#[derive(Debug, Clone)]
pub enum Grant {
    Foreground(ForegroundGrant),
    Consequence(ConsequenceGrant),
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
        let (actor, agent_actor, default_actor) = default_product_actor();
        let control_epoch = backend.control_epoch().unwrap_or(0);
        // Load persistent always_allow permissions into the in-memory map.
        let permissions = store
            .list_app_permissions()
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.app_key.clone(), p))
            .collect();
        let runtime = Self {
            paths,
            entry,
            store: Mutex::new(store),
            gates: Mutex::new(HashMap::new()),
            grants: Mutex::new(HashMap::new()),
            app_permissions: Mutex::new(permissions),
            allow_once: Mutex::new(std::collections::HashSet::new()),
            budgets: Mutex::new(HashMap::new()),
            limits: TaskLimits::default(),
            backend,
            effect_guard: Arc::new(StaticEffectGuard),
            actor,
            scheduler: TaskScheduler::default(),
            pending: Mutex::new(HashMap::new()),
            current_target: Mutex::new(HashMap::new()),
            target_waiters: Mutex::new(Vec::new()),
            surface_owners: Mutex::new(HashMap::new()),
            surface_waiters: Mutex::new(HashMap::new()),
            foreground: Mutex::new(None),
            foreground_authorized: Mutex::new(HashMap::new()),
            control_epoch: AtomicU64::new(control_epoch),
            agent_actor,
            default_actor,
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
            gates: Mutex::new(HashMap::new()),
            grants: Mutex::new(HashMap::new()),
            app_permissions: Mutex::new(HashMap::new()),
            allow_once: Mutex::new(std::collections::HashSet::new()),
            budgets: Mutex::new(HashMap::new()),
            limits: TaskLimits::default(),
            backend,
            effect_guard: Arc::new(StaticEffectGuard),
            actor: Arc::new(lcu_model::FakeActor),
            scheduler: TaskScheduler::default(),
            pending: Mutex::new(HashMap::new()),
            current_target: Mutex::new(HashMap::new()),
            target_waiters: Mutex::new(Vec::new()),
            surface_owners: Mutex::new(HashMap::new()),
            surface_waiters: Mutex::new(HashMap::new()),
            foreground: Mutex::new(None),
            foreground_authorized: Mutex::new(HashMap::new()),
            control_epoch: AtomicU64::new(0),
            agent_actor: Arc::new(lcu_model::AgentActor::new()),
            default_actor: DecisionActor::Vlm,
        })
    }

    /// Replace the agent decision actor (tests; also lets a task-level
    /// `--actor agent` override the process default).
    pub fn set_agent_actor(&mut self, actor: Arc<lcu_model::AgentActor>) {
        self.agent_actor = actor;
    }

    pub fn backend(&self) -> &dyn PlatformBackend {
        self.backend.as_ref()
    }

    pub(crate) fn refresh_control_epoch(&self, task_id: &TaskId) -> LcuResult<bool> {
        let current = self.backend.control_epoch()?;
        if current == 0 {
            return Ok(false);
        }
        let previous = self.control_epoch.swap(current, Ordering::AcqRel);
        if previous == 0 || previous == current {
            return Ok(false);
        }
        let affected = self.invalidate_session_state("system session changed; temporary control state invalidated");
        Ok(affected.contains(&task_id.0))
    }

    fn invalidate_session_state(&self, reason: &str) -> HashSet<String> {
        let mut affected = self
            .current_target
            .lock()
            .expect("current_target")
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        affected.extend(self.pending.lock().expect("pending").keys().cloned());
        affected.extend(
            self.allow_once
                .lock()
                .expect("allow_once")
                .iter()
                .map(|(task_id, _)| task_id.clone()),
        );
        affected.extend(
            self.foreground_authorized
                .lock()
                .expect("foreground_authorized")
                .keys()
                .cloned(),
        );
        affected.extend(
            self.surface_owners
                .lock()
                .expect("surface_owners")
                .values()
                .map(|task_id| task_id.0.clone()),
        );
        affected.extend(
            self.grants
                .lock()
                .expect("grants")
                .values()
                .map(|grant| match grant {
                    Grant::Foreground(grant) => grant.task_id.0.clone(),
                    Grant::Consequence(grant) => grant.task_id.0.clone(),
                }),
        );

        for id in &affected {
            let task_id = TaskId(id.clone());
            let Ok(task) = self.get_task(&task_id) else { continue };
            if !task.state.is_terminal() && task.state != TaskState::PausedByUser {
                let _ = self.apply_command(&task_id, TaskCommand::PauseByUser, reason);
            } else {
                self.release_current_target(&task_id);
                self.clear_task_pending(&task_id);
            }
        }
        affected
    }

    /// Override the product vision actor (tests).
    pub fn set_actor(&mut self, actor: Arc<dyn VisionActor>) {
        self.actor = actor;
    }

    /// Preload VLM weights (desktop background).
    pub fn warm_vision_actor(&self) -> LcuResult<()> {
        self.actor.warm_up()
    }

    /// True when the local VLM is the process default decision maker.
    pub fn is_vlm_default(&self) -> bool {
        self.default_actor == DecisionActor::Vlm
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

    pub(crate) fn release_current_target(&self, task_id: &TaskId) {
        let target = self
            .current_target
            .lock()
            .expect("current_target")
            .remove(&task_id.0);
        self.target_waiters
            .lock()
            .expect("target_waiters")
            .retain(|(waiting, _, _)| waiting != task_id);
        if let Some(t) = target {
            let _ = self.backend.release(&t);
            let ready = {
                let mut waiters = self.target_waiters.lock().expect("target_waiters");
                let mut ready = Vec::new();
                waiters.retain(|(waiting, pid, window_id)| {
                    if *pid == t.pid && *window_id == t.window_id {
                        ready.push(waiting.clone());
                        false
                    } else {
                        true
                    }
                });
                ready
            };
            for waiting in ready {
                let _ = self.scheduler.enqueue(waiting);
            }
        }
        self.close_foreground_session_for(task_id);
        self.release_serial_surface(task_id);
    }

    pub(crate) fn reserve_serial_surface(
        &self,
        task_id: &TaskId,
        selector: &AppSelector,
    ) -> bool {
        let Some(key) = self.backend.serial_surface_key(selector) else {
            return true;
        };
        self.reserve_serial_surface_key(task_id, key)
    }

    fn reserve_serial_surface_key(&self, task_id: &TaskId, key: String) -> bool {
        let mut owners = self.surface_owners.lock().expect("surface_owners");
        match owners.get(&key) {
            Some(owner) if owner != task_id => {
                drop(owners);
                let mut waiters = self.surface_waiters.lock().expect("surface_waiters");
                let queue = waiters.entry(key).or_default();
                if !queue.contains(task_id) {
                    queue.push(task_id.clone());
                }
                false
            }
            _ => {
                owners.insert(key, task_id.clone());
                true
            }
        }
    }

    fn release_serial_surface(&self, task_id: &TaskId) {
        {
            let mut waiters = self.surface_waiters.lock().expect("surface_waiters");
            waiters.retain(|_, queue| {
                queue.retain(|waiting| waiting != task_id);
                !queue.is_empty()
            });
        }
        let keys = {
            let mut owners = self.surface_owners.lock().expect("surface_owners");
            let keys = owners
                .iter()
                .filter(|(_, owner)| *owner == task_id)
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            for key in &keys {
                owners.remove(key);
            }
            keys
        };
        let ready = {
            let mut waiters = self.surface_waiters.lock().expect("surface_waiters");
            let mut ready = Vec::new();
            for key in keys {
                let mut empty = false;
                if let Some(queue) = waiters.get_mut(&key) {
                    if !queue.is_empty() {
                        ready.push((key.clone(), queue.remove(0)));
                    }
                    empty = queue.is_empty();
                }
                if empty {
                    waiters.remove(&key);
                }
            }
            ready
        };
        {
            let mut owners = self.surface_owners.lock().expect("surface_owners");
            for (key, task) in &ready {
                owners.insert(key.clone(), task.clone());
            }
        }
        for (_, task) in ready {
            let _ = self.scheduler.enqueue(task);
        }
    }

    pub(crate) fn current_target_for(&self, task_id: &TaskId) -> Option<AppTarget> {
        self.current_target
            .lock()
            .expect("current_target")
            .get(&task_id.0)
            .cloned()
    }

    /// Reserve one strict PID/window for this task. A conflicting task parks
    /// until the owner releases; other targets remain runnable.
    pub(crate) fn set_current_target(
        &self,
        task_id: &TaskId,
        target: AppTarget,
    ) -> LcuResult<bool> {
        let mut reservations = self.current_target.lock().expect("current_target");
        if reservations.iter().any(|(owner, reserved)| {
            owner != &task_id.0
                && reserved.pid == target.pid
                && reserved.window_id == target.window_id
        }) {
            drop(reservations);
            let mut waiters = self.target_waiters.lock().expect("target_waiters");
            if !waiters.iter().any(|(waiting, _, _)| waiting == task_id) {
                waiters.push((task_id.clone(), target.pid, target.window_id));
            }
            return Ok(false);
        }
        if reservations.get(&task_id.0).is_some_and(|reserved| {
            reserved.pid == target.pid && reserved.window_id == target.window_id
        }) {
            return Ok(true);
        }
        self.backend.set_takeover_watch(&target, true)?;
        let previous = reservations.insert(task_id.0.clone(), target.clone());
        drop(reservations);
        if let Some(old) = previous {
            if old.pid != target.pid || old.window_id != target.window_id {
                let _ = self.backend.release(&old);
                self.close_foreground_session_for(task_id);
            }
        }
        Ok(true)
    }

    /// Whether the GUI-approved foreground session matches this task + exact target.
    pub(crate) fn foreground_is_active(&self, task_id: &TaskId, target: &AppTarget) -> bool {
        self.foreground
            .lock()
            .expect("foreground")
            .as_ref()
            .map(|(owner, t)| owner == task_id && t.pid == target.pid && t.window_id == target.window_id)
            .unwrap_or(false)
    }

    pub(crate) fn foreground_is_authorized(&self, task_id: &TaskId, target: &AppTarget) -> bool {
        self.foreground_authorized
            .lock()
            .expect("foreground_authorized")
            .get(&task_id.0)
            .is_some_and(|authorized| {
                authorized.pid == target.pid && authorized.window_id == target.window_id
            })
    }

    /// Open the approved foreground session: mark the slot and tell the backend
    /// to activate the exact target (activation happens only here).
    pub(crate) fn open_foreground_session(
        &self,
        task_id: &TaskId,
        target: &AppTarget,
    ) -> LcuResult<()> {
        self.backend.set_agent_session(target, true)?;
        self.foreground_authorized
            .lock()
            .expect("foreground_authorized")
            .insert(task_id.0.clone(), target.clone());
        *self.foreground.lock().expect("foreground") = Some((task_id.clone(), target.clone()));
        Ok(())
    }

    pub(crate) fn resume_foreground_session(
        &self,
        task_id: &TaskId,
        target: &AppTarget,
    ) -> LcuResult<()> {
        if !self.foreground_is_authorized(task_id, target) {
            return Err(LcuError::coded(
                ErrorCode::ForegroundRequired,
                "foreground authorization missing",
            ));
        }
        match self.backend.resume_agent_session(target) {
            Ok(()) => {}
            Err(e) if e.code() == ErrorCode::ForegroundRequired => {
                self.backend.set_agent_session(target, true)?;
            }
            Err(e) => return Err(e),
        }
        *self.foreground.lock().expect("foreground") = Some((task_id.clone(), target.clone()));
        Ok(())
    }

    pub(crate) fn suspend_foreground_session_for(&self, task_id: &TaskId) {
        let slot = {
            let mut foreground = self.foreground.lock().expect("foreground");
            match foreground.as_ref() {
                Some((owner, _)) if owner == task_id => foreground.take(),
                _ => None,
            }
        };
        if let Some((_, target)) = slot {
            let _ = self.backend.suspend_agent_session(&target);
        }
    }

    /// Clear the single foreground session slot (terminal / pause / release /
    /// target change). Never restores the previous app.
    pub(crate) fn close_foreground_session_for(&self, task_id: &TaskId) {
        let slot = {
            let mut foreground = self.foreground.lock().expect("foreground");
            match foreground.as_ref() {
                Some((owner, _)) if owner == task_id => foreground.take(),
                _ => None,
            }
        };
        let authorized = self
            .foreground_authorized
            .lock()
            .expect("foreground_authorized")
            .remove(&task_id.0);
        if let Some(target) = authorized.or_else(|| slot.map(|(_, target)| target)) {
            let _ = self.backend.set_agent_session(&target, false);
        }
    }

    /// Terminal/pause cleanup for one task: drop the pending gate, invalidate
    /// unconsumed grants, and release task-scoped allow_once permissions.
    /// Persistent always_allow permissions are untouched.
    pub(crate) fn clear_task_pending(&self, task_id: &TaskId) {
        self.clear_task_gate(task_id);
        let mut allow_once = self.allow_once.lock().expect("allow_once lock");
        allow_once.retain(|(tid, _)| tid != &task_id.0);
    }

    pub(crate) fn clear_task_gate(&self, task_id: &TaskId) {
        self.pending
            .lock()
            .expect("pending lock")
            .remove(&task_id.0);
        {
            let mut gates = self.gates.lock().expect("gates lock");
            gates.retain(|_, request| request.task_id != *task_id);
        }
        {
            let mut grants = self.grants.lock().expect("grants lock");
            grants.retain(|_, grant| match grant {
                Grant::Foreground(g) => g.task_id != *task_id,
                Grant::Consequence(g) => g.task_id != *task_id,
            });
        }
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
        self.submit_task_with_limits(goal, caller, app_selector, None, None, None)
    }

    /// Submit with optional per-task step budget (CLI `--max-steps`), decision
    /// maker (`actor`: `vlm` | `agent`; None follows the Runtime) and control
    /// mode (`auto` | `background_only` | `foreground`).
    pub fn submit_task_with_limits(
        &self,
        goal: impl Into<String>,
        caller: CallerIdentity,
        app_selector: Option<AppSelector>,
        max_steps: Option<u32>,
        actor: Option<String>,
        control_mode: Option<String>,
    ) -> LcuResult<TaskRecord> {
        self.submit_task_full(
            goal,
            caller,
            app_selector,
            max_steps,
            actor,
            control_mode,
        )
    }

    pub(crate) fn submit_task_full(
        &self,
        goal: impl Into<String>,
        caller: CallerIdentity,
        app_selector: Option<AppSelector>,
        max_steps: Option<u32>,
        actor: Option<String>,
        control_mode: Option<String>,
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
        let mut record = TaskRecord::new(goal, caller, app_selector);
        if let Some(actor) = actor.as_deref() {
            match actor {
                "vlm" | "qwen" | "agent" => record.actor = Some(actor.to_string()),
                other => {
                    return Err(LcuError::coded(
                        ErrorCode::InvalidRequest,
                        format!("unknown --actor {other:?}; use vlm or agent"),
                    ));
                }
            }
        }
        if let Some(mode) = control_mode.as_deref() {
            record.control_mode = match mode {
                "auto" => ControlMode::Auto,
                "background_only" => ControlMode::BackgroundOnly,
                "foreground" => ControlMode::Foreground,
                other => {
                    return Err(LcuError::coded(
                        ErrorCode::InvalidRequest,
                        format!(
                            "unknown --control-mode {other:?}; use auto, background_only or foreground"
                        ),
                    ));
                }
            };
        }
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

    /// Pending GUI gate requests (desktop process only — not via socket).
    /// Carries no executable Action: gates only hold the confirmation request.
    pub fn list_pending_gates(&self) -> Vec<GateUiLaunch> {
        let tasks = self.list_tasks();
        let gates = self.gates.lock().expect("gates lock");
        let pending_map = self.pending.lock().expect("pending lock");
        gates
            .values()
            .filter(|r| r.status == GrantStatus::Pending)
            .filter(|r| pending_map.values().any(|p| p.grant_id == r.grant_id.0))
            .map(|r| {
                let task_id = r.task_id.0.clone();
                let goal = tasks
                    .iter()
                    .find(|t| t.task_id == r.task_id)
                    .map(|t| t.goal.clone())
                    .unwrap_or_default();
                let takeover_started = pending_map
                    .values()
                    .find(|p| p.grant_id == r.grant_id.0)
                    .map(|p| p.takeover_started)
                    .unwrap_or(false);
                GateUiLaunch {
                    grant_id: r.grant_id.0.clone(),
                    kind: r.kind,
                    gui_only: true,
                    message: if goal.is_empty() {
                        r.reason.clone()
                    } else {
                        format!("goal: {goal}")
                    },
                    binding_hash: r.binding_hash.clone(),
                    task_id,
                    target_app: r.app_key.clone(),
                    summary: r.summary.clone(),
                    impact: format!("{} [{}]", r.reason, r.risk_note),
                    risk_note: r.risk_note.clone(),
                    requires_takeover: r.requires_takeover,
                    takeover_started,
                }
            })
            .collect()
    }

    /// Desktop GUI: approve a pending gate (Foreground / Consequence only —
    /// R4 uses takeover, AppAccess uses `app_access_in_gui`).
    pub fn approve_pending_in_gui(&self, grant_id: &str) -> LcuResult<()> {
        let (kind, has_pending) = {
            let pending = self.pending.lock().expect("pending lock");
            let entry = pending.values().find(|p| p.grant_id == grant_id);
            let kind = entry.map(|p| p.kind);
            (kind, entry.is_some())
        };
        if !has_pending {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "no pending gate bound to this grant; stale or already handled",
            ));
        }
        match kind {
            Some(GateKind::Takeover) => {
                return Err(LcuError::coded(
                    ErrorCode::PermissionDenied,
                    "R4 requires begin_takeover then complete_takeover; not plain approve",
                ));
            }
            Some(GateKind::AppAccess) => {
                return Err(LcuError::coded(
                    ErrorCode::PermissionDenied,
                    "app access requires an explicit allow_once / always_allow / deny decision",
                ));
            }
            _ => {}
        }
        let binding = self.gate_binding(grant_id)?;
        self.approve_in_gui(grant_id, &binding)
    }

    /// Desktop GUI: record the app-access decision for a pending AppAccess gate.
    /// AlwaysAllow persists; AllowOnce is task-scoped; Deny fails the task.
    pub fn app_access_in_gui(&self, grant_id: &str, decision: AppAccessDecision) -> LcuResult<()> {
        let (task_id, app_key) = {
            let pending = self.pending.lock().expect("pending lock");
            let entry = pending.values().find(|p| p.grant_id == grant_id).ok_or_else(
                || {
                    LcuError::coded(
                        ErrorCode::ApprovalInvalid,
                        "no pending gate bound to this grant",
                    )
                },
            )?;
            if entry.kind != GateKind::AppAccess {
                return Err(LcuError::coded(
                    ErrorCode::PermissionDenied,
                    "app_access_in_gui is only for app-access gates",
                ));
            }
            (entry.task_id.clone(), entry.app_key.clone())
        };
        if decision == AppAccessDecision::Deny {
            return self.deny_pending_in_gui(grant_id);
        }
        let binding = self.gate_binding(grant_id)?;
        self.approve_in_gui(grant_id, &binding)?;
        {
            let mut gates = self.gates.lock().expect("gates lock");
            if let Some(req) = gates.get_mut(grant_id) {
                req.app_decision = Some(decision);
            }
        }
        match decision {
            AppAccessDecision::AlwaysAllow => {
                let perm = AppPermission::new(app_key.clone(), decision);
                self.app_permissions
                    .lock()
                    .expect("app_permissions")
                    .insert(app_key.clone(), perm.clone());
                if let Ok(store) = self.store.lock() {
                    let _ = store.upsert_app_permission(&perm);
                }
            }
            AppAccessDecision::AllowOnce => {
                self.allow_once
                    .lock()
                    .expect("allow_once")
                    .insert((task_id.0.clone(), app_key));
            }
            AppAccessDecision::Deny => unreachable!("deny returned above"),
        }
        if let Ok(task) = self.get_task(&task_id) {
            if task.state == TaskState::WaitingActor {
                let _ = self.apply_command(
                    &task_id,
                    TaskCommand::Approve,
                    "app access decided in GUI",
                );
            }
        }
        if self.scheduler.is_started() {
            let _ = self.scheduler.enqueue(task_id);
        }
        Ok(())
    }

    pub fn list_app_permissions(&self) -> Vec<AppPermission> {
        self.app_permissions
            .lock()
            .expect("app_permissions")
            .values()
            .cloned()
            .collect()
    }

    pub fn revoke_app_permission(&self, app_key: &str) -> LcuResult<bool> {
        let removed = self
            .store
            .lock()
            .map_err(|_| LcuError::coded(ErrorCode::InternalError, "store lock poisoned"))?
            .delete_app_permission(app_key)?;
        self.app_permissions
            .lock()
            .expect("app_permissions")
            .remove(app_key);
        Ok(removed)
    }

    /// Desktop GUI: deny a pending gate.
    pub fn deny_pending_in_gui(&self, grant_id: &str) -> LcuResult<()> {
        let task_id = {
            let mut gates = self.gates.lock().expect("gates lock");
            let request = gates.get_mut(grant_id).ok_or_else(|| {
                LcuError::coded(ErrorCode::ApprovalInvalid, "unknown grant id")
            })?;
            request.deny_in_gui()?;
            request.task_id.clone()
        };
        {
            let mut pending = self.pending.lock().expect("pending lock");
            pending.retain(|_, p| p.grant_id != grant_id);
        }
        if task_id.0 != "pending" {
            let _ = self.fail_task(
                &task_id,
                format!("gate {grant_id} denied by user in GUI"),
            );
        }
        Ok(())
    }

    /// Desktop GUI: user begins R4 human takeover.
    pub fn begin_takeover_in_gui(&self, grant_id: &str) -> LcuResult<()> {
        let mut pending = self.pending.lock().expect("pending lock");
        let entry = pending
            .values_mut()
            .find(|p| p.grant_id == grant_id)
            .ok_or_else(|| {
                LcuError::coded(ErrorCode::ApprovalInvalid, "unknown pending takeover")
            })?;
        if entry.kind != GateKind::Takeover {
            return Err(LcuError::coded(
                ErrorCode::PermissionDenied,
                "begin_takeover is only for R4 takeover gates",
            ));
        }
        entry.takeover_started = true;
        Ok(())
    }

    /// Desktop GUI: mark R4 takeover complete (Runtime never executes the R4 action).
    pub fn complete_takeover_in_gui(&self, grant_id: &str) -> LcuResult<()> {
        {
            let pending = self.pending.lock().expect("pending lock");
            let entry = pending
                .values()
                .find(|p| p.grant_id == grant_id)
                .ok_or_else(|| {
                    LcuError::coded(ErrorCode::ApprovalInvalid, "unknown pending takeover")
                })?;
            if entry.kind != GateKind::Takeover {
                return Err(LcuError::coded(
                    ErrorCode::PermissionDenied,
                    "complete_takeover is only for R4 takeover gates",
                ));
            }
            if !entry.takeover_started {
                return Err(LcuError::coded(
                    ErrorCode::ApprovalInvalid,
                    "R4 takeover not started; call begin_takeover first",
                ));
            }
        }
        let binding = self.gate_binding(grant_id)?;
        self.approve_in_gui(grant_id, &binding)
    }

    fn load_task(&self, task_id: &TaskId) -> LcuResult<TaskRecord> {
        let store = self.store.lock().expect("store lock");
        store
            .get_task(task_id)?
            .ok_or_else(|| LcuError::coded(ErrorCode::TaskNotFound, "task not found"))
    }

    pub fn get_task(&self, task_id: &TaskId) -> LcuResult<TaskRecord> {
        let record = self.load_task(task_id)?;
        if record.state == TaskState::WaitingActor && self.expire_waiting_gate(task_id) {
            self.load_task(task_id)
        } else {
            Ok(record)
        }
    }

    pub fn list_tasks(&self) -> Vec<TaskRecord> {
        let mut tasks = self
            .store
            .lock()
            .expect("store lock")
            .list_tasks()
            .unwrap_or_default();
        for task in &mut tasks {
            if task.state == TaskState::WaitingActor && self.expire_waiting_gate(&task.task_id) {
                if let Ok(updated) = self.load_task(&task.task_id) {
                    *task = updated;
                }
            }
        }
        tasks
    }

    /// Gate expiry is reconciled on the existing status/list read path. An
    /// unconsumed approved grant that outlived its TTL is invalidated too.
    fn expire_waiting_gate(&self, task_id: &TaskId) -> bool {
        let grant_id = self
            .pending
            .lock()
            .expect("pending lock")
            .get(&task_id.0)
            .map(|pending| pending.grant_id.clone());
        let Some(grant_id) = grant_id else {
            return false;
        };
        let expired = {
            let mut gates = self.gates.lock().expect("gates lock");
            let Some(request) = gates.get_mut(&grant_id) else {
                return false;
            };
            if request.status == GrantStatus::Pending && request.is_expired(Utc::now()) {
                request.status = GrantStatus::Expired;
            }
            request.status == GrantStatus::Expired
        };
        if !expired {
            return false;
        }

        let error = format!("gate {grant_id} expired");
        if self.fail_task(task_id, error).is_err() {
            return false;
        }
        true
    }

    /// After a crash / kill, tasks left in Queued/Running/WaitingActor have
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
        // Reset any foreground session the dead process left active in the
        // native service (single slot; pid 0 with active=false clears it).
        let _ = self.backend.set_agent_session(
            &AppTarget {
                app_id: String::new(),
                pid: 0,
                window_id: 0,
                window_title: String::new(),
            },
            false,
        );
        // Gates/grants from the dead process are gone with it: invalidate and
        // drop all pending gate requests and unconsumed grants. Persistent
        // always_allow app permissions survive.
        self.gates.lock().expect("gates lock").clear();
        self.grants.lock().expect("grants lock").clear();
        self.pending.lock().expect("pending lock").clear();
        self.allow_once.lock().expect("allow_once lock").clear();
        tracing::info!(recovered, "stale tasks recovered to paused");
    }

    pub(crate) fn apply_command(
        &self,
        task_id: &TaskId,
        command: TaskCommand,
        message: impl Into<String>,
    ) -> LcuResult<TaskRecord> {
        let mut record = self.load_task(task_id)?;
        TaskStateMachine::apply(&mut record, command)?;
        let event = TaskEvent {
            task_id: record.task_id.clone(),
            state: record.state,
            at: Utc::now(),
            message: message.into(),
            step: Some(record.step_count),
        };
        if record.state.is_terminal() {
            self.clear_task_pending(task_id);
            self.release_current_target(task_id);
            self.budgets
                .lock()
                .expect("budgets")
                .remove(&record.task_id.0);
        } else if record.state == TaskState::WaitingActor {
            // waiting_actor releases the native foreground session but keeps
            // the non-executable target reservation (realignment §4.6).
            self.suspend_foreground_session_for(task_id);
        } else if record.state == TaskState::PausedByUser {
            // Pause (user pause / request_user / takeover) ends the foreground
            // session, invalidates unconsumed grants and releases the target
            // reservation; the user owns the window until an explicit resume.
            self.clear_task_pending(task_id);
            self.close_foreground_session_for(task_id);
            self.release_current_target(task_id);
        }
        self.persist_task(&record, Some(&event));
        // A parked external-Agent decision owns only a temp screenshot and
        // target reservation; terminal/pause removes both immediately.
        if record.state.is_terminal() || record.state == TaskState::PausedByUser {
            if let Some(obs_id) = &record.last_observation_id {
                self.agent_actor.discard(&obs_id.0);
            }
        }
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

    /// Create and store a pending GateRequest; return its grant id.
    fn insert_gate(&self, request: GateRequest) -> String {
        let grant_id = request.grant_id.0.clone();
        self.gates
            .lock()
            .expect("gates lock")
            .insert(grant_id.clone(), request);
        grant_id
    }

    /// Insert a pending consequence gate (R3 confirm or R4 takeover) for a
    /// proposal. The executable Action is never stored — only the runtime
    /// consequence identity / exact-match screenshot evidence.
    pub(crate) fn insert_consequence_gate(
        &self,
        task_id: &TaskId,
        app_key: &str,
        observation: &AppObservation,
        action: &Action,
        effect: &EffectClaim,
        judgement: &lcu_core::effect_guard::EffectJudgement,
    ) -> String {
        let evidence = screenshot_evidence_for(observation, action, effect);
        let identity = consequence_identity_for(observation, action);
        let summary = describe_consequence_for_ui(observation, action, effect);
        let is_takeover = judgement.risk.requires_user_takeover();
        let request = GateRequest::new(
            if is_takeover {
                GateKind::Takeover
            } else {
                GateKind::Consequence
            },
            task_id.clone(),
            app_key,
            format!(
                "{} requires {}: {}",
                if is_takeover { "human takeover" } else { "confirmation" },
                if is_takeover { "R4" } else { "R3" },
                judgement.rationale
            ),
            summary.clone(),
            format!("{:?}", judgement.risk),
            is_takeover,
            Duration::minutes(5),
        );
        let grant_id = self.insert_gate(request);
        let grant = ConsequenceGrant::new(
            task_id.clone(),
            app_key,
            effect,
            identity,
            summary,
            evidence,
            Duration::minutes(5),
        );
        // Keep the pending grant's status in sync with its gate request.
        let mut grant = grant;
        grant.grant_id = GrantId(grant_id.clone());
        if is_takeover {
            // Takeover gates are decided by begin/complete flow; no consumable grant.
            return grant_id;
        }
        self.grants
            .lock()
            .expect("grants lock")
            .insert(grant_id.clone(), Grant::Consequence(grant));
        grant_id
    }

    /// Insert a pending foreground gate (task-scoped, one-time, never persists).
    pub(crate) fn insert_foreground_gate(
        &self,
        task_id: &TaskId,
        app_key: &str,
        target: &AppTarget,
        reason: String,
    ) -> String {
        let request = GateRequest::new(
            GateKind::Foreground,
            task_id.clone(),
            app_key,
            reason,
            format!(
                "foreground activation: bring {} window {} to the front once",
                target.app_id, target.window_id
            ),
            "foreground",
            false,
            Duration::minutes(5),
        );
        let grant_id = self.insert_gate(request);
        let mut grant = ForegroundGrant::new(task_id.clone(), app_key, Duration::minutes(5));
        grant.grant_id = GrantId(grant_id.clone());
        self.grants
            .lock()
            .expect("grants lock")
            .insert(grant_id.clone(), Grant::Foreground(grant));
        grant_id
    }

    /// Insert a pending app-access gate (first control of a stable app identity).
    pub(crate) fn insert_app_access_gate(
        &self,
        task_id: &TaskId,
        app_key: &str,
        target: &AppTarget,
    ) -> String {
        let request = GateRequest::new(
            GateKind::AppAccess,
            task_id.clone(),
            app_key,
            "first control of this app requires an access decision",
            format!(
                "app access: {} (allow once / always allow / deny)",
                target.app_id
            ),
            "app_access",
            false,
            Duration::minutes(30),
        );
        self.insert_gate(request)
    }

    /// Resolve the app-access state for a task + stable app identity.
    pub(crate) fn app_access_state(&self, task_id: &TaskId, app_key: &str) -> AppAccessOutcome {
        if let Some(perm) = self.app_permissions.lock().expect("app_permissions").get(app_key) {
            return match perm.decision {
                AppAccessDecision::AlwaysAllow => AppAccessOutcome::Allowed,
                AppAccessDecision::AllowOnce => {
                    // allow_once is task-scoped by construction; a persisted
                    // AllowOnce row cannot exist (never upserted).
                    AppAccessOutcome::Allowed
                }
                AppAccessDecision::Deny => AppAccessOutcome::Denied,
            };
        }
        if self
            .allow_once
            .lock()
            .expect("allow_once")
            .contains(&(task_id.0.clone(), app_key.to_string()))
        {
            return AppAccessOutcome::Allowed;
        }
        AppAccessOutcome::RequestDecision
    }

    /// Validate an action against observation + EffectGuard. Does not execute OS effects.
    pub fn evaluate_action(
        &self,
        observation: &AppObservation,
        action: &Action,
        effect: Option<&EffectClaim>,
        task_authorized_max_risk: RiskLevel,
    ) -> LcuResult<EvaluatedAction> {
        self.evaluate_action_inner(
            None,
            observation,
            action,
            effect,
            task_authorized_max_risk,
            None,
        )
    }

    /// Task-bound evaluation: R3/R4 park a human gate; agents cannot self-approve.
    pub fn evaluate_action_for_task(
        &self,
        task_id: Option<&TaskId>,
        observation: &AppObservation,
        action: &Action,
        effect: Option<&EffectClaim>,
        task_authorized_max_risk: RiskLevel,
        caller: Option<&CallerIdentity>,
    ) -> LcuResult<EvaluatedAction> {
        self.evaluate_action_inner(
            task_id,
            observation,
            action,
            effect,
            task_authorized_max_risk,
            caller,
        )
    }

    fn evaluate_action_inner(
        &self,
        task_id: Option<&TaskId>,
        observation: &AppObservation,
        action: &Action,
        effect: Option<&EffectClaim>,
        task_authorized_max_risk: RiskLevel,
        caller: Option<&CallerIdentity>,
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
            effect,
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

        let _ = (task_id, caller);
        Ok(EvaluatedAction {
            risk: judgement.risk,
            requires_takeover: judgement.risk.requires_user_takeover(),
            unknown: judgement.unknown,
            rationale: judgement.rationale,
        })
    }

    pub fn gate_binding(&self, grant_id: &str) -> LcuResult<GateRequest> {
        let gates = self.gates.lock().expect("gates lock");
        gates
            .get(grant_id)
            .cloned()
            .ok_or_else(|| LcuError::coded(ErrorCode::ApprovalInvalid, "unknown grant id"))
    }

    /// `lcu approve` must only open the GUI; it never marks a gate complete.
    pub fn request_gate_ui(&self, grant_id: &str) -> LcuResult<GateUiLaunch> {
        let gates = self.gates.lock().expect("gates lock");
        let request = gates
            .get(grant_id)
            .ok_or_else(|| LcuError::coded(ErrorCode::ApprovalInvalid, "unknown grant id"))?;
        if request.status != GrantStatus::Pending {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                format!("gate not pending: {:?}", request.status),
            ));
        }
        let takeover_started = {
            let pending = self.pending.lock().expect("pending lock");
            pending
                .values()
                .find(|p| p.grant_id == grant_id)
                .map(|p| p.takeover_started)
                .unwrap_or(false)
        };
        Ok(GateUiLaunch {
            grant_id: grant_id.to_string(),
            kind: request.kind,
            gui_only: true,
            message: "open desktop confirmation UI; CLI cannot complete a gate".into(),
            binding_hash: request.binding_hash.clone(),
            task_id: request.task_id.0.clone(),
            target_app: request.app_key.clone(),
            summary: request.summary.clone(),
            impact: request.reason.clone(),
            risk_note: request.risk_note.clone(),
            requires_takeover: request.requires_takeover,
            takeover_started,
        })
    }

    /// Direct CLI approval is forbidden by contract.
    pub fn approve_from_cli(&self, _grant_id: &str) -> LcuResult<()> {
        Err(LcuError::coded(
            ErrorCode::PermissionDenied,
            "lcu approve cannot complete a gate in CLI; GUI presence required",
        ))
    }

    /// Agents must never approve their own high-risk actions.
    pub fn approve_from_agent(&self, _grant_id: &str) -> LcuResult<()> {
        Err(LcuError::coded(
            ErrorCode::PermissionDenied,
            "agent cannot approve own R3/R4 actions; GUI user presence required",
        ))
    }

    /// GUI-only finalization path (desktop shell).
    pub fn approve_in_gui(&self, grant_id: &str, expected: &GateRequest) -> LcuResult<()> {
        let (task_id, kind) = {
            let mut gates = self.gates.lock().expect("gates lock");
            let request = gates.get_mut(grant_id).ok_or_else(|| {
                LcuError::coded(ErrorCode::ApprovalInvalid, "unknown grant id")
            })?;
            request.approve_in_gui(expected, Utc::now())?;
            (request.task_id.clone(), request.kind)
        };
        if matches!(kind, GateKind::Foreground | GateKind::Consequence) {
            let mut grants = self.grants.lock().expect("grants lock");
            let grant = grants.get_mut(grant_id).ok_or_else(|| {
                LcuError::coded(ErrorCode::ApprovalInvalid, "gate has no matching grant")
            })?;
            match grant {
                Grant::Foreground(grant) => grant.status = GrantStatus::Approved,
                Grant::Consequence(grant) => grant.status = GrantStatus::Approved,
            }
        }
        if task_id.0 != "pending" && kind != GateKind::AppAccess {
            if let Ok(task) = self.get_task(&task_id) {
                if task.state == TaskState::WaitingActor {
                    let _ = self.apply_command(&task_id, TaskCommand::Approve, "gate decided in GUI");
                }
            }
            if self.scheduler.is_started() {
                let _ = self.scheduler.enqueue(task_id);
            }
        }
        Ok(())
    }

    /// Approved, unconsumed consequence grant for a task (if any). The worker
    /// matches it against the new proposal after the gate; it is never consumed
    /// by a stored Action (there is none).
    pub(crate) fn pending_consequence_grant(
        &self,
        task_id: &TaskId,
        app_key: &str,
    ) -> Option<ConsequenceGrant> {
        let grants = self.grants.lock().expect("grants lock");
        grants.values().find_map(|grant| match grant {
            Grant::Consequence(g)
                if g.task_id == *task_id
                    && g.app_key == app_key
                    && g.status == GrantStatus::Approved
                    && !g.is_expired(Utc::now()) =>
            {
                Some(g.clone())
            }
            _ => None,
        })
    }

    /// Consume a consequence grant exactly once (identity/evidence match passed).
    pub(crate) fn consume_consequence_grant(
        &self,
        grant_id: &str,
    ) -> LcuResult<ConsequenceGrant> {
        let mut grants = self.grants.lock().expect("grants lock");
        let grant = match grants.get_mut(grant_id) {
            Some(Grant::Consequence(g)) => g,
            _ => {
                return Err(LcuError::coded(
                    ErrorCode::ApprovalInvalid,
                    "unknown consequence grant",
                ))
            }
        };
        if grant.status != GrantStatus::Approved {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                format!("consequence grant not approved: {:?}", grant.status),
            ));
        }
        if grant.is_expired(Utc::now()) {
            grant.status = GrantStatus::Expired;
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "consequence grant expired before consume",
            ));
        }
        grant.status = GrantStatus::Consumed;
        Ok(grant.clone())
    }

    /// Invalidate an unconsumed consequence grant (different proposal, expiry,
    /// pause, cancel, restart). Consumed grants are never refunded.
    pub(crate) fn invalidate_consequence_grant(&self, grant_id: &str) {
        let mut grants = self.grants.lock().expect("grants lock");
        if let Some(Grant::Consequence(g)) = grants.get_mut(grant_id) {
            if g.status == GrantStatus::Approved {
                g.status = GrantStatus::Invalidated;
            }
        }
    }

    /// Consume a foreground grant once; Runtime then activates the exact target.
    pub(crate) fn consume_foreground_grant(&self, grant_id: &str) -> LcuResult<ForegroundGrant> {
        let mut grants = self.grants.lock().expect("grants lock");
        let grant = match grants.get_mut(grant_id) {
            Some(Grant::Foreground(g)) => g,
            _ => {
                return Err(LcuError::coded(
                    ErrorCode::ApprovalInvalid,
                    "unknown foreground grant",
                ))
            }
        };
        if grant.status != GrantStatus::Approved {
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                format!("foreground grant not approved: {:?}", grant.status),
            ));
        }
        if grant.is_expired(Utc::now()) {
            grant.status = GrantStatus::Expired;
            return Err(LcuError::coded(
                ErrorCode::ApprovalInvalid,
                "foreground grant expired before consume",
            ));
        }
        grant.status = GrantStatus::Consumed;
        Ok(grant.clone())
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
                "default_actor={}; vlm_implementation={} (LCU_VISION_ACTOR=auto|agent|vlm|qwen)",
                match self.default_actor {
                    DecisionActor::Agent => "agent",
                    DecisionActor::Vlm => "vlm",
                },
                self.actor.name()
            ),
            if self.scheduler.is_started() {
                "product worker: scheduler running (observe→guard→act)".into()
            } else {
                "product worker: scheduler not started (desktop must call start_scheduler)".into()
            },
            "gates: app access / consequence / foreground grants; desktop tray only; CLI never finalizes".into(),
            "queue: global serial FIFO; waiting_actor / paused release the execution slot".into(),
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
    pub risk: RiskLevel,
    /// R4: human must take over; Runtime must never execute even after GUI ack.
    #[serde(default)]
    pub requires_takeover: bool,
    /// Actor declared `unknown` (or missing effect): stop and ask the user.
    #[serde(default)]
    pub unknown: bool,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GateUiLaunch {
    pub grant_id: String,
    pub kind: GateKind,
    pub gui_only: bool,
    pub message: String,
    pub binding_hash: String,
    #[serde(default)]
    pub task_id: String,
    #[serde(default)]
    pub target_app: String,
    /// Consequence display summary (task, app, effect, object/destination).
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub impact: String,
    #[serde(default)]
    pub risk_note: String,
    #[serde(default)]
    pub requires_takeover: bool,
    #[serde(default)]
    pub takeover_started: bool,
}

/// Result of the app-access gate for a task + stable app identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAccessOutcome {
    Allowed,
    Denied,
    RequestDecision,
}

/// Runtime-extracted consequence identity for an action + observation
/// (realignment §3.3/§3.4). Every field that can change the user's judgment
/// participates in the identity hash.
pub(crate) fn consequence_identity_for(
    observation: &AppObservation,
    action: &Action,
) -> ConsequenceIdentity {
    let surface = observation
        .surface_scope
        .as_deref()
        .map(|scope| format!("surface_sha256:{}", digest(scope)));
    let (operation, object, destination, content) = match action {
        Action::Semantic(SemanticAction::Navigate { url }) => {
            let destination = format!(
                "{}|url_sha256:{}",
                surface.as_deref().unwrap_or("surface:none"),
                digest(url)
            );
            ("navigate", None, Some(destination), None)
        }
        Action::Semantic(SemanticAction::Invoke { element_id }) => {
            let object = observation
                .elements
                .iter()
                .find(|e| e.id == *element_id)
                .map(element_identity)
                .or_else(|| Some(element_id.clone()));
            ("invoke", object, surface.clone(), None)
        }
        Action::Semantic(SemanticAction::SetValue { element_id, value }) => {
            let object = observation
                .elements
                .iter()
                .find(|e| e.id == *element_id)
                .map(element_identity)
                .or_else(|| Some(element_id.clone()));
            ("set_value", object, surface.clone(), Some(digest(value)))
        }
        Action::Semantic(SemanticAction::Scroll { .. }) => ("scroll", None, surface.clone(), None),
        Action::Semantic(SemanticAction::Focus { element_id }) => {
            ("focus", Some(element_id.clone()), surface.clone(), None)
        }
        Action::Targeted(TargetedInput::Click { .. }) => ("click", None, surface.clone(), None),
        Action::Targeted(TargetedInput::TypeText { text, .. }) => {
            ("type", None, surface.clone(), Some(digest(text)))
        }
        Action::Targeted(TargetedInput::KeyCombo { keys }) => {
            ("keys", Some(keys.join("+")), surface, None)
        }
        _ => ("observe", None, None, None),
    };
    ConsequenceIdentity {
        operation: operation.into(),
        object,
        destination,
        content_digest: content,
        amount: None,
        account: None,
    }
}

/// Normalize an object label for identity comparison: truncate, trim.
fn normalize_object(s: String) -> String {
    let trimmed = s.trim();
    let mut out = String::with_capacity(trimmed.len().min(64));
    for ch in trimmed.chars().take(64) {
        out.push(ch.to_ascii_lowercase());
    }
    out
}

fn element_identity(element: &lcu_core::observation::ElementNode) -> String {
    normalize_object(format!(
        "{}@{:.4},{:.4},{:.4},{:.4}|{}",
        element.role,
        element.frame.x,
        element.frame.y,
        element.frame.width,
        element.frame.height,
        element.label.as_deref().unwrap_or("")
    ))
}

fn digest(value: &str) -> String {
    use sha2::Digest as _;
    let bytes = sha2::Sha256::digest(value.as_bytes());
    hex::encode(bytes)
}

/// Exact-match screenshot evidence for screenshot-only proposals. Runtime
/// never stores a replayable Action; the evidence only proves "picture
/// unchanged" plus the candidate identity for same-candidate detection.
pub(crate) fn screenshot_evidence_for(
    observation: &AppObservation,
    action: &Action,
    effect: &EffectClaim,
) -> Option<ScreenshotEvidence> {
    let (element_id, input_kind, x, y) = match action {
        Action::Semantic(SemanticAction::Invoke { element_id }) => {
            (Some(element_id.clone()), None, None, None)
        }
        Action::Targeted(TargetedInput::Click { x, y, .. }) => {
            (None, Some("click".into()), Some(*x), Some(*y))
        }
        Action::Targeted(TargetedInput::TypeText { x, y, .. }) => {
            (None, Some("type".into()), *x, *y)
        }
        Action::Targeted(TargetedInput::KeyCombo { .. }) => (None, Some("keys".into()), None, None),
        _ => return None,
    };
    Some(ScreenshotEvidence {
        observation_id: observation.observation_id.clone(),
        image_hash: observation.image_hash.clone(),
        action_hash: action.action_hash(),
        element_id,
        input_kind,
        x,
        y,
        effect_kind: effect.kind,
    })
}

/// Human-readable consequence summary for the desktop confirmation dialog and
/// audit. Display only — never participates in grant matching.
pub fn describe_consequence_for_ui(
    observation: &AppObservation,
    action: &Action,
    effect: &EffectClaim,
) -> String {
    let action_part = match action {
        Action::Observe => "observe".to_string(),
        Action::Wait { milliseconds } => format!("wait {milliseconds}ms"),
        Action::Done { summary } => format!("done: {summary}"),
        Action::Fail { reason } => format!("fail: {reason}"),
        Action::RequestUser { reason } => format!("request user: {reason}"),
        Action::Semantic(SemanticAction::Navigate { url }) => format!("navigate to `{url}`"),
        Action::Semantic(SemanticAction::Invoke { element_id }) => {
            let label = observation
                .elements
                .iter()
                .find(|e| e.id == *element_id)
                .and_then(|e| e.label.clone())
                .unwrap_or_else(|| element_id.clone());
            let preview: String = label.chars().take(60).collect();
            format!("invoke `{preview}`")
        }
        Action::Semantic(SemanticAction::SetValue { element_id, value }) => {
            let label = observation
                .elements
                .iter()
                .find(|e| e.id == *element_id)
                .and_then(|e| e.label.clone())
                .unwrap_or_else(|| element_id.clone());
            format!("set_value `{}` ({} chars)", label, value.chars().count())
        }
        Action::Semantic(SemanticAction::Focus { element_id }) => {
            format!("focus `{element_id}`")
        }
        Action::Semantic(SemanticAction::Scroll { .. }) => "scroll".to_string(),
        Action::Targeted(TargetedInput::Click { x, y, .. }) => {
            format!("click ({x:.2},{y:.2})")
        }
        Action::Targeted(TargetedInput::TypeText { text, .. }) => {
            format!("type ({} chars)", text.chars().count())
        }
        Action::Targeted(TargetedInput::KeyCombo { keys }) => format!("keys {}", keys.join("+")),
    };
    let effect_part = match effect.kind {
        EffectKind::Observe => "observe",
        EffectKind::Navigate => "navigate",
        EffectKind::LocalEdit => "local edit",
        EffectKind::ExternalCommunication => "external communication",
        EffectKind::ExternalSubmit => "external submit",
        EffectKind::Destructive => "destructive",
        EffectKind::PermissionChange => "permission change",
        EffectKind::Financial => "financial",
        EffectKind::Credential => "credential",
        EffectKind::Unknown => "unknown",
    };
    let mut out = format!("{action_part} | effect={effect_part}");
    if let Some(summary) = &effect.summary {
        if !summary.trim().is_empty() {
            let preview: String = summary.chars().take(80).collect();
            out.push_str(&format!(" | {preview}"));
        }
    }
    out
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
        /// Per-task decision maker: `vlm` (local model) or `agent` (external
        /// agent via lcu decide/act). Default: follow the Runtime process
        /// setting (LCU_VISION_ACTOR).
        #[serde(default)]
        actor: Option<String>,
        /// Task control mode: `auto` | `background_only` | `foreground`.
        #[serde(default)]
        control_mode: Option<String>,
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
    OpenGateUi {
        grant_id: String,
    },
    /// Fetch the observation the worker is waiting on for an agent decision.
    GetDecision {
        task_id: String,
    },
    /// Submit an agent decision for a pending observation. `effect` is the
    /// closed-set consequence claim (`{"kind": ..., "summary": ...}`).
    SubmitDecision {
        task_id: String,
        observation_id: String,
        action: serde_json::Value,
        #[serde(default)]
        effect: Option<serde_json::Value>,
        #[serde(default)]
        confidence: Option<f32>,
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
    GateUi {
        launch: GateUiLaunch,
    },
    Gates {
        pending: Vec<GateUiLaunch>,
    },
    /// Agent-mode observation for `lcu decide` (no image bytes; path only).
    Decision {
        task_id: String,
        observation: lcu_model::ModelObservation,
        context: lcu_model::ModelTaskContext,
        image_path: Option<String>,
        expires_in_secs: u64,
    },
    /// Echo of the parsed action for `lcu act`.
    Submitted {
        task_id: String,
        action: Action,
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
                actor,
                control_mode,
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
                match self.submit_task_with_limits(
                    goal,
                    caller,
                    selector,
                    max_steps,
                    actor,
                    control_mode,
                ) {
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
            InternalRequest::OpenGateUi { grant_id } => match self.request_gate_ui(&grant_id) {
                Ok(launch) => InternalResponse::GateUi { launch },
                Err(err) => Self::map_err_resp(err),
            }
            InternalRequest::GetDecision { task_id } => {
                let actor = &self.agent_actor;
                let record = match self.get_task(&TaskId(task_id.clone())) {
                    Ok(r) => r,
                    Err(err) => return Self::map_err_resp(err),
                };
                let Some(obs_id) = record.last_observation_id else {
                    return Self::map_err_resp(LcuError::coded(
                        ErrorCode::InvalidRequest,
                        "task has no observation yet; wait for the worker to reach the decision step (lcu decide --wait)",
                    ));
                };
                match actor.pending(&obs_id.0) {
                    Some(view) => InternalResponse::Decision {
                        task_id,
                        observation: view.observation,
                        context: view.context,
                        image_path: view.image_path.map(|p| p.display().to_string()),
                        expires_in_secs: view.expires_in_secs,
                    },
                    None => Self::map_err_resp(LcuError::coded(
                        ErrorCode::TaskNotFound,
                        format!(
                            "no pending decision for observation {}; worker may have moved on (lcu decide --wait)",
                            obs_id.0
                        ),
                    )),
                }
            }
            InternalRequest::SubmitDecision {
                task_id,
                observation_id,
                action,
                effect,
                confidence,
            } => {
                let actor = &self.agent_actor;
                let record = match self.get_task(&TaskId(task_id.clone())) {
                    Ok(r) => r,
                    Err(err) => return Self::map_err_resp(err),
                };
                if record
                    .last_observation_id
                    .as_ref()
                    .map(|o| o.0 != observation_id)
                    .unwrap_or(true)
                {
                    return Self::map_err_resp(LcuError::coded(
                        ErrorCode::InvalidRequest,
                        format!(
                            "observation_id {observation_id} is not the task's current observation; fetch a fresh decision"
                        ),
                    ));
                }
                if record.state != TaskState::WaitingActor {
                    return Self::map_err_resp(LcuError::coded(
                        ErrorCode::InvalidRequest,
                        format!("task is not waiting for an Agent decision ({:?})", record.state),
                    ));
                }
                match actor.submit(&observation_id, &action.to_string(), effect, confidence) {
                    Ok(action) => {
                        let task_id_value = TaskId(task_id.clone());
                        if let Err(err) = self.apply_command(
                            &task_id_value,
                            TaskCommand::ActorReady,
                            "external Agent decision submitted",
                        ) {
                            return Self::map_err_resp(err);
                        }
                        if let Err(err) = self.scheduler.enqueue(task_id_value) {
                            return Self::map_err_resp(err);
                        }
                        InternalResponse::Submitted { task_id, action }
                    }
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
            surface_scope: Some("chrome:profile:test:tab:1:origin:https://example.com".into()),
            image_hash: None,
            capture_backend: None,
            image_png: None,
        }
    }

    fn invoke_e1() -> Action {
        Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        })
    }

    fn navigate_effect() -> EffectClaim {
        EffectClaim::new(EffectKind::Navigate, "open")
    }

    fn park_consequence(
        rt: &Runtime,
        task: &TaskRecord,
        obs: &AppObservation,
        action: &Action,
    ) -> String {
        let effect = navigate_effect();
        let evaluated = rt
            .evaluate_action_for_task(
                Some(&task.task_id),
                obs,
                action,
                Some(&effect),
                RiskLevel::R4,
                Some(&task.caller),
            )
            .unwrap();
        let judgement = rt.effect_guard.judge(&EffectContext {
            observation: obs,
            action,
            effect: Some(&effect),
            task_authorized_max_risk: RiskLevel::R4,
        });
        assert_eq!(judgement.risk, evaluated.risk);
        rt.insert_consequence_gate(&task.task_id, &obs.target.app_id, obs, action, &effect, &judgement)
    }

    #[test]
    fn high_risk_requires_gui_only_gate() {
        let rt = test_runtime();
        let obs = sample_obs("发送");
        let action = invoke_e1();
        let evaluated = rt
            .evaluate_action(&obs, &action, Some(&navigate_effect()), RiskLevel::R4)
            .unwrap();
        // Evidence floor keeps the send label at R3 regardless of the claim.
        assert_eq!(evaluated.risk, RiskLevel::R3);
        let task = rt
            .submit_task("submit form", CallerIdentity::HumanCli, None)
            .unwrap();
        let grant_id = park_consequence(&rt, &task, &obs, &action);
        let launch = rt.request_gate_ui(&grant_id).unwrap();
        assert!(launch.gui_only);
        assert_eq!(launch.kind, GateKind::Consequence);
        assert!(rt.approve_from_cli(&grant_id).is_err());
        assert!(rt.approve_from_agent(&grant_id).is_err());
    }

    #[test]
    fn consequence_grant_approve_and_consume_once() {
        let rt = test_runtime();
        let task = rt
            .submit_task("submit form", CallerIdentity::HumanCli, None)
            .unwrap();
        let obs = sample_obs("发送");
        let action = invoke_e1();
        let grant_id = park_consequence(&rt, &task, &obs, &action);
        let binding = rt.gate_binding(&grant_id).unwrap();
        rt.approve_in_gui(&grant_id, &binding).unwrap();
        let pending = rt
            .pending_consequence_grant(&task.task_id, &obs.target.app_id)
            .expect("approved grant visible to the worker");
        assert_eq!(pending.identity.operation, "invoke");
        assert_eq!(pending.effect_kind, EffectKind::Navigate);
        rt.consume_consequence_grant(&grant_id).unwrap();
        assert!(rt.consume_consequence_grant(&grant_id).is_err());
        assert!(rt
            .pending_consequence_grant(&task.task_id, &obs.target.app_id)
            .is_none());
    }

    #[test]
    fn cancel_makes_task_terminal_and_clears_context() {
        let rt = test_runtime();
        let task = rt
            .submit_task("demo", CallerIdentity::HumanCli, None)
            .unwrap();
        rt.apply_command(&task.task_id, TaskCommand::Start, "test start")
            .unwrap();
        let obs = sample_obs("发送");
        let action = invoke_e1();
        let grant_id = park_consequence(&rt, &task, &obs, &action);
        rt.pending.lock().unwrap().insert(
            task.task_id.0.clone(),
            PendingGate {
                grant_id: grant_id.clone(),
                kind: GateKind::Consequence,
                task_id: task.task_id.clone(),
                app_key: obs.target.app_id.clone(),
                target: obs.target.clone(),
                consequence: Some(consequence_identity_for(&obs, &action)),
                evidence: screenshot_evidence_for(&obs, &action, &navigate_effect()),
                transition: "consequence confirmed".into(),
                takeover_started: false,
            },
        );
        rt.apply_command(&task.task_id, TaskCommand::WaitActor, "test gate")
            .unwrap();
        assert_eq!(rt.list_pending_gates().len(), 1);

        let cancelled = rt.cancel_task(&task.task_id).unwrap();

        assert_eq!(cancelled.state, lcu_core::task::TaskState::Cancelled);
        assert!(rt.current_target.lock().unwrap().is_empty());
        assert!(rt.list_pending_gates().is_empty());
        assert!(rt.approve_pending_in_gui(&grant_id).is_err());
        assert!(rt.grants.lock().unwrap().is_empty());
    }


    #[test]
    fn cancelling_other_task_does_not_release_current_target() {
        let rt = test_runtime();
        let current = rt
            .submit_task("current", CallerIdentity::HumanCli, None)
            .unwrap();
        let other = rt
            .submit_task("other", CallerIdentity::HumanCli, None)
            .unwrap();
        let target = sample_obs("Open").target;
        assert!(rt.set_current_target(&current.task_id, target.clone()).unwrap());

        rt.cancel_task(&other.task_id).unwrap();

        assert_eq!(rt.current_target_for(&current.task_id), Some(target.clone()));
        assert!(!rt.set_current_target(&other.task_id, target).unwrap());
        rt.cancel_task(&current.task_id).unwrap();
        assert!(rt.current_target.lock().unwrap().is_empty());
    }

    #[test]
    fn waiting_actor_releases_foreground_but_keeps_target_reservation() {
        let rt = test_runtime();
        let current = rt
            .submit_task("current", CallerIdentity::HumanCli, None)
            .unwrap();
        let target = sample_obs("Open").target;
        assert!(rt.set_current_target(&current.task_id, target.clone()).unwrap());
        rt.open_foreground_session(&current.task_id, &target).unwrap();
        assert!(rt.foreground_is_active(&current.task_id, &target));

        rt.apply_command(&current.task_id, TaskCommand::Start, "test start")
            .unwrap();
        rt.apply_command(&current.task_id, TaskCommand::WaitActor, "agent thinking")
            .unwrap();

        assert!(!rt.foreground_is_active(&current.task_id, &target));
        assert!(rt.foreground_is_authorized(&current.task_id, &target));
        assert_eq!(rt.current_target_for(&current.task_id), Some(target.clone()));

        rt.resume_foreground_session(&current.task_id, &target).unwrap();
        assert!(rt.foreground_is_active(&current.task_id, &target));
    }

    #[test]
    fn serial_surface_waiter_does_not_steal_the_live_owner() {
        let rt = test_runtime();
        let first = TaskId("first".into());
        let second = TaskId("second".into());
        assert!(rt.reserve_serial_surface_key(&first, "chrome".into()));
        assert!(!rt.reserve_serial_surface_key(&second, "chrome".into()));
        assert_eq!(rt.surface_owners.lock().unwrap().get("chrome"), Some(&first));
        rt.release_serial_surface(&first);
        assert!(rt.reserve_serial_surface_key(&second, "chrome".into()));
    }

    #[test]
    fn expired_gate_becomes_failed_on_status_read() {
        let rt = test_runtime();
        let task = rt
            .submit_task("submit form", CallerIdentity::HumanCli, None)
            .unwrap();
        rt.apply_command(&task.task_id, TaskCommand::Start, "test start")
            .unwrap();
        let obs = sample_obs("发送");
        let action = invoke_e1();
        let grant_id = park_consequence(&rt, &task, &obs, &action);
        rt.pending.lock().unwrap().insert(
            task.task_id.0.clone(),
            PendingGate {
                grant_id: grant_id.clone(),
                kind: GateKind::Consequence,
                task_id: task.task_id.clone(),
                app_key: obs.target.app_id.clone(),
                target: obs.target.clone(),
                consequence: Some(consequence_identity_for(&obs, &action)),
                evidence: screenshot_evidence_for(&obs, &action, &navigate_effect()),
                transition: "consequence confirmed".into(),
                takeover_started: false,
            },
        );
        rt.apply_command(&task.task_id, TaskCommand::WaitActor, "test gate")
            .unwrap();
        rt.gates
            .lock()
            .unwrap()
            .get_mut(&grant_id)
            .unwrap()
            .expires_at = Utc::now() - Duration::seconds(1);

        let expired = rt.get_task(&task.task_id).unwrap();

        assert_eq!(expired.state, TaskState::Failed);
        assert!(expired.error.unwrap().contains("expired"));
        assert!(rt.pending.lock().unwrap().is_empty());
        assert!(rt.gates.lock().unwrap().is_empty());
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
        let action = invoke_e1();
        let effect = navigate_effect();
        let evaluated = rt
            .evaluate_action_for_task(
                Some(&task.task_id),
                &obs,
                &action,
                Some(&effect),
                RiskLevel::R4,
                Some(&CallerIdentity::HumanCli),
            )
            .unwrap();
        assert!(evaluated.requires_takeover);
        let judgement = rt.effect_guard.judge(&EffectContext {
            observation: &obs,
            action: &action,
            effect: Some(&effect),
            task_authorized_max_risk: RiskLevel::R4,
        });
        let grant_id = rt.insert_consequence_gate(
            &task.task_id,
            &obs.target.app_id,
            &obs,
            &action,
            &effect,
            &judgement,
        );
        rt.pending.lock().unwrap().insert(
            task.task_id.0.clone(),
            PendingGate {
                grant_id: grant_id.clone(),
                kind: GateKind::Takeover,
                task_id: task.task_id.clone(),
                app_key: obs.target.app_id.clone(),
                target: obs.target.clone(),
                consequence: Some(consequence_identity_for(&obs, &action)),
                evidence: screenshot_evidence_for(&obs, &action, &effect),
                transition: "takeover complete".into(),
                takeover_started: false,
            },
        );
        assert!(rt.approve_pending_in_gui(&grant_id).is_err());
        assert!(rt.complete_takeover_in_gui(&grant_id).is_err());
        rt.begin_takeover_in_gui(&grant_id).unwrap();
        rt.complete_takeover_in_gui(&grant_id).unwrap();
        assert_eq!(
            rt.gate_binding(&grant_id).unwrap().status,
            GrantStatus::Approved
        );
    }

    #[test]
    fn consequence_identity_changes_with_object() {
        let rt = test_runtime();
        let task = rt
            .submit_task("demo", CallerIdentity::HumanCli, None)
            .unwrap();
        let obs = sample_obs("发送");
        let action = invoke_e1();
        let grant_id = park_consequence(&rt, &task, &obs, &action);
        let binding = rt.gate_binding(&grant_id).unwrap();
        rt.approve_in_gui(&grant_id, &binding).unwrap();
        let grant = rt
            .pending_consequence_grant(&task.task_id, &obs.target.app_id)
            .unwrap();
        // Same action on a different element label must not match the grant.
        let mut other = obs.clone();
        other.elements[0].label = Some("发布".into());
        let id2 = consequence_identity_for(&other, &action);
        assert_ne!(grant.identity, id2, "object change breaks identity match");

        let mut moved = obs.clone();
        moved.elements[0].frame.x += 0.1;
        assert_ne!(
            grant.identity,
            consequence_identity_for(&moved, &action),
            "same label at a different control position needs a new confirmation"
        );

        let nav_a = consequence_identity_for(
            &obs,
            &Action::Semantic(SemanticAction::Navigate {
                url: "https://example.com/a?token=one".into(),
            }),
        );
        let nav_b = consequence_identity_for(
            &obs,
            &Action::Semantic(SemanticAction::Navigate {
                url: "https://example.com/b?token=two".into(),
            }),
        );
        assert_ne!(nav_a, nav_b, "full navigation destination is confirmation-bound");

        let mut other_surface = obs.clone();
        other_surface.surface_scope = Some(
            "chrome:profile:other:tab:2:origin:https://example.com".into(),
        );
        assert_ne!(
            grant.identity,
            consequence_identity_for(&other_surface, &action),
            "Chrome profile/tab/page scope is confirmation-bound"
        );
    }

    #[test]
    fn app_access_deny_and_always_allow_persist() {
        let dir = tempdir().unwrap();
        let path = dir.path().to_path_buf();
        {
            let rt = Runtime::new(RuntimePaths::from_root(&path), Arc::new(NullBackend)).unwrap();
            let task = rt
                .submit_task("demo", CallerIdentity::HumanCli, None)
                .unwrap();
            rt.apply_command(&task.task_id, TaskCommand::Start, "test start")
                .unwrap();
            // First control of an app with no decision → gate.
            assert_eq!(
                rt.app_access_state(&task.task_id, "com.example.app"),
                AppAccessOutcome::RequestDecision
            );
            // Always allow persists and survives reload.
            let gate_id =
                rt.insert_app_access_gate(&task.task_id, "com.example.app", &sample_obs("x").target);
            rt.pending.lock().unwrap().insert(
                task.task_id.0.clone(),
                PendingGate {
                    grant_id: gate_id.clone(),
                    kind: GateKind::AppAccess,
                    task_id: task.task_id.clone(),
                    app_key: "com.example.app".into(),
                    target: sample_obs("x").target,
                    consequence: None,
                    evidence: None,
                    transition: "app access granted".into(),
                    takeover_started: false,
                },
            );
            rt.apply_command(&task.task_id, TaskCommand::WaitActor, "test gate")
                .unwrap();
            rt.app_access_in_gui(&gate_id, AppAccessDecision::AlwaysAllow)
                .unwrap();
            assert_eq!(rt.get_task(&task.task_id).unwrap().state, TaskState::Running);
            assert_eq!(
                rt.app_access_state(&task.task_id, "com.example.app"),
                AppAccessOutcome::Allowed
            );

            let denied = rt
                .submit_task("denied", CallerIdentity::HumanCli, None)
                .unwrap();
            rt.apply_command(&denied.task_id, TaskCommand::Start, "test start")
                .unwrap();
            rt.apply_command(&denied.task_id, TaskCommand::WaitActor, "test gate")
                .unwrap();
            let denied_gate = rt.insert_app_access_gate(
                &denied.task_id,
                "com.example.denied",
                &sample_obs("x").target,
            );
            rt.pending.lock().unwrap().insert(
                denied.task_id.0.clone(),
                PendingGate {
                    grant_id: denied_gate.clone(),
                    kind: GateKind::AppAccess,
                    task_id: denied.task_id.clone(),
                    app_key: "com.example.denied".into(),
                    target: sample_obs("x").target,
                    consequence: None,
                    evidence: None,
                    transition: "app access granted".into(),
                    takeover_started: false,
                },
            );
            rt.app_access_in_gui(&denied_gate, AppAccessDecision::Deny)
                .unwrap();
            assert_eq!(rt.get_task(&denied.task_id).unwrap().state, TaskState::Failed);
        }
        let rt2 = Runtime::new(RuntimePaths::from_root(&path), Arc::new(NullBackend)).unwrap();
        assert_eq!(
            rt2.app_access_state(&TaskId("other_task".into()), "com.example.app"),
            AppAccessOutcome::Allowed,
            "always_allow survives runtime restart"
        );
        assert!(rt2.revoke_app_permission("com.example.app").unwrap());
        let rt3 = Runtime::new(RuntimePaths::from_root(&path), Arc::new(NullBackend)).unwrap();
        assert_eq!(
            rt3.app_access_state(&TaskId("other_task".into()), "com.example.app"),
            AppAccessOutcome::RequestDecision,
            "revoked always_allow stays revoked after restart"
        );
    }

    #[test]
    fn session_invalidation_pauses_only_tasks_with_temporary_control_state() {
        let rt = test_runtime();
        let affected = rt
            .submit_task("affected", CallerIdentity::HumanCli, None)
            .unwrap();
        rt.apply_command(&affected.task_id, TaskCommand::Start, "start")
            .unwrap();
        let queued = rt
            .submit_task("untouched", CallerIdentity::HumanCli, None)
            .unwrap();
        let target = sample_obs("x").target;
        assert!(rt
            .set_current_target(&affected.task_id, target)
            .unwrap());

        let invalidated = rt.invalidate_session_state("test session change");
        assert!(invalidated.contains(&affected.task_id.0));
        assert_eq!(
            rt.get_task(&affected.task_id).unwrap().state,
            TaskState::PausedByUser
        );
        assert!(rt.current_target_for(&affected.task_id).is_none());
        assert_eq!(rt.get_task(&queued.task_id).unwrap().state, TaskState::Queued);
    }
}
