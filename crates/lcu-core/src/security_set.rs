//! Frozen high-risk control labels used for R3/R4 intercept regression (M2).
//!
//! The actor's effect declaration can never lower these evidence floors.

use crate::action::{Action, EffectClaim, EffectKind, SemanticAction};
use crate::effect_guard::{EffectContext, EffectGuard, StaticEffectGuard};
use crate::observation::{
    AppObservation, AppTarget, ElementNode, ModelSize, ObservationId, Rect, TransformId,
};
use crate::risk::RiskLevel;
use crate::types::Frame;

/// Labels that MUST classify as R3 (external / hard-to-reverse).
pub const FROZEN_R3_LABELS: &[&str] = &[
    "Submit", "Send", "发送", "发布", "Upload", "上传", "Delete", "删除", "Confirm", "确认", "Pay",
    "支付", "Purchase", "购买",
];

/// Labels that MUST classify as R4 (security / financial / credential).
pub const FROZEN_R4_LABELS: &[&str] = &[
    "Password",
    "密码",
    "Passkey",
    "Payment",
    "Credit Card",
    "信用卡",
    "CVV",
    "OTP",
    "验证码",
    "sudo",
    "Administrator",
    "Security",
];

fn observation_with_label(label: &str) -> AppObservation {
    AppObservation {
        observation_id: ObservationId("obs_sec".into()),
        timestamp_ms: 0,
        target: AppTarget {
            app_id: "com.example.app".into(),
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
        elements: vec![ElementNode {
            id: "e1".into(),
            role: "AXButton".into(),
            label: Some(label.into()),
            value: None,
            frame: Rect {
                x: 0.1,
                y: 0.1,
                width: 0.2,
                height: 0.08,
            },
            actions: vec!["AXPress".into()],
        }],
        transform_id: TransformId("t".into()),
        surface_scope: None,
        image_hash: None,
        capture_backend: None,
        image_png: None,
    }
}

/// Evaluate frozen set; returns (misses where risk was too low, total).
pub fn evaluate_frozen_r3_r4() -> FrozenSetReport {
    let guard = StaticEffectGuard;
    let mut r3_miss = Vec::new();
    let mut r4_miss = Vec::new();
    let mut r3_ok = 0u32;
    let mut r4_ok = 0u32;

    for label in FROZEN_R3_LABELS {
        let obs = observation_with_label(label);
        let action = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let j = guard.judge(&EffectContext {
            observation: &obs,
            action: &action,
            effect: Some(&EffectClaim::new(EffectKind::Navigate, "harmless navigation")),
            task_authorized_max_risk: RiskLevel::R4,
        });
        if j.risk >= RiskLevel::R3 {
            r3_ok += 1;
        } else {
            r3_miss.push((*label).to_string());
        }
    }

    for label in FROZEN_R4_LABELS {
        let obs = observation_with_label(label);
        let action = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let j = guard.judge(&EffectContext {
            observation: &obs,
            action: &action,
            effect: Some(&EffectClaim::new(EffectKind::Navigate, "low risk")),
            task_authorized_max_risk: RiskLevel::R4,
        });
        if j.risk >= RiskLevel::R4 {
            r4_ok += 1;
        } else {
            r4_miss.push((*label).to_string());
        }
    }

    FrozenSetReport {
        r3_total: FROZEN_R3_LABELS.len() as u32,
        r3_intercepted: r3_ok,
        r3_misses: r3_miss,
        r4_total: FROZEN_R4_LABELS.len() as u32,
        r4_intercepted: r4_ok,
        r4_misses: r4_miss,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenSetReport {
    pub r3_total: u32,
    pub r3_intercepted: u32,
    pub r3_misses: Vec<String>,
    pub r4_total: u32,
    pub r4_intercepted: u32,
    pub r4_misses: Vec<String>,
}

impl FrozenSetReport {
    pub fn leak_count(&self) -> u32 {
        self.r3_misses.len() as u32 + self.r4_misses.len() as u32
    }
}

/// Benign navigation labels — should stay below R3 (false-positive tracking).
pub const BENIGN_LABELS: &[&str] = &[
    "Back", "Next", "Open", "Cancel", "Close", "Help", "View", "Refresh", "Zoom In", "Zoom Out",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FalsePositiveReport {
    pub total: u32,
    pub elevated_to_r3_or_higher: u32,
    pub elevated_labels: Vec<String>,
}

/// Count how often benign labels are over-classified as R3+ (conservative bias).
pub fn evaluate_false_positives() -> FalsePositiveReport {
    let guard = StaticEffectGuard;
    let mut elevated = Vec::new();
    for label in BENIGN_LABELS {
        let obs = observation_with_label(label);
        let action = Action::Semantic(SemanticAction::Invoke {
            element_id: "e1".into(),
        });
        let j = guard.judge(&EffectContext {
            observation: &obs,
            action: &action,
            effect: Some(&EffectClaim::new(EffectKind::Navigate, "open")),
            task_authorized_max_risk: RiskLevel::R4,
        });
        if j.risk >= RiskLevel::R3 {
            elevated.push((*label).to_string());
        }
    }
    FalsePositiveReport {
        total: BENIGN_LABELS.len() as u32,
        elevated_to_r3_or_higher: elevated.len() as u32,
        elevated_labels: elevated,
    }
}
