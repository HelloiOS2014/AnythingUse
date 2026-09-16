//! Android evidence layer (plan §5.6).
//!
//! The Runtime — not the Actor — decides the risk floor. Evidence comes from the
//! dump the Actor was actually given (role / label / value / capabilities /
//! `password`), and the Actor's closed-set `EffectClaim` can only *raise* it.
//! Nothing here keys off an application name or package (mac execution contract
//! §1): package identity is for app access and audit, never for policy.

use anything_core::action::{Action, EffectClaim, EffectKind, SemanticAction, TargetedInput};
use anything_core::effect_guard::{effect_policy, is_executable};
use anything_core::risk::RiskLevel;
use serde_json::Value;

/// Fields aligned with `anything_core::effect_guard::EffectJudgement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgement {
    pub risk: RiskLevel,
    pub rationale: String,
    pub model_claim_overridden: bool,
    /// Actor cannot classify the consequence (or an executable action carried no
    /// effect): stop and ask a human; never auto-execute.
    pub unknown: bool,
}

fn element<'a>(elements: &'a [Value], id: &str) -> Option<&'a Value> {
    elements.iter().find(|e| e.get("id").and_then(|v| v.as_str()) == Some(id))
}

fn text_of(el: Option<&Value>) -> String {
    let Some(el) = el else {
        return String::new();
    };
    let mut out = String::new();
    for key in ["label", "value", "role"] {
        if let Some(s) = el.get(key).and_then(|v| v.as_str()) {
            out.push_str(s);
            out.push(' ');
        }
    }
    out.to_lowercase()
}

/// Credential or financial surface: never auto-execute (plan §5.6 → R4).
fn is_security_or_finance(text: &str) -> bool {
    const KEYS: &[&str] = &[
        "password",
        "passwd",
        "密码",
        "passkey",
        "验证码",
        "otp",
        "cvv",
        "支付密码",
        "信用卡",
        "credit card",
        "card number",
        "支付",
        "转账",
        "payment",
        "wallet",
        "钱包",
        "实名",
        "身份证",
        "sudo",
        "administrator",
    ];
    KEYS.iter().any(|k| text.contains(k))
}

/// Outbound or irreversible: one-time confirmation (plan §5.6 → R3).
fn is_submit_like(text: &str) -> bool {
    const KEYS: &[&str] = &[
        "发送",
        "提交",
        "发布",
        "上传",
        "删除",
        "卸载",
        "确认",
        "确认支付",
        "购买",
        "下单",
        "清空",
        "格式化",
        "重置",
        "注销",
        "退出登录",
        "send",
        "submit",
        "publish",
        "upload",
        "delete",
        "remove",
        "uninstall",
        "confirm",
        "purchase",
        "pay",
        "reset",
        "erase",
        "sign out",
        "log out",
    ];
    KEYS.iter().any(|k| text.contains(k))
}

/// Recognisable search/filter fields stay navigation-like rather than R3.
fn is_search_like(text: &str) -> bool {
    const KEYS: &[&str] = &["搜索", "查找", "筛选", "search", "find", "filter", "query"];
    KEYS.iter().any(|k| text.contains(k))
}

/// Heuristic only: a value shaped like a secret is treated as one.
fn looks_like_secret(value: &str) -> bool {
    let v = value.trim();
    if v.len() >= 12
        && v.chars().any(|c| c.is_ascii_digit())
        && v.chars().any(|c| c.is_ascii_alphabetic())
    {
        return true;
    }
    v.len() == 6 && v.chars().all(|c| c.is_ascii_digit())
}

fn is_password_field(el: Option<&Value>) -> bool {
    el.and_then(|e| e.get("password"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn declares(el: Option<&Value>, capability: &str) -> bool {
    el.and_then(|e| e.get("capabilities"))
        .and_then(|v| v.as_array())
        .map(|caps| caps.iter().any(|c| c.as_str() == Some(capability)))
        .unwrap_or(false)
}

/// Evidence floor from the observation the Actor was given.
fn classify(action: &Action, elements: &[Value]) -> (RiskLevel, String) {
    match action {
        Action::Observe | Action::Wait { .. } => (RiskLevel::R0, "observation or wait".into()),
        Action::Done { .. } | Action::Fail { .. } | Action::RequestUser { .. } => {
            (RiskLevel::R0, "control action without side effects".into())
        }
        Action::Semantic(SemanticAction::Navigate { url }) => {
            let tail = url
                .find(['?', '#'])
                .map(|i| url[i + 1..].to_lowercase())
                .unwrap_or_default();
            if is_security_or_finance(&tail) || tail.contains("token") || tail.contains("secret") {
                (RiskLevel::R4, "navigation carries sensitive data".into())
            } else {
                (RiskLevel::R1, "browser navigation".into())
            }
        }
        Action::Semantic(SemanticAction::Focus { .. }) => (RiskLevel::R0, "focus only".into()),
        Action::Semantic(SemanticAction::Scroll { element_id, .. }) => {
            match element_id.as_deref().map(|id| element(elements, id)) {
                Some(None) => (RiskLevel::R3, "scroll target not in this observation".into()),
                _ => (RiskLevel::R1, "scroll".into()),
            }
        }
        // A platform-level back is navigation-like; it has no element to point at.
        Action::Semantic(SemanticAction::GlobalBack) => {
            (RiskLevel::R1, "system back navigation".into())
        }
        Action::Semantic(SemanticAction::Invoke { element_id }) => {
            let el = element(elements, element_id);
            if el.is_none() {
                return (
                    RiskLevel::R3,
                    format!("element {element_id} is not in this observation"),
                );
            }
            let text = text_of(el);
            if is_security_or_finance(&text) {
                (RiskLevel::R4, "security or finance control".into())
            } else if is_submit_like(&text) && !is_search_like(&text) {
                (RiskLevel::R3, "outbound or irreversible control".into())
            } else {
                (RiskLevel::R1, "semantic invoke, navigation-like".into())
            }
        }
        Action::Semantic(SemanticAction::SetValue { element_id, value }) => {
            let el = element(elements, element_id);
            if el.is_none() {
                return (
                    RiskLevel::R3,
                    format!("element {element_id} is not in this observation"),
                );
            }
            let text = text_of(el);
            if is_password_field(el)
                || is_security_or_finance(&text)
                || looks_like_secret(value)
            {
                (RiskLevel::R4, "credential or sensitive input".into())
            } else {
                (RiskLevel::R2, "local value edit".into())
            }
        }
        // Coordinates are refused downstream (§0 D5); when they are proposed the
        // evidence layer will not pretend they are harmless.
        Action::Targeted(TargetedInput::TypeText { text, .. }) => {
            if is_security_or_finance(&text) || looks_like_secret(text) {
                (RiskLevel::R4, "credential-like typed text".into())
            } else {
                (RiskLevel::R3, "coordinate input is not a semantic action".into())
            }
        }
        Action::Targeted(_) => (
            RiskLevel::R3,
            "coordinate input is not a semantic action".into(),
        ),
    }
}

/// Judge one proposed action against the observation it was bound to.
pub fn judge(elements: &[Value], action: &Action, effect: Option<&EffectClaim>) -> Judgement {
    let (mut risk, mut rationale) = classify(action, elements);
    let mut overridden = false;

    // An element action must target a node that advertises the capability; a
    // guess is never navigation-like.
    if let Some(id) = action.referenced_element_id() {
        let el = element(elements, id);
        if el.is_some() {
            let needed = match action {
                Action::Semantic(SemanticAction::Invoke { .. }) => Some("invoke"),
                Action::Semantic(SemanticAction::SetValue { .. }) => Some("set_value"),
                Action::Semantic(SemanticAction::Scroll { .. }) => Some("scroll"),
                _ => None,
            };
            if let Some(cap) = needed {
                if !declares(el, cap) {
                    risk = risk.max(RiskLevel::R3);
                    rationale = format!("{rationale}; {id} did not advertise {cap}");
                    overridden = true;
                }
            }
        }
    }

    // The Actor's closed-set claim feeds the shared policy table and can only
    // raise the floor.
    if let Some(claim) = effect {
        if let Some((declared, reason)) = effect_policy(claim.kind) {
            if declared > risk {
                risk = declared;
                rationale = format!("{reason}; actor claim elevated risk");
                overridden = true;
            } else if declared < risk {
                rationale = format!("{rationale}; actor claim could not lower the evidence floor");
                overridden = true;
            }
        }
        if claim.kind == EffectKind::Unknown {
            return Judgement {
                risk: risk.max(RiskLevel::R3),
                rationale: "actor declared unknown consequence; stop and ask the user".into(),
                model_claim_overridden: false,
                unknown: true,
            };
        }
    } else if is_executable(action) {
        return Judgement {
            risk: risk.max(RiskLevel::R3),
            rationale: "executable action has no effect declaration; treat as unknown".into(),
            model_claim_overridden: false,
            unknown: true,
        };
    }

    Judgement {
        risk,
        rationale,
        model_claim_overridden: overridden,
        unknown: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn elements() -> Vec<Value> {
        vec![
            json!({"id": "e1", "role": "Button", "label": "发送", "capabilities": ["invoke"]}),
            json!({"id": "e2", "role": "Button", "label": "搜索", "capabilities": ["invoke"]}),
            json!({"id": "e3", "role": "EditText", "capabilities": ["set_value", "focus"]}),
            json!({"id": "e4", "role": "EditText", "password": true, "label": "密码", "capabilities": ["set_value"]}),
            json!({"id": "e5", "role": "LinearLayout", "label": "蓝牙 已开启", "capabilities": ["invoke"]}),
        ]
    }

    fn claim(kind: EffectKind) -> EffectClaim {
        EffectClaim::new(kind, "test")
    }

    fn j(action: Action, effect: Option<EffectKind>) -> Judgement {
        let c = effect.map(claim);
        judge(&elements(), &action, c.as_ref())
    }

    fn invoke(id: &str) -> Action {
        Action::Semantic(SemanticAction::Invoke {
            element_id: id.into(),
        })
    }

    #[test]
    fn ordinary_navigation_stays_r1() {
        assert_eq!(j(invoke("e5"), Some(EffectKind::Navigate)).risk, RiskLevel::R1);
    }

    #[test]
    fn submit_label_floors_r3_even_when_claimed_as_navigate() {
        // Acceptance 15: a delete/pay/send label cannot be talked down.
        let v = j(invoke("e1"), Some(EffectKind::Navigate));
        assert_eq!(v.risk, RiskLevel::R3);
        assert!(v.model_claim_overridden);
    }

    #[test]
    fn password_field_forces_r4() {
        let action = Action::Semantic(SemanticAction::SetValue {
            element_id: "e4".into(),
            value: "hunter2".into(),
        });
        let v = j(action, Some(EffectKind::LocalEdit));
        assert_eq!(v.risk, RiskLevel::R4);
        assert!(v.risk.requires_user_takeover());
    }

    #[test]
    fn secret_shaped_text_forces_r4_even_on_a_plain_field() {
        for text in ["482913", "abc123xyz789"] {
            let action = Action::Semantic(SemanticAction::SetValue {
                element_id: "e3".into(),
                value: text.into(),
            });
            assert_eq!(j(action, Some(EffectKind::LocalEdit)).risk, RiskLevel::R4, "{text}");
        }
    }

    #[test]
    fn plain_local_edit_stays_r2() {
        let action = Action::Semantic(SemanticAction::SetValue {
            element_id: "e3".into(),
            value: "你好LCU".into(),
        });
        assert_eq!(j(action, Some(EffectKind::LocalEdit)).risk, RiskLevel::R2);
    }

    #[test]
    fn search_field_is_not_treated_as_outbound() {
        assert_eq!(j(invoke("e2"), Some(EffectKind::Navigate)).risk, RiskLevel::R1);
    }

    #[test]
    fn unknown_element_is_never_navigation_like() {
        let v = j(invoke("e99"), Some(EffectKind::Navigate));
        assert_eq!(v.risk, RiskLevel::R3);
    }

    #[test]
    fn missing_capability_raises_the_floor() {
        // e3 advertises set_value, not invoke.
        let v = j(invoke("e3"), Some(EffectKind::Navigate));
        assert_eq!(v.risk, RiskLevel::R3);
        assert!(v.model_claim_overridden);
    }

    #[test]
    fn unknown_or_missing_effect_stops_and_asks() {
        assert!(j(invoke("e5"), Some(EffectKind::Unknown)).unknown);
        assert!(j(invoke("e5"), None).unknown);
        // Control actions do not need an effect.
        let done = j(Action::Done { summary: "ok".into() }, None);
        assert!(!done.unknown);
        assert_eq!(done.risk, RiskLevel::R0);
    }

    #[test]
    fn system_back_is_navigation_like() {
        let v = j(
            Action::Semantic(SemanticAction::GlobalBack),
            Some(EffectKind::Navigate),
        );
        assert_eq!(v.risk, RiskLevel::R1);
        assert!(!v.unknown);
    }

    #[test]
    fn coordinates_are_never_silently_harmless() {
        let click = Action::Targeted(TargetedInput::Click {
            x: 0.5,
            y: 0.5,
            button: Default::default(),
        });
        assert_eq!(j(click, Some(EffectKind::Navigate)).risk, RiskLevel::R3);
    }
}
