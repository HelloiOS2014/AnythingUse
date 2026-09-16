//! Strict action validation against the current observation (M4).
//!
//! Trust boundary (realignment §3.3): every executable proposal must declare a
//! closed-set `effect`; missing or invalid values are rejected here, never
//! mapped to a hidden default business policy.

use lcu_core::action::{
    is_http_navigation_url, Action, EffectClaim, SemanticAction, TargetedInput,
};
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::observation::{AppObservation, ElementNode, ObservationId};

use crate::capabilities_for_element;

/// Validate the closed-set effect declaration of an executable proposal.
///
/// `EffectKind::Unknown` is a legal declaration (it leads to stop-and-ask, not
/// auto-execution); a missing effect for an executable action is not.
pub fn validate_effect(action: &Action, effect: Option<&EffectClaim>) -> LcuResult<()> {
    let executable = matches!(action, Action::Semantic(_) | Action::Targeted(_));
    let Some(effect) = effect else {
        if executable {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                "executable action requires a closed-set effect declaration",
            ));
        }
        return Ok(());
    };
    // `EffectKind` is a closed enum; serde rejects unknown strings at parse time.
    let _ = effect.kind;
    if let Some(summary) = &effect.summary {
        if summary.len() > 200 {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                "effect summary too long (max 200 chars)",
            ));
        }
    }
    Ok(())
}

/// Validate a proposed action before Runtime execution.
pub fn validate_action(obs: &AppObservation, action: &Action) -> LcuResult<()> {
    // Observation binding: Done/Fail/RequestUser/Wait/Observe always ok.
    match action {
        Action::Observe
        | Action::Wait { .. }
        | Action::Fail { .. }
        | Action::RequestUser { .. } => Ok(()),
        Action::Done { summary } => {
            if summary.trim().len() < 8 {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    "done summary too short",
                ));
            }
            Ok(())
        }
        Action::Semantic(sem) => {
            if let SemanticAction::Navigate { url } = sem {
                if !is_http_navigation_url(url) {
                    return Err(LcuError::coded(
                        ErrorCode::InvalidRequest,
                        "navigate requires an explicit http:// or https:// URL with a host",
                    ));
                }
            }
            if let Some(id) = action.referenced_element_id() {
                // Synthetic nav_* ids are never valid product actions.
                if id.starts_with("nav_") {
                    return Err(LcuError::coded(
                        ErrorCode::InvalidRequest,
                        format!("synthetic navigation id {id} is forbidden"),
                    ));
                }
                ensure_element(obs, id)?;
            }
            match sem {
                SemanticAction::Invoke { element_id } => {
                    require_capability(obs, element_id, "invoke")?
                }
                SemanticAction::SetValue { element_id, .. } => {
                    require_capability(obs, element_id, "set_value")?
                }
                SemanticAction::Focus { element_id } => {
                    require_capability(obs, element_id, "focus")?
                }
                SemanticAction::Navigate { .. }
                | SemanticAction::GlobalBack
                | SemanticAction::Scroll { .. } => {}
            }
            Ok(())
        }
        Action::Targeted(t) => validate_targeted(obs, t),
    }
}

fn ensure_element(obs: &AppObservation, id: &str) -> LcuResult<()> {
    if obs.contains_element(id) {
        Ok(())
    } else {
        Err(LcuError::coded(
            ErrorCode::InvalidRequest,
            format!("stale or unknown element_id {id}"),
        ))
    }
}

fn require_capability(obs: &AppObservation, id: &str, required: &str) -> LcuResult<()> {
    let element = obs
        .elements
        .iter()
        .find(|element| element.id == id)
        .expect("element existence checked before capability validation");
    if capabilities_for_element(element)
        .iter()
        .any(|capability| capability == required)
    {
        Ok(())
    } else {
        Err(LcuError::coded(
            ErrorCode::UnsupportedCapability,
            format!("element {id} does not support semantic {required}"),
        ))
    }
}

fn semantic_element_at<'a>(
    obs: &'a AppObservation,
    x: f64,
    y: f64,
    required: &str,
) -> Option<&'a ElementNode> {
    obs.elements
        .iter()
        .filter(|element| {
            let frame = element.frame;
            frame.width > 0.0
                && frame.height > 0.0
                && x >= frame.x
                && x <= frame.x + frame.width
                && y >= frame.y
                && y <= frame.y + frame.height
                && capabilities_for_element(element)
                    .iter()
                    .any(|capability| capability == required)
        })
        .min_by(|a, b| {
            let area = |element: &ElementNode| element.frame.width * element.frame.height;
            area(a)
                .partial_cmp(&area(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn reject_targeted_when_semantic_exists(
    obs: &AppObservation,
    x: Option<f64>,
    y: Option<f64>,
    required: &str,
) -> LcuResult<()> {
    let element = match (x, y) {
        (Some(x), Some(y)) => semantic_element_at(obs, x, y, required),
        _ => None,
    };
    if let Some(element) = element {
        Err(LcuError::coded(
            ErrorCode::SemanticActionRequired,
            format!(
                "element {} supports {required}; targeted input is not allowed",
                element.id
            ),
        ))
    } else {
        Ok(())
    }
}

fn validate_targeted(obs: &AppObservation, t: &TargetedInput) -> LcuResult<()> {
    match t {
        TargetedInput::Click { x, y, .. } => {
            if !(0.0..=1.0).contains(x) || !(0.0..=1.0).contains(y) {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    format!("click coordinates out of bounds: ({x},{y})"),
                ));
            }
            reject_targeted_when_semantic_exists(obs, Some(*x), Some(*y), "invoke")
        }
        TargetedInput::TypeText { text, x, y } => {
            if text.is_empty() {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    "type_text empty",
                ));
            }
            if text.len() > 4000 {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    "type_text too long",
                ));
            }
            if x.is_some() != y.is_some() {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    "type_text requires both x and y or neither",
                ));
            }
            if let (Some(x), Some(y)) = (x, y) {
                if !(0.0..=1.0).contains(x) || !(0.0..=1.0).contains(y) {
                    return Err(LcuError::coded(
                        ErrorCode::InvalidRequest,
                        format!("type_text coordinates out of bounds: ({x},{y})"),
                    ));
                }
            }
            reject_targeted_when_semantic_exists(obs, *x, *y, "set_value")
        }
        TargetedInput::KeyCombo { keys } => {
            if keys.is_empty() {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    "key_combo empty",
                ));
            }
            Ok(())
        }
    }
}

/// Reject proposals bound to a different observation than the current one.
pub fn ensure_observation_binding(
    obs: &AppObservation,
    proposed_observation_id: &ObservationId,
) -> LcuResult<()> {
    if obs.observation_id == *proposed_observation_id {
        Ok(())
    } else {
        Err(LcuError::coded(
            ErrorCode::InvalidRequest,
            "action bound to stale observation_id",
        ))
    }
}

/// Compress semantic tree for the model: drop empty labels, cap count, sort by area.
pub fn compress_elements_for_model(
    obs: &AppObservation,
    max_elements: usize,
) -> Vec<lcu_core::observation::ElementNode> {
    let mut els: Vec<_> = obs
        .elements
        .iter()
        .filter(|e| {
            let has_label = e
                .label
                .as_ref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false);
            let actionable = !e.actions.is_empty();
            has_label || actionable
        })
        .cloned()
        .collect();
    els.sort_by(|a, b| {
        let aa = a.frame.width * a.frame.height;
        let bb = b.frame.width * b.frame.height;
        bb.partial_cmp(&aa).unwrap_or(std::cmp::Ordering::Equal)
    });
    els.truncate(max_elements);
    els
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcu_core::action::{Action, SemanticAction, TargetedInput};
    use lcu_core::observation::{
        AppTarget, ElementNode, ModelSize, ObservationId, Rect, TransformId,
    };
    use lcu_core::types::Frame;

    fn sample_obs() -> AppObservation {
        AppObservation {
            observation_id: ObservationId("obs1".into()),
            timestamp_ms: 0,
            target: AppTarget {
                app_id: "a".into(),
                pid: 1,
                window_id: 1,
                window_title: "t".into(),
            },
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
            elements: vec![
                ElementNode {
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
                    actions: vec!["press".into()],
                },
                ElementNode {
                    id: "e2".into(),
                    role: "text_field".into(),
                    label: Some("Search".into()),
                    value: None,
                    frame: Rect {
                        x: 0.4,
                        y: 0.4,
                        width: 0.2,
                        height: 0.1,
                    },
                    actions: vec!["AXSetValue".into()],
                },
            ],
            transform_id: TransformId("t".into()),
            surface_scope: None,
            image_hash: None,
            capture_backend: None,
            image_png: None,
        }
    }

    #[test]
    fn rejects_unknown_element_oob_click_and_nav_ids() {
        let obs = sample_obs();
        assert!(validate_action(
            &obs,
            &Action::Semantic(SemanticAction::Invoke {
                element_id: "missing".into(),
            })
        )
        .is_err());
        assert!(validate_action(
            &obs,
            &Action::Targeted(TargetedInput::Click {
                x: 1.5,
                y: 0.5,
                button: Default::default(),
            })
        )
        .is_err());
        assert!(validate_action(
            &obs,
            &Action::Semantic(SemanticAction::Invoke {
                element_id: "nav_chrome_history".into(),
            })
        )
        .is_err());
        assert!(validate_action(
            &obs,
            &Action::Semantic(SemanticAction::Invoke {
                element_id: "e1".into(),
            })
        )
        .is_ok());
    }

    #[test]
    fn navigation_requires_explicit_http_url() {
        let obs = sample_obs();
        assert!(validate_action(
            &obs,
            &Action::Semantic(SemanticAction::Navigate {
                url: "https://example.com/path".into(),
            })
        )
        .is_ok());
        for url in [
            "example.com",
            "file:///tmp/x",
            "javascript:alert(1)",
            "https://",
            "https://user:pass@example.com/",
        ] {
            assert!(validate_action(
                &obs,
                &Action::Semantic(SemanticAction::Navigate { url: url.into() })
            )
            .is_err());
        }
    }

    #[test]
    fn exposes_capabilities_and_requires_semantic_actions() {
        let obs = sample_obs();
        let model = crate::ModelObservation::from(&obs);
        assert_eq!(model.elements[0].capabilities, vec!["invoke"]);
        assert_eq!(model.elements[1].capabilities, vec!["set_value"]);

        let click = validate_action(
            &obs,
            &Action::Targeted(TargetedInput::Click {
                x: 0.15,
                y: 0.15,
                button: Default::default(),
            }),
        )
        .unwrap_err();
        assert_eq!(click.code(), ErrorCode::SemanticActionRequired);

        let type_text = validate_action(
            &obs,
            &Action::Targeted(TargetedInput::TypeText {
                text: "hello".into(),
                x: Some(0.45),
                y: Some(0.45),
            }),
        )
        .unwrap_err();
        assert_eq!(type_text.code(), ErrorCode::SemanticActionRequired);

        assert!(validate_action(
            &obs,
            &Action::Targeted(TargetedInput::TypeText {
                text: "canvas input".into(),
                x: None,
                y: None,
            })
        )
        .is_ok());

        assert!(validate_action(
            &obs,
            &Action::Semantic(SemanticAction::SetValue {
                element_id: "e2".into(),
                value: "hello".into(),
            })
        )
        .is_ok());
        assert_eq!(
            validate_action(
                &obs,
                &Action::Semantic(SemanticAction::SetValue {
                    element_id: "e1".into(),
                    value: "hello".into(),
                })
            )
            .unwrap_err()
            .code(),
            ErrorCode::UnsupportedCapability
        );
    }
}
