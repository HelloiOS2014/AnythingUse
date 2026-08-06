//! VisionActor implementation driven by an external agent through the `lcu`
//! CLI (`lcu decide` / `lcu act`), with exactly the same data surface as the
//! local VLM: compact element tree, scaled screenshot, goal/step context.
//!
//! The product worker is a serial FIFO, so at most one decision is pending at
//! any time. `propose_action` parks the worker thread on a condvar until the
//! agent submits a decision for the matching observation_id, or the decision
//! times out / is aborted by task cancel/pause.

use std::path::PathBuf;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use lcu_core::action::{Action, ProposedAction};
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
    pub effect_claim: Option<String>,
    pub expected_effect: Option<String>,
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
    slot: Mutex<Option<PendingDecision>>,
    cv: Condvar,
    timeout: Duration,
}

impl AgentActor {
    /// Decision timeout from `LCU_AGENT_DECISION_TIMEOUT_SECS` (default 600s,
    /// min 5s).
    pub fn new() -> Self {
        let secs = std::env::var("LCU_AGENT_DECISION_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(600u64)
            .max(5);
        Self::with_timeout(Duration::from_secs(secs))
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            slot: Mutex::new(None),
            cv: Condvar::new(),
            timeout,
        }
    }

    /// Snapshot of the pending decision for `observation_id`, if any.
    pub fn pending(&self, observation_id: &str) -> Option<PendingDecisionView> {
        let guard = self.slot.lock().expect("agent slot lock");
        let dec = guard.as_ref()?;
        if dec.observation.observation_id != observation_id {
            return None;
        }
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

    /// Submit a decision for `observation_id`. Parses the action JSON early so
    /// the agent gets a sharp error; semantic validation (elements/risk) is
    /// done later by the worker loop exactly like VLM proposals.
    pub fn submit(
        &self,
        observation_id: &str,
        action_json: &str,
        effect_claim: Option<String>,
        expected_effect: Option<String>,
        confidence: Option<f32>,
    ) -> LcuResult<Action> {
        let action = parse_action_json(action_json)?;
        {
            let mut guard = self.slot.lock().expect("agent slot lock");
            match guard.as_mut() {
                Some(dec) if dec.observation.observation_id == observation_id => {
                    dec.submitted = Some(SubmittedDecision {
                        action: action.clone(),
                        effect_claim,
                        expected_effect,
                        confidence: confidence.unwrap_or(0.9),
                    });
                }
                Some(_) => {
                    return Err(LcuError::coded(
                        ErrorCode::InvalidRequest,
                        format!(
                            "observation_id {observation_id} is stale; fetch a fresh decision with lcu decide"
                        ),
                    ));
                }
                None => {
                    return Err(LcuError::coded(
                        ErrorCode::TaskNotFound,
                        "no pending decision; the worker may not have reached the decision step yet (use lcu decide --wait)",
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
            let mut guard = self.slot.lock().expect("agent slot lock");
            if let Some(dec) = guard.as_mut() {
                if dec.observation.observation_id == observation_id && dec.aborted.is_none() {
                    dec.aborted = Some(reason.to_string());
                }
            }
        }
        self.cv.notify_all();
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

        // Screenshot hand-off via a 0600 temp file (same pattern as the VLM
        // path); the agent must read it before submitting. Bytes never enter
        // the IPC JSON.
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

        let mut guard = self.slot.lock().expect("agent slot lock");
        // Replace any stale pending decision (previous timeout/abort) and its
        // image file. `ponytail:` a killed process can leave lcu-agent-*.png
        // behind, same known trait as the VLM temp files; no startup sweep.
        if let Some(old) = guard.take() {
            if let Some(p) = old.image_path {
                let _ = std::fs::remove_file(p);
            }
        }
        *guard = Some(PendingDecision {
            observation,
            context: context.clone(),
            image_path,
            deadline: Instant::now() + self.timeout,
            submitted: None,
            aborted: None,
        });
        drop(guard);

        loop {
            let mut guard = self.slot.lock().expect("agent slot lock");
            let dec = guard.as_mut().expect("slot held by this proposal");
            if dec.observation.observation_id != observation_id {
                // Replaced by a newer proposal (e.g. retry after timeout); the
                // new waiter owns the slot. Leave it and its image alone.
                return Err(LcuError::coded(
                    ErrorCode::TaskFailed,
                    "agent decision superseded by a newer observation; refetch with lcu decide",
                ));
            }
            if let Some(sub) = dec.submitted.take() {
                let image = dec.image_path.take();
                let _ = guard.take();
                drop(guard);
                if let Some(p) = image {
                    let _ = std::fs::remove_file(p);
                }
                return Ok(ProposedAction {
                    observation_id: ObservationId(observation_id),
                    action: sub.action,
                    effect_claim: sub.effect_claim,
                    expected_effect: sub.expected_effect,
                    model_claimed_risk: None,
                    confidence: sub.confidence,
                });
            }
            if let Some(reason) = dec.aborted.clone() {
                let image = dec.image_path.take();
                let _ = guard.take();
                drop(guard);
                if let Some(p) = image {
                    let _ = std::fs::remove_file(p);
                }
                return Err(LcuError::coded(
                    ErrorCode::WaitingUser,
                    format!("agent decision aborted: {reason}"),
                ));
            }
            if Instant::now() >= dec.deadline {
                let image = dec.image_path.take();
                let _ = guard.take();
                drop(guard);
                if let Some(p) = image {
                    let _ = std::fs::remove_file(p);
                }
                return Err(LcuError::coded(
                    ErrorCode::TaskFailed,
                    format!(
                        "agent decision timed out after {}s; fetch a fresh observation with lcu decide",
                        self.timeout.as_secs()
                    ),
                ));
            }
            // 250ms slices: keeps abort/timeout responsive without busy-wait.
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
            window_title: "t".into(),
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
            .submit("obs_1", r#"{"kind":"semantic","type":"invoke","element_id":"e1"}"#, None, None, None)
            .unwrap();
        assert!(matches!(action, Action::Semantic(_)));
        let proposal = h.join().unwrap().unwrap();
        assert_eq!(proposal.observation_id.0, "obs_1");
        assert!(matches!(proposal.action, Action::Semantic(_)));
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
            .submit("obs_wrong", r#"{"kind":"wait","milliseconds":1}"#, None, None, None)
            .unwrap_err();
        assert!(err.to_string().contains("stale"));
        // Waiter still alive and can be satisfied with the right id.
        actor
            .submit("obs_a", r#"{"kind":"wait","milliseconds":1}"#, None, None, None)
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
    fn replacing_proposal_deletes_old_image() {
        let actor = Arc::new(AgentActor::with_timeout(Duration::from_secs(60)));
        let mut obs = sample_obs("obs_i1");
        obs.image_png = Some(b"fake png bytes".to_vec());

        let h1 = {
            let a = actor.clone();
            let o = obs.clone();
            thread::spawn(move || a.propose_action(&o, &ctx()))
        };
        let old_img = loop {
            if let Some(v) = actor.pending("obs_i1") {
                assert!(v.image_path.is_some(), "image path handed to agent");
                break v.image_path.unwrap();
            }
            thread::sleep(Duration::from_millis(10));
        };
        // Second proposal replaces the slot and must delete the first image.
        let h2 = {
            let a = actor.clone();
            let o = sample_obs("obs_i2");
            thread::spawn(move || a.propose_action(&o, &ctx()))
        };
        for _ in 0..100 {
            if actor.pending("obs_i2").is_some() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let p1 = h1.join().unwrap().unwrap_err(); // superseded
        assert!(p1.to_string().contains("superseded"));
        // Old image file removed; new slot has no image.
        assert!(!old_img.exists(), "old image deleted on replacement");
        let v2 = actor.pending("obs_i2").unwrap();
        assert!(v2.image_path.is_none());
        actor.abort_waiting("obs_i2", "test end");
        let _ = h2.join();
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
            .submit("obs_v", "not json at all", None, None, None)
            .unwrap_err();
        assert!(err.to_string().contains("parse failed"));
        actor.abort_waiting("obs_v", "test end");
        let _ = h.join();
    }
}
