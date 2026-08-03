//! Task step / duration / frequency limits (M2).

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use lcu_core::error::{ErrorCode, LcuError, LcuResult};

/// Configurable safety limits applied by Runtime before each step.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskLimits {
    pub max_steps: u32,
    pub max_duration: Duration,
    pub min_step_interval: Duration,
    pub max_actions_per_minute: u32,
    /// Conservative global queue depth (active + queued non-terminal tasks).
    pub max_queue_depth: u32,
}

impl Default for TaskLimits {
    fn default() -> Self {
        Self {
            max_steps: 100,
            max_duration: Duration::minutes(30),
            min_step_interval: Duration::milliseconds(50),
            max_actions_per_minute: 120,
            max_queue_depth: 32,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskBudget {
    pub started_at: DateTime<Utc>,
    pub step_count: u32,
    pub last_step_at: Option<DateTime<Utc>>,
    /// Sliding window of action timestamps (newest last).
    pub recent_actions: Vec<DateTime<Utc>>,
    pub limits: TaskLimits,
}

impl TaskBudget {
    pub fn new(limits: TaskLimits) -> Self {
        Self {
            started_at: Utc::now(),
            step_count: 0,
            last_step_at: None,
            recent_actions: Vec::new(),
            limits,
        }
    }

    pub fn check_before_step(&self, now: DateTime<Utc>) -> LcuResult<()> {
        if self.step_count >= self.limits.max_steps {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                format!("step limit exceeded ({})", self.limits.max_steps),
            ));
        }
        if now - self.started_at > self.limits.max_duration {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                "task duration limit exceeded",
            ));
        }
        if let Some(last) = self.last_step_at {
            let elapsed = now - last;
            if elapsed < self.limits.min_step_interval {
                // Wait out the remainder instead of failing the whole task.
                // Product steps (Observe / done-reject retries) often re-enter
                // within a few ms; hard-failing is worse than a short sleep.
                let remaining = self.limits.min_step_interval - elapsed;
                let ms = remaining.num_milliseconds().clamp(1, 2_000) as u64;
                std::thread::sleep(std::time::Duration::from_millis(ms));
            }
        }
        let window_start = now - Duration::minutes(1);
        let recent = self
            .recent_actions
            .iter()
            .filter(|t| **t >= window_start)
            .count() as u32;
        if recent >= self.limits.max_actions_per_minute {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                "actions-per-minute limit exceeded",
            ));
        }
        Ok(())
    }

    pub fn record_step(&mut self, now: DateTime<Utc>) {
        self.step_count += 1;
        self.last_step_at = Some(now);
        self.recent_actions.push(now);
        let window_start = now - Duration::minutes(1);
        self.recent_actions.retain(|t| *t >= window_start);
    }
}

