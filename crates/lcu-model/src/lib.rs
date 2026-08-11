//! Model isolation boundary.
//!
//! The model process may only receive scaled images, compact semantic trees, and
//! minimal task context. It must never receive platform handles, client secrets,
//! or the ability to approve actions.

pub mod agent_actor;
pub mod loop_guard;
pub mod subprocess_actor;
pub mod validate;

pub use agent_actor::AgentActor;
pub use loop_guard::{LoopGuard, LoopGuardConfig};
pub use subprocess_actor::SubprocessVisionActor;
pub use validate::{compress_elements_for_model, ensure_observation_binding, validate_action};

use lcu_core::action::{Action, ProposedAction};
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::AppObservation;
use serde::{Deserialize, Serialize};

/// Compact observation view safe to send to a model worker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelObservation {
    pub observation_id: String,
    pub app_id: String,
    pub window_title: String,
    pub elements: Vec<ModelElement>,
    /// Image bytes stay in the model worker memory only; not serialized to agents.
    #[serde(skip)]
    pub image_png: Option<Vec<u8>>,
    pub image_width: u32,
    pub image_height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelElement {
    pub id: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub frame: [f64; 4],
}

impl From<&AppObservation> for ModelObservation {
    fn from(obs: &AppObservation) -> Self {
        Self {
            observation_id: obs.observation_id.0.clone(),
            app_id: obs.target.app_id.clone(),
            window_title: obs.target.window_title.clone(),
            elements: obs
                .elements
                .iter()
                .map(|e| ModelElement {
                    id: e.id.clone(),
                    role: e.role.clone(),
                    label: e.label.clone(),
                    frame: [e.frame.x, e.frame.y, e.frame.width, e.frame.height],
                })
                .collect(),
            // Product VLM path may attach in-memory PNG; Agent IPC never serializes this field.
            image_png: obs.image_png.clone(),
            image_width: obs.model_size.width,
            image_height: obs.model_size.height,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelTaskContext {
    pub goal: String,
    pub step: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_action_summary: Option<String>,
}

/// Pluggable local VLM backend.
pub trait VisionActor: Send + Sync {
    fn name(&self) -> &str;

    fn propose_action(
        &self,
        observation: &ModelObservation,
        context: &ModelTaskContext,
    ) -> LcuResult<ProposedAction>;

    fn warm_up(&self) -> LcuResult<()> {
        Ok(())
    }
}

/// Parses strict JSON action payloads from model text output.
pub fn parse_action_json(text: &str) -> LcuResult<Action> {
    let trimmed = text.trim();
    // Accept either raw action JSON or {"action": ...} wrapper.
    if let Ok(action) = serde_json::from_str::<Action>(trimmed) {
        return Ok(action);
    }
    #[derive(Deserialize)]
    struct Wrapper {
        action: Action,
    }
    serde_json::from_str::<Wrapper>(trimmed)
        .map(|w| w.action)
        .map_err(|e| {
            LcuError::coded(
                ErrorCode::InvalidRequest,
                format!("model action JSON parse failed: {e}"),
            )
        })
}

/// Placeholder actor used until mistral.rs / Qwen3-VL is wired in M1 spike-model.
#[derive(Debug, Default)]
pub struct NullVisionActor;

impl VisionActor for NullVisionActor {
    fn name(&self) -> &str {
        "null"
    }

    fn propose_action(
        &self,
        observation: &ModelObservation,
        context: &ModelTaskContext,
    ) -> LcuResult<ProposedAction> {
        let _ = (observation, context);
        Err(LcuError::coded(
            ErrorCode::NotImplemented,
            "NullVisionActor: load Qwen3-VL-4B 4-bit via mistral.rs in spike-model",
        ))
    }
}

/// Tiny test-only actor: invoke first button-like element, else Done.
///
/// Not used on the product path (product uses Qwen subprocess only).
#[derive(Debug, Default)]
pub struct FakeActor;

impl VisionActor for FakeActor {
    fn name(&self) -> &str {
        "fake"
    }

    fn propose_action(
        &self,
        observation: &ModelObservation,
        context: &ModelTaskContext,
    ) -> LcuResult<ProposedAction> {
        use lcu_core::action::SemanticAction;
        use lcu_core::observation::ObservationId;

        let button = observation.elements.iter().find(|e| {
            let role = e.role.to_lowercase();
            let label = e.label.as_deref().unwrap_or("").to_lowercase();
            role.contains("button")
                || role.contains("link")
                || label.contains("open")
                || label.contains("ok")
        });
        let action = if let Some(el) = button {
            Action::Semantic(SemanticAction::Invoke {
                element_id: el.id.clone(),
            })
        } else if context.step == 0 && !observation.elements.is_empty() {
            Action::Semantic(SemanticAction::Invoke {
                element_id: observation.elements[0].id.clone(),
            })
        } else {
            Action::Done {
                summary: format!("fake done for: {}", context.goal),
            }
        };
        Ok(ProposedAction {
            observation_id: ObservationId(observation.observation_id.clone()),
            action,
            effect_claim: None,
            expected_effect: None,
            model_claimed_risk: None,
            confidence: 1.0,
        })
    }
}

/// Model package identity used for download/hash gates.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelManifest {
    pub name: String,
    pub repo: String,
    pub revision: String,
    pub quant: String,
    pub expected_min_bytes: u64,
    pub local_dir: String,
}

impl ModelManifest {
    pub fn qwen3_vl_4b_4bit() -> Self {
        Self {
            name: "Qwen3-VL-4B-Instruct-4bit".into(),
            repo: "Qwen/Qwen3-VL-4B-Instruct".into(),
            revision: "main".into(),
            quant: "4bit".into(),
            expected_min_bytes: 1_000_000_000,
            local_dir: "models/Qwen3-VL-4B-Instruct".into(),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use lcu_core::action::SemanticAction;

    
    #[test]
    fn parses_strict_action_json() {
        let text = r#"{"action":{"kind":"done","summary":"ok"}}"#;
        let action = parse_action_json(text).unwrap();
        assert!(matches!(action, Action::Done { .. }));

        let invoke = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let serialized = serde_json::to_string(&invoke).unwrap();
        let parsed = parse_action_json(&serialized).unwrap();
        assert_eq!(parsed, invoke);

        let navigate = parse_action_json(
            r#"{"kind":"semantic","type":"navigate","url":"https://example.com"}"#,
        )
        .unwrap();
        assert!(matches!(
            navigate,
            Action::Semantic(SemanticAction::Navigate { .. })
        ));
    }
}

