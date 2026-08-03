//! Visual stability / repeat-action / ping-pong loop detection (M4).

use lcu_core::action::Action;
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopGuardConfig {
    pub max_identical_actions: u32,
    pub max_ping_pong_pairs: u32,
}

impl Default for LoopGuardConfig {
    fn default() -> Self {
        Self {
            // General actions: allow one retry then trip (3 identical).
            // Observe is special-cased in record_and_check to fail on the 2nd.
            max_identical_actions: 3,
            max_ping_pong_pairs: 3,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct LoopGuard {
    pub config: LoopGuardConfig,
    history: Vec<String>,
}

impl LoopGuard {
    pub fn new(config: LoopGuardConfig) -> Self {
        Self {
            config,
            history: Vec::new(),
        }
    }

    pub fn record_and_check(&mut self, action: &Action) -> LcuResult<()> {
        let key = action.action_hash();
        // Empty re-observe must die immediately: second consecutive Observe fails.
        // Product loop already has a fresh observation every step.
        if matches!(action, Action::Observe) {
            if self.history.last().is_some_and(|h| h == &key) {
                return Err(LcuError::coded(
                    ErrorCode::InvalidRequest,
                    "repeat observe loop detected (consecutive observe)",
                ));
            }
        }
        self.history.push(key);
        self.check_identical()?;
        self.check_ping_pong()?;
        Ok(())
    }

    fn check_identical(&self) -> LcuResult<()> {
        let n = self.config.max_identical_actions as usize;
        if self.history.len() < n {
            return Ok(());
        }
        let slice = &self.history[self.history.len() - n..];
        if slice.iter().all(|h| h == &slice[0]) {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                format!("repeat action loop detected ({n} identical hashes)"),
            ));
        }
        Ok(())
    }

    fn check_ping_pong(&self) -> LcuResult<()> {
        let pairs = self.config.max_ping_pong_pairs as usize;
        // Need 2*pairs entries alternating A B A B ...
        let need = pairs * 2;
        if self.history.len() < need {
            return Ok(());
        }
        let slice = &self.history[self.history.len() - need..];
        let a = &slice[0];
        let b = &slice[1];
        if a == b {
            return Ok(());
        }
        let mut ok = true;
        for (i, h) in slice.iter().enumerate() {
            let expect = if i % 2 == 0 { a } else { b };
            if h != expect {
                ok = false;
                break;
            }
        }
        if ok {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                "ping-pong action loop detected",
            ));
        }
        Ok(())
    }

    pub fn clear(&mut self) {
        self.history.clear();
    }
}

/// Reject Done without verifiable evidence (M4 gate).
pub fn require_done_evidence(summary: &str, evidence: Option<&str>) -> LcuResult<()> {
    let summary_ok = summary.trim().len() >= 8;
    let evidence_ok = evidence.map(|e| e.trim().len() >= 4).unwrap_or(false);
    if summary_ok && evidence_ok {
        Ok(())
    } else {
        Err(LcuError::coded(
            ErrorCode::InvalidRequest,
            "done requires summary (>=8 chars) and verifiable evidence",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcu_core::action::Action;

    #[test]
    fn two_identical_observes_trip_guard() {
        let mut g = LoopGuard::new(LoopGuardConfig::default());
        assert!(g.record_and_check(&Action::Observe).is_ok());
        let err = g.record_and_check(&Action::Observe).unwrap_err();
        assert!(
            err.to_string().contains("repeat observe"),
            "expected consecutive observe detect, got {err}"
        );
    }

    #[test]
    fn two_identical_waits_do_not_trip_yet() {
        // General max_identical is 3; second wait is still allowed.
        let mut g = LoopGuard::new(LoopGuardConfig::default());
        let wait = Action::Wait {
            milliseconds: 500,
        };
        assert!(g.record_and_check(&wait).is_ok());
        assert!(g.record_and_check(&wait).is_ok());
        let err = g.record_and_check(&wait).unwrap_err();
        assert!(err.to_string().contains("repeat action"));
    }

    #[test]
    fn different_actions_do_not_trip_on_two() {
        let mut g = LoopGuard::new(LoopGuardConfig::default());
        assert!(g.record_and_check(&Action::Observe).is_ok());
        assert!(g
            .record_and_check(&Action::Wait {
                milliseconds: 500
            })
            .is_ok());
        assert!(g.record_and_check(&Action::Observe).is_ok());
    }
}

