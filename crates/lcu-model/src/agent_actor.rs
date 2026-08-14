//! VisionActor implementation driven by an external agent through the `lcu`
//! CLI (`lcu decide` / `lcu act`), with exactly the same data surface as the
//! local VLM: compact element tree, scaled screenshot, goal/step context.
//!
//! Agent decisions are parked by observation so model think time does not hold
//! the serial desktop execution slot. The synchronous `VisionActor` adapter
//! remains for tests/embedding and waits on the same slots.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use lcu_core::action::{Action, EffectClaim, ProposedAction};
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::ObservationId;

use crate::subprocess_actor::write_private_temp_file;
use crate::{parse_action_json, ModelObservation, ModelTaskContext, VisionActor};

/// One pending agent decision. At most one exists (serial worker).
pub struct PendingDecision {
    /// Observation with `image_png` stripped (bytes never leave via IPC).
    pub observation: ModelObservation,
    pub context: ModelTaskContext,
    /// 0600 temp file the agent reads before submitting; removed on every
    /// exit path of propose_action and when the slot is replaced.
    pub image_path: Option<PathBuf>,
    deadline: Instant,
    submitted: Option<SubmittedDecision>,
    aborted: Option<String>,
}

/// An agent-submitted decision, waiting for the worker to consume it.
#[derive(Debug, Clone)]
pub struct SubmittedDecision {
    pub action: Action,
    /// Closed-set consequence claim; parsed strictly at the trust boundary.
    pub effect: Option<EffectClaim>,
    pub confidence: f32,
}

/// Snapshot returned to `lcu decide`: no image bytes, no internal flags.
#[derive(Debug, Clone)]
pub struct PendingDecisionView {
    pub observation: ModelObservation,
    pub context: ModelTaskContext,
    pub image_path: Option<PathBuf>,
    pub expires_in_secs: u64,
}

pub struct AgentActor {
    slots: Mutex<HashMap<String, PendingDecision>>,
    cv: Condvar,
    timeout: Duration,
}

impl AgentActor {
    /// Decision timeout from `LCU_AGENT_DECISION_TIMEOUT_SECS` (default 600s,
    /// min 5s).
    pub fn new() -> Self {
        crate::subprocess_actor::cleanup_stale_temp_files();
        let secs = std::env::var("LCU_AGENT_DECISION_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(600u64)
            .max(5);
        Self::with_timeout(Duration::from_secs(secs))
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
            timeout,
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Snapshot of the pending decision for `observation_id`, if any.
    pub fn pending(&self, observation_id: &str) -> Option<PendingDecisionView> {
        let guard = self.slots.lock().expect("agent slot lock");
        let dec = guard.get(observation_id)?;
        Some(PendingDecisionView {
            observation: dec.observation.clone(),
            context: dec.context.clone(),
            image_path: dec.image_path.clone(),
            expires_in_secs: dec
                .deadline
                .saturating_duration_since(Instant::now())
                .as_secs(),
        })
    }

    /// Submit a decision for `observation_id`. Parses the action JSON and the
    /// closed-set effect early so the agent gets a sharp error; semantic
    /// validation (elements/risk) is done later by the worker loop exactly
    /// like VLM proposals.
    pub fn submit(
        &self,
        observation_id: &str,
        action_json: &str,
        effect: Option<serde_json::Value>,
        confidence: Option<f32>,
    ) -> LcuResult<Action> {
        let action = parse_action_json(action_json)?;
        let effect: Option<EffectClaim> = match effect {
            Some(value) => Some(serde_json::from_value(value).map_err(|e| {
                LcuError::coded(
                    ErrorCode::InvalidRequest,
                    format!("effect is not a valid closed-set consequence: {e}"),
                )
            })?),
            None => None,
        };
        {
            let mut guard = self.slots.lock().expect("agent slot lock");
            match guard.get_mut(observation_id) {
                Some(dec) => {
                    if Instant::now() >= dec.deadline {
                        return Err(LcuError::coded(
                            ErrorCode::TaskFailed,
                            "agent decision expired; fetch a fresh observation after resume",
                        ));
                    }
                    dec.submitted = Some(SubmittedDecision {
                        action: action.clone(),
                        effect,
                        confidence: confidence.unwrap_or(0.9),
                    });
                }
                None => {
                    return Err(LcuError::coded(
                        ErrorCode::TaskNotFound,
                        "stale or no pending decision; use lcu decide --wait for a fresh observation",
                    ));
                }
            }
        }
        self.cv.notify_all();
        Ok(action)
    }

    /// Wake a parked worker immediately (task cancelled / paused / failed).
    pub fn abort_waiting(&self, observation_id: &str, reason: &str) {
        {
            let mut guard = self.slots.lock().expect("agent slot lock");
            if let Some(dec) = guard.get_mut(observation_id) {
                if dec.aborted.is_none() {
                    dec.aborted = Some(reason.to_string());
                }
            }
        }
        self.cv.notify_all();
    }

    /// Remove a parked decision immediately (task pause/cancel/terminal).
    pub fn discard(&self, observation_id: &str) {
        let removed = self
            .slots
            .lock()
            .expect("agent slot lock")
            .remove(observation_id);
        if let Some(path) = removed.and_then(|decision| decision.image_path) {
            let _ = std::fs::remove_file(path);
        }
        self.cv.notify_all();
    }

    /// Expire one still-unsubmitted decision. Returns true only when this call
    /// removed the slot; a decision submitted before its deadline wins.
    pub fn expire(&self, observation_id: &str) -> bool {
        let mut guard = self.slots.lock().expect("agent slot lock");
        let should_expire = guard.get(observation_id).is_some_and(|decision| {
            decision.submitted.is_none() && Instant::now() >= decision.deadline
        });
        if !should_expire {
            return false;
        }
        let removed = guard.remove(observation_id).expect("slot exists");
        drop(guard);
        if let Some(path) = removed.image_path {
            let _ = std::fs::remove_file(path);
        }
        true
    }

    /// Publish one observation for an external Agent without blocking the
    /// Runtime's serial execution worker.
    pub fn begin_decision(
        &self,
        observation: &ModelObservation,
        context: &ModelTaskContext,
    ) -> LcuResult<()> {
        let observation_id = observation.observation_id.clone();
        let image_path = observation.image_png.as_ref().map(|png| {
            let path = std::env::temp_dir().join(format!(
                "lcu-agent-{}-{observation_id}.png",
                std::process::id()
            ));
            let _ = write_private_temp_file(&path, png);
            path
        });
        let mut observation = observation.clone();
        observation.image_png = None;
        let replaced = self.slots.lock().expect("agent slot lock").insert(
            observation_id,
            PendingDecision {
                observation,
                context: context.clone(),
                image_path,
                deadline: Instant::now() + self.timeout,
                submitted: None,
                aborted: None,
            },
        );
        if let Some(old) = replaced.and_then(|old| old.image_path) {
            let _ = std::fs::remove_file(old);
        }
        self.cv.notify_all();
        Ok(())
    }

    /// Consume a submitted decision after the scheduler gives the task its
    /// next FIFO turn. `None` means the Agent has not submitted yet.
    pub fn take_submitted(
        &self,
        observation_id: &str,
    ) -> LcuResult<Option<(ModelObservation, ModelTaskContext, ProposedAction)>> {
        let mut guard = self.slots.lock().expect("agent slot lock");
        let Some(dec) = guard.get(observation_id) else {
            return Ok(None);
        };
        if Instant::now() >= dec.deadline {
            let expired = guard.remove(observation_id).expect("slot exists");
            drop(guard);
            if let Some(path) = expired.image_path {
                let _ = std::fs::remove_file(path);
            }
            return Err(LcuError::coded(
                ErrorCode::TaskFailed,
                format!("agent decision timed out after {}s", self.timeout.as_secs()),
            ));
        }
        if let Some(reason) = dec.aborted.clone() {
            let aborted = guard.remove(observation_id).expect("slot exists");
            drop(guard);
            if let Some(path) = aborted.image_path {
                let _ = std::fs::remove_file(path);
            }
            return Err(LcuError::coded(
                ErrorCode::WaitingUser,
                format!("agent decision aborted: {reason}"),
            ));
        }
        if dec.submitted.is_none() {
            return Ok(None);
        }
        let mut ready = guard.remove(observation_id).expect("slot exists");
        drop(guard);
        if let Some(path) = ready.image_path.take() {
            let _ = std::fs::remove_file(path);
        }
        let submitted = ready.submitted.take().expect("submitted checked");
        Ok(Some((
            ready.observation,
            ready.context,
            ProposedAction {
                observation_id: ObservationId(observation_id.to_string()),
                action: submitted.action,
                effect: submitted.effect,
                confidence: submitted.confidence,
            },
        )))
    }
}

impl Default for AgentActor {
    fn default() -> Self {
        Self::new()
    }
}

impl VisionActor for AgentActor {
    fn name(&self) -> &str {
        "agent"
    }

    fn propose_action(
        &self,
        observation: &ModelObservation,
        context: &ModelTaskContext,
    ) -> LcuResult<ProposedAction> {
        let observation_id = observation.observation_id.clone();
        self.begin_decision(observation, context)?;
        loop {
            if let Some((_, _, proposal)) = self.take_submitted(&observation_id)? {
                return Ok(proposal);
            }
            let guard = self.slots.lock().expect("agent slot lock");
            let _ = self.cv.wait_timeout(guard, Duration::from_millis(250));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    fn sample_obs(id: &str) -> ModelObservation {
        ModelObservation {
            observation_id: id.into(),
            app_id: "com.example.App".into(),
            pid: 1,
            window_id: 2,
            window_title: "t".into(),
            window_frame: [0.0, 0.0, 10.0, 10.0],
            transform_id: "transform_1".into(),
            image_hash: None,
            display_scale: 1.0,
            elements: vec![],
            image_png: None,
            image_width: 10,
            image_height: 10,
        }
    }

    fn ctx() -> ModelTaskContext {
        ModelTaskContext {
            goal: "g".into(),
            step: 0,
            last_action_summary: None,
            transition_result: None,
        }
    }

    #[test]
    fn propose_then_submit_returns_paired_action() {
        let actor = Arc::new(AgentActor::with_timeout(Duration::from_secs(60)));
        let h = {
            let a = actor.clone();
            let obs = sample_obs("obs_1");
            thread::spawn(move || a.propose_action(&obs, &ctx()))
        };
        // Wait until the slot is visible, then submit.
        for _ in 0..100 {
            if actor.pending("obs_1").is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let action = actor
            .submit("obs_1", r#"{"kind":"semantic","type":"invoke","element_id":"e1"}"#, None, None)
            .unwrap();
        assert!(matches!(action, Action::Semantic(_)));
        let proposal = h.join().unwrap().unwrap();
        assert_eq!(proposal.observation_id.0, "obs_1");
        assert!(matches!(proposal.action, Action::Semantic(_)));
        assert_eq!(
            proposal.effect.as_ref().map(|e| e.kind),
            None,
            "effect passes through as submitted"
        );
        assert!(actor.pending("obs_1").is_none(), "slot cleared after consume");
    }

    #[test]
    fn timeout_clears_slot_with_error() {
        let actor = AgentActor::with_timeout(Duration::from_millis(50));
        let obs = sample_obs("obs_t");
        let err = actor.propose_action(&obs, &ctx()).unwrap_err();
        assert!(err.to_string().contains("timed out"));
        assert!(actor.pending("obs_t").is_none(), "slot cleared after timeout");
    }

    #[test]
    fn wrong_observation_id_rejected_and_waiter_survives() {
        let actor = Arc::new(AgentActor::with_timeout(Duration::from_millis(300)));
        let h = {
            let a = actor.clone();
            let obs = sample_obs("obs_a");
            thread::spawn(move || a.propose_action(&obs, &ctx()))
        };
        for _ in 0..100 {
            if actor.pending("obs_a").is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let err = actor
            .submit("obs_wrong", r#"{"kind":"wait","milliseconds":1}"#, None, None)
            .unwrap_err();
        assert!(err.to_string().contains("stale"));
        // Waiter still alive and can be satisfied with the right id.
        actor
            .submit("obs_a", r#"{"kind":"wait","milliseconds":1}"#, None, None)
            .unwrap();
        assert!(h.join().unwrap().is_ok());
    }

    #[test]
    fn abort_wakes_waiter_immediately() {
        let actor = Arc::new(AgentActor::with_timeout(Duration::from_secs(60)));
        let h = {
            let a = actor.clone();
            let obs = sample_obs("obs_ab");
            thread::spawn(move || a.propose_action(&obs, &ctx()))
        };
        for _ in 0..100 {
            if actor.pending("obs_ab").is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        actor.abort_waiting("obs_ab", "task Cancelled");
        let err = h.join().unwrap().unwrap_err();
        assert!(err.to_string().contains("aborted"));
        assert!(actor.pending("obs_ab").is_none());
    }

    #[test]
    fn independent_pending_decisions_do_not_block_or_replace_each_other() {
        let actor = AgentActor::with_timeout(Duration::from_secs(60));
        let mut obs = sample_obs("obs_i1");
        obs.image_png = Some(b"fake png bytes".to_vec());
        actor.begin_decision(&obs, &ctx()).unwrap();
        actor.begin_decision(&sample_obs("obs_i2"), &ctx()).unwrap();
        let old_img = actor.pending("obs_i1").unwrap().image_path.unwrap();
        assert!(actor.pending("obs_i2").is_some());
        actor
            .submit("obs_i1", r#"{"kind":"wait","milliseconds":1}"#, None, None)
            .unwrap();
        assert!(actor.take_submitted("obs_i1").unwrap().is_some());
        assert!(!old_img.exists(), "consumed decision deletes its image");
        assert!(actor.pending("obs_i2").is_some());
        actor.abort_waiting("obs_i2", "test end");
        assert!(actor.take_submitted("obs_i2").is_err());
    }

    #[test]
    fn pending_view_has_no_bytes_and_rejects_bad_json() {
        let actor = Arc::new(AgentActor::with_timeout(Duration::from_secs(60)));
        let obs = sample_obs("obs_v");
        let h = {
            let a = actor.clone();
            let o = obs.clone();
            thread::spawn(move || a.propose_action(&o, &ctx()))
        };
        for _ in 0..100 {
            if actor.pending("obs_v").is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let view = actor.pending("obs_v").unwrap();
        assert!(view.observation.image_png.is_none());
        let err = actor
            .submit("obs_v", "not json at all", None, None)
            .unwrap_err();
        assert!(err.to_string().contains("parse failed"));
        // Invalid effect kind is rejected at the trust boundary, not defaulted.
        let err = actor
            .submit(
                "obs_v",
                r#"{"kind":"wait","milliseconds":1}"#,
                Some(serde_json::json!({"kind": "harmless"})),
                None,
            )
            .unwrap_err();
        assert!(err.to_string().contains("closed-set consequence"));
        actor.abort_waiting("obs_v", "test end");
        let _ = h.join();
    }
}
