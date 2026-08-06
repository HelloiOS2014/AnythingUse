//! Independent side-effect re-evaluation. Model claims are never authoritative.

use serde::{Deserialize, Serialize};

use crate::action::{Action, SemanticAction, TargetedInput};
use crate::observation::AppObservation;
use crate::risk::RiskLevel;

/// Inputs considered when re-scoring risk before execution.
#[derive(Debug, Clone)]
pub struct EffectContext<'a> {
    pub observation: &'a AppObservation,
    pub action: &'a Action,
    pub model_effect_claim: Option<&'a str>,
    pub task_authorized_max_risk: RiskLevel,
}

/// Result of independent re-evaluation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectJudgement {
    pub risk: RiskLevel,
    pub rationale: String,
    /// True when the model claim was ignored or contradicted.
    pub model_claim_overridden: bool,
}

/// Trait implemented by Runtime policy. Unknown effects default to R3.
pub trait EffectGuard: Send + Sync {
    fn judge(&self, ctx: &EffectContext<'_>) -> EffectJudgement;
}

/// Conservative default guard used until richer page semantics exist.
#[derive(Debug, Default, Clone)]
pub struct StaticEffectGuard;

impl EffectGuard for StaticEffectGuard {
    fn judge(&self, ctx: &EffectContext<'_>) -> EffectJudgement {
        let (mut risk, rationale) = classify(ctx.action, ctx.observation);
        let mut overridden = false;

        // Never trust a lower model claim; unknown/missing semantics elevate.
        if let Some(claim) = ctx.model_effect_claim {
            if looks_like_external_submit(claim) && risk < RiskLevel::R3 {
                risk = RiskLevel::R3;
                overridden = true;
            }
        }

        // Only elevate "unknown" R0. Observe/wait/control and pure focus are intentionally R0/R1
        // and must not force GUI approval (that blocks ordinary typing product paths).
        if risk == RiskLevel::R0
            && !matches!(
                ctx.action,
                Action::Observe
                    | Action::Wait { .. }
                    | Action::Done { .. }
                    | Action::Fail { .. }
                    | Action::RequestUser { .. }
                    | Action::Semantic(SemanticAction::Focus { .. })
            )
        {
            risk = RiskLevel::R3;
            overridden = true;
        }

        // Cap by task authorization is enforced by Runtime; guard only scores.
        let _ = ctx.task_authorized_max_risk;

        EffectJudgement {
            risk,
            rationale: if overridden {
                format!("{rationale}; model claim overridden or elevated")
            } else {
                rationale
            },
            model_claim_overridden: overridden,
        }
    }
}

fn classify(action: &Action, observation: &AppObservation) -> (RiskLevel, String) {
    match action {
        Action::Observe | Action::Wait { .. } => (RiskLevel::R0, "observation or wait".into()),
        Action::Done { .. } | Action::Fail { .. } | Action::RequestUser { .. } => {
            (RiskLevel::R0, "control action without side effects".into())
        }
        Action::Semantic(SemanticAction::Invoke { element_id }) => {
            // Synthetic nav_* is not a valid product path; elevate so it cannot auto-run.
            if element_id.starts_with("nav_") {
                return (
                    RiskLevel::R3,
                    format!("forbidden synthetic navigation {element_id}"),
                );
            }
            if let Some(el) = observation.elements.iter().find(|e| e.id == *element_id) {
                let text = format!(
                    "{} {}",
                    el.label.as_deref().unwrap_or(""),
                    el.value.as_deref().unwrap_or("")
                )
                .to_lowercase();
                if is_security_or_finance(&text) {
                    (RiskLevel::R4, "security or finance control".into())
                } else if is_submit_like(&text) {
                    (RiskLevel::R3, "external submit-like control".into())
                } else if el.actions.iter().any(|action| action == "AXConfirm")
                    && (el.role.contains("Text") || el.role.contains("Field"))
                    && !is_search_like(&text)
                {
                    // Return on an arbitrary editable can send/submit content. Only
                    // recognizable search/filter fields stay navigation-like.
                    (RiskLevel::R3, "editable confirm may submit content".into())
                } else {
                    (RiskLevel::R1, "semantic invoke navigation-like".into())
                }
            } else {
                // Unknown element: elevate, never auto-allow.
                (RiskLevel::R3, "unknown element defaults to R3".into())
            }
        }
        Action::Semantic(SemanticAction::SetValue { element_id, value }) => {
            let el = observation.elements.iter().find(|e| e.id == *element_id);
            let ctx = format!(
                "{} {} {}",
                el.and_then(|e| e.label.as_deref()).unwrap_or(""),
                el.and_then(|e| e.value.as_deref()).unwrap_or(""),
                el.map(|e| e.role.as_str()).unwrap_or("")
            )
            .to_lowercase();
            if is_security_or_finance(&ctx)
                || ctx.contains("password")
                || ctx.contains("passwd")
                || ctx.contains("otp")
                || ctx.contains("验证码")
                || ctx.contains("信用卡")
                || ctx.contains("card number")
                || looks_like_secret_value(value)
            {
                (RiskLevel::R4, "credential or sensitive input".into())
            } else {
                (RiskLevel::R2, "local value edit".into())
            }
        }
        Action::Semantic(SemanticAction::Focus { .. }) => (RiskLevel::R0, "focus only".into()),
        Action::Semantic(SemanticAction::Scroll { .. }) => (RiskLevel::R1, "scroll".into()),
        Action::Targeted(TargetedInput::TypeText { text }) => {
            // Same content classification as SetValue: targeted typing has no
            // element context, but the text itself can be a credential. Without
            // this check a model could type a password/OTP/card number and
            // bypass the R4 gate that SetValue enforces.
            if is_security_or_finance(text)
                || text.to_lowercase().contains("password")
                || text.to_lowercase().contains("passwd")
                || text.to_lowercase().contains("otp")
                || text.contains("验证码")
                || text.contains("信用卡")
                || text.to_lowercase().contains("card number")
                || looks_like_secret_value(text)
            {
                (RiskLevel::R4, "credential or sensitive typed text".into())
            } else {
                (RiskLevel::R2, "targeted text entry".into())
            }
        }
        Action::Targeted(TargetedInput::Click { .. })
        | Action::Targeted(TargetedInput::KeyCombo { .. }) => {
            // Untyped click/key defaults high until richer classification exists.
            (
                RiskLevel::R3,
                "untargeted semantic click/key defaults to R3".into(),
            )
        }
        Action::Exclusive(_) => (
            RiskLevel::R3,
            "exclusive input requires explicit consent".into(),
        ),
    }
}

fn is_submit_like(text: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "submit", "send", "发布", "发送", "上传", "upload", "delete", "删除", "confirm", "确认",
        "pay", "支付", "purchase", "购买",
    ];
    KEYWORDS.iter().any(|k| text.contains(k))
}

fn is_search_like(text: &str) -> bool {
    const KEYWORDS: &[&str] = &["search", "find", "filter", "query", "搜索", "查找", "筛选"];
    KEYWORDS.iter().any(|keyword| text.contains(keyword))
}

fn is_security_or_finance(text: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "password",
        "密码",
        "passkey",
        "payment",
        "支付",
        "信用卡",
        "credit card",
        "cvv",
        "otp",
        "验证码",
        "sudo",
        "administrator",
        "系统偏好",
        "security",
    ];
    KEYWORDS.iter().any(|k| text.contains(k))
}

fn looks_like_secret_value(value: &str) -> bool {
    // Heuristic only — elevates unknown sensitive typing; not a password detector.
    let v = value.trim();
    if v.len() >= 12
        && v.chars().any(|c| c.is_ascii_digit())
        && v.chars().any(|c| c.is_ascii_alphabetic())
    {
        return true;
    }
    // Common OTP shapes
    v.len() == 6 && v.chars().all(|c| c.is_ascii_digit())
}

fn looks_like_external_submit(claim: &str) -> bool {
    is_submit_like(&claim.to_lowercase())
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::observation::{
        AppObservation, AppTarget, ElementNode, ModelSize, ObservationId, Rect, TransformId,
    };
    use crate::types::Frame;

    fn obs_with_button(label: &str) -> AppObservation {
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
    fn submit_button_is_r3_and_password_is_r4() {
        let guard = StaticEffectGuard;
        let obs = obs_with_button("发送");
        let action = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let j = guard.judge(&EffectContext {
            observation: &obs,
            action: &action,
            model_effect_claim: None,
            task_authorized_max_risk: RiskLevel::R4,
        });
        assert_eq!(j.risk, RiskLevel::R3);

        let obs = obs_with_button("Password");
        let j = guard.judge(&EffectContext {
            observation: &obs,
            action: &action,
            model_effect_claim: None,
            task_authorized_max_risk: RiskLevel::R4,
        });
        assert_eq!(j.risk, RiskLevel::R4);
    }

    #[test]
    fn targeted_type_text_classifies_secrets_as_r4() {
        let guard = StaticEffectGuard;
        let obs = obs_with_button("button");

        fn judge(guard: &StaticEffectGuard, obs: &AppObservation, text: &str) -> RiskLevel {
            let action = Action::Targeted(TargetedInput::TypeText {
                text: text.into(),
            });
            guard.judge(&EffectContext {
                observation: obs,
                action: &action,
                model_effect_claim: None,
                task_authorized_max_risk: RiskLevel::R4,
            }).risk
        }

        // Plain text stays R2 (auto-executable).
        assert_eq!(judge(&guard, &obs, "hello world"), RiskLevel::R2);
        // Credential-like text is elevated to R4 (requires user takeover).
        assert_eq!(judge(&guard, &obs, "password hunter2"), RiskLevel::R4);
        assert_eq!(judge(&guard, &obs, "OTP 123456"), RiskLevel::R4);
        assert_eq!(judge(&guard, &obs, "验证码 8888"), RiskLevel::R4);
        assert_eq!(judge(&guard, &obs, "信用卡 4111111111111111"), RiskLevel::R4);
        // 6-digit numeric OTP shape triggers the secret heuristic.
        assert_eq!(judge(&guard, &obs, "482913"), RiskLevel::R4);
    }
}
