//! Primitive shared geometry and identity types.

use serde::{Deserialize, Serialize};

/// Screen or window point in device-independent coordinates when noted.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Axis-aligned rectangle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Who initiated a task or command.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CallerIdentity {
    HumanGui,
    HumanCli,
    Agent { client_id: String, name: String },
}

impl CallerIdentity {
    pub fn is_agent(&self) -> bool {
        matches!(self, Self::Agent { .. })
    }
}
