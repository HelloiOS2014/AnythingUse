//! Observation and target selection contracts.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::types::Frame;

/// Stable observation identifier for this product session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ObservationId(pub String);

impl ObservationId {
    pub fn new() -> Self {
        Self(format!("obs_{}", Uuid::new_v4()))
    }
}

impl Default for ObservationId {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps model-normalized coordinates to a specific window capture transform.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TransformId(pub String);

impl TransformId {
    pub fn new() -> Self {
        Self(format!("transform_{}", Uuid::new_v4()))
    }
}

impl Default for TransformId {
    fn default() -> Self {
        Self::new()
    }
}

/// How a caller selects a target application.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppSelector {
    /// Bundle ID on macOS; executable/package identity later on Windows.
    pub app_id: Option<String>,
    pub pid: Option<u32>,
    pub window_title_contains: Option<String>,
}

/// Resolved target application/window identity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppTarget {
    pub app_id: String,
    pub pid: u32,
    pub window_id: u64,
    pub window_title: String,
}

/// Normalized element rectangle in [0,1] relative to the model image.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Compact semantic tree node exposed to the model path (not to Agents on stdout).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ElementNode {
    pub id: String,
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub frame: Rect,
    #[serde(default)]
    pub actions: Vec<String>,
}

/// Unified observation produced by a platform backend.
///
/// Image bytes stay in memory and are never part of Agent-facing JSON.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AppObservation {
    pub observation_id: ObservationId,
    pub timestamp_ms: i64,
    pub target: AppTarget,
    pub window_frame: Frame,
    pub model_size: ModelSize,
    pub elements: Vec<ElementNode>,
    pub transform_id: TransformId,
    /// SHA-256 of the in-memory frame when present; never the image itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_hash: Option<String>,
    /// Capture implementation label (e.g. `xcap_cgwindowlist`, `screencapturekit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_backend: Option<String>,
    /// In-process PNG for the product VLM path only. Never Agent IPC JSON.
    #[serde(skip)]
    pub image_png: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSize {
    pub width: u32,
    pub height: u32,
}

impl AppObservation {
    pub fn element_ids(&self) -> impl Iterator<Item = &str> {
        self.elements.iter().map(|e| e.id.as_str())
    }

    pub fn contains_element(&self, element_id: &str) -> bool {
        self.elements.iter().any(|e| e.id == element_id)
    }
}

