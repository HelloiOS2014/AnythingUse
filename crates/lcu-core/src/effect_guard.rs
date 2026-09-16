//! macOS evidence layer for the shared `EffectGuard`.
//!
//! Scaffolding (context, judgement, trait, closed-set policy table) lives in
//! `anything-core::effect_guard`. This module keeps the **macOS evidence
//! implementation**: `StaticEffectGuard` classifies actions against AX
//! observation evidence (`AXConfirm` action strings, AX role casing). Other
//! endpoints (`lau` on Android) must not reuse this implementation; they
//! define their own evidence layer over `anything_core::effect_policy`
//! (see `docs/lau-android-plan.md` §7).

pub use anything_core::effect_guard::{
    effect_policy, is_executable, EffectContext, EffectGuard, EffectJudgement,
};

use anything_core::{
    Action, EffectKind, RiskLevel, SemanticAction, TargetedInput,
};

/// Conservative default guard used until richer page semantics exist.
#[derive(Debug, Default, Clone)]
pub struct StaticEffectGuard;

impl EffectGuard for StaticEffectGuard {
    fn judge(&self, ctx: &EffectContext<'_>) -> EffectJudgement {
        let mut overridden = false;

        // Evidence floor from the fresh observation + action (never lowered).
        let (mut risk, mut rationale) = classify(ctx.action, ctx.observation);

        // Actor closed-set classification → base policy risk. Only raises.
        if let Some(claim) = ctx.effect {
            if let Some((declared, reason)) = effect_policy(claim.kind) {
                if declared > risk {
                    risk = declared;
                    rationale = format!("{reason}; actor claim elevated risk");
                    overridden = true;
                } else if declared < risk {
                    rationale = format!("{rationale}; actor claim could not lower evidence floor");
                    overridden = true;
                }
            }
            if claim.kind == EffectKind::Unknown {
                return EffectJudgement {
                    risk: risk.max(RiskLevel::R3),
                    rationale: "actor declared unknown consequence; stop and ask the user".into(),
                    model_claim_overridden: false,
                    unknown: true,
                };
            }
        } else if is_executable(ctx.action) {
            // Executable action without a closed-set effect: fail closed.
            return EffectJudgement {
                risk: risk.max(RiskLevel::R3),
                rationale: "executable action has no effect declaration; treat as unknown".into(),
                model_claim_overridden: false,
                unknown: true,
            };
        }

        EffectJudgement {
            risk,
            rationale,
            model_claim_overridden: overridden,
            unknown: false,
        }
    }
}

/// Evidence-based classification of the action against the current observation.
fn classify(action: &Action, observation: &anything_core::AppObservation) -> (RiskLevel, String) {
    match action {
        Action::Observe | Action::Wait { .. } => (RiskLevel::R0, "observation or wait".into()),
        Action::Done { .. } | Action::Fail { .. } | Action::RequestUser { .. } => {
            (RiskLevel::R0, "control action without side effects".into())
        }
        Action::Semantic(SemanticAction::Navigate { url }) => {
            let query_or_fragment = url
                .find(['?', '#'])
                .map(|start| url[start + 1..].to_lowercase())
                .unwrap_or_default();
            if has_sensitive_navigation_data(&query_or_fragment) {
                (
                    RiskLevel::R4,
                    "navigation contains sensitive query or fragment".into(),
                )
            } else {
                (RiskLevel::R1, "browser navigation".into())
            }
        }
        Action::Semantic(SemanticAction::GlobalBack) => {
            (RiskLevel::R1, "platform back navigation".into())
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
        // Input primitives carry no risk floor by themselves (§3.3): the
        // actor's closed-set classification + observation evidence decide.
        Action::Targeted(input) => classify_targeted(input),
    }
}

fn classify_targeted(input: &TargetedInput) -> (RiskLevel, String) {
    match input {
        // Content evidence only: typed text that looks like a credential
        // elevates regardless of the actor's classification.
        TargetedInput::TypeText { text, .. } => {
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
                (RiskLevel::R0, "targeted text input".into())
            }
        }
        // No evidence either way: the actor classification (or observation
        // evidence) decides; a bare coordinate click is never auto-R3.
        TargetedInput::Click { .. } | TargetedInput::KeyCombo { .. } => {
            (RiskLevel::R0, "targeted input".into())
        }
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

fn has_sensitive_navigation_data(query_or_fragment: &str) -> bool {
    const SENSITIVE_KEYS: &[&str] = &[
        "token",
        "access_token",
        "id_token",
        "secret",
        "client_secret",
        "credential",
        "authorization",
        "api_key",
        "apikey",
        "session",
    ];
    is_security_or_finance(query_or_fragment)
        || query_or_fragment
            .split(['&', ';', '#', '?'])
            .map(|part| part.split('=').next().unwrap_or_default())
            .any(|key| SENSITIVE_KEYS.contains(&key))
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

#[cfg(test)]
mod tests {

    use super::*;
    use anything_core::action::{EffectClaim, TargetedInput};
    use anything_core::observation::{
        AppObservation, AppTarget, ElementNode, ModelSize, ObservationId, Rect, TransformId,
    };
    use anything_core::types::Frame;

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
            surface_scope: None,
            image_hash: None,
            capture_backend: None,
            image_png: None,
        }
    }

    fn judge(
        guard: &StaticEffectGuard,
        obs: &AppObservation,
        action: &Action,
        effect: Option<EffectKind>,
    ) -> EffectJudgement {
        let effect = effect.map(|kind| EffectClaim::new(kind, "summary"));
        guard.judge(&EffectContext {
            observation: obs,
            action,
            effect: effect.as_ref(),
            task_authorized_max_risk: RiskLevel::R4,
        })
    }

    #[test]
    fn submit_label_floors_r3_and_password_r4_even_with_navigate_claim() {
        let guard = StaticEffectGuard;
        let action = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        // Actor claims navigate on a Send-labeled control: evidence floors R3.
        let j = judge(&guard, &obs_with_button("发送"), &action, Some(EffectKind::Navigate));
        assert_eq!(j.risk, RiskLevel::R3);
        assert!(j.model_claim_overridden);

        // Password control stays R4 regardless of claim.
        let j = judge(
            &guard,
            &obs_with_button("Password"),
            &action,
            Some(EffectKind::Navigate),
        );
        assert_eq!(j.risk, RiskLevel::R4);
    }

    #[test]
    fn ordinary_invoke_with_navigate_claim_stays_r1() {
        let guard = StaticEffectGuard;
        let obs = obs_with_button("Open");
        let action = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let j = judge(&guard, &obs, &action, Some(EffectKind::Navigate));
        assert_eq!(j.risk, RiskLevel::R1);
        assert!(!j.unknown);
    }

    #[test]
    fn navigation_uses_existing_r1_gate() {
        let guard = StaticEffectGuard;
        let obs = obs_with_button("button");
        let risk = |url: &str| {
            let action = Action::Semantic(SemanticAction::Navigate { url: url.into() });
            judge(&guard, &obs, &action, Some(EffectKind::Navigate)).risk
        };
        assert_eq!(risk("https://example.com/path"), RiskLevel::R1);
        assert_eq!(risk("https://example.com/?password=secret"), RiskLevel::R4);
        assert_eq!(risk("https://example.com/#otp=123456"), RiskLevel::R4);
        assert_eq!(risk("https://example.com/?token=abc"), RiskLevel::R4);
        assert_eq!(risk("https://example.com/#api_key=abc"), RiskLevel::R4);
    }

    #[test]
    fn targeted_click_follows_actor_classification_not_input_primitive() {
        let guard = StaticEffectGuard;
        let obs = obs_with_button("button");
        let click = Action::Targeted(TargetedInput::Click {
            x: 0.5,
            y: 0.5,
            button: Default::default(),
        });
        // The core policy change: an ordinary screenshot click classified
        // navigate is not auto-R3; send/pay classifications raise.
        let j = judge(&guard, &obs, &click, Some(EffectKind::Navigate));
        assert_eq!(j.risk, RiskLevel::R1);
        let j = judge(&guard, &obs, &click, Some(EffectKind::ExternalCommunication));
        assert_eq!(j.risk, RiskLevel::R3);
        let j = judge(&guard, &obs, &click, Some(EffectKind::Financial));
        assert_eq!(j.risk, RiskLevel::R4);
    }

    #[test]
    fn targeted_type_text_classifies_secrets_as_r4() {
        let guard = StaticEffectGuard;
        let obs = obs_with_button("button");

        fn judge_text(
            guard: &StaticEffectGuard,
            obs: &AppObservation,
            text: &str,
        ) -> EffectJudgement {
            let action = Action::Targeted(TargetedInput::TypeText {
                text: text.into(),
                x: None,
                y: None,
            });
            judge(guard, obs, &action, Some(EffectKind::LocalEdit))
        }

        // Plain text with local_edit classification stays R2 (auto-executable).
        assert_eq!(judge_text(&guard, &obs, "hello world").risk, RiskLevel::R2);
        // Credential-like text is elevated to R4 (requires user takeover).
        assert_eq!(judge_text(&guard, &obs, "password hunter2").risk, RiskLevel::R4);
        assert_eq!(judge_text(&guard, &obs, "OTP 123456").risk, RiskLevel::R4);
        assert_eq!(judge_text(&guard, &obs, "验证码 8888").risk, RiskLevel::R4);
        assert_eq!(
            judge_text(&guard, &obs, "信用卡 4111111111111111").risk,
            RiskLevel::R4
        );
        // 6-digit numeric OTP shape triggers the secret heuristic.
        assert_eq!(judge_text(&guard, &obs, "482913").risk, RiskLevel::R4);
    }

    #[test]
    fn unknown_or_missing_effect_stops() {
        let guard = StaticEffectGuard;
        let obs = obs_with_button("button");
        let click = Action::Targeted(TargetedInput::Click {
            x: 0.5,
            y: 0.5,
            button: Default::default(),
        });
        // Explicit unknown → stop and ask the user.
        let j = judge(&guard, &obs, &click, Some(EffectKind::Unknown));
        assert!(j.unknown);
        // Missing effect on an executable action → fail closed as unknown.
        let j = guard.judge(&EffectContext {
            observation: &obs,
            action: &click,
            effect: None,
            task_authorized_max_risk: RiskLevel::R4,
        });
        assert!(j.unknown);
        // Control actions do not need an effect.
        let j = guard.judge(&EffectContext {
            observation: &obs,
            action: &Action::Done { summary: "ok".into() },
            effect: None,
            task_authorized_max_risk: RiskLevel::R4,
        });
        assert!(!j.unknown);
        assert_eq!(j.risk, RiskLevel::R0);
    }
}
