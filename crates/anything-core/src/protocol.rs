//! External JSON contract and process exit codes.

use serde::{Deserialize, Serialize};

/// Public JSON schema version returned by `lcu doctor --json` and other envelopes.
pub const PROTOCOL_SCHEMA_VERSION: &str = "1.1.0";

/// Stable process exit codes for the `lcu` CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Success = 0,
    WaitingUser = 2,
    TaskFailed = 3,
    PermissionDenied = 4,
    UsageError = 64,
    RuntimeUnavailable = 69,
    InternalError = 70,
}
impl ExitCode {
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

/// High-level JSON status for Agent-facing output.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JsonStatus {
    Ok,
    WaitingUser,
    Failed,
    PermissionDenied,
    Unavailable,
}

/// Versioned JSON envelope used by CLI `--json` outputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonEnvelope<T> {
    pub schema_version: String,
    pub status: JsonStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

impl<T> JsonEnvelope<T> {
    pub fn ok(data: T) -> Self {
        Self {
            schema_version: PROTOCOL_SCHEMA_VERSION.to_string(),
            status: JsonStatus::Ok,
            error: None,
            data: Some(data),
        }
    }

    pub fn err(
        status: JsonStatus,
        code: crate::error::ErrorCode,
        message: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: PROTOCOL_SCHEMA_VERSION.to_string(),
            status,
            error: Some(JsonError {
                code,
                message: message.into(),
            }),
            data: None,
        }
    }

    pub fn waiting(data: T) -> Self {
        Self {
            schema_version: PROTOCOL_SCHEMA_VERSION.to_string(),
            status: JsonStatus::WaitingUser,
            error: None,
            data: Some(data),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JsonError {
    pub code: crate::error::ErrorCode,
    pub message: String,
}

/// Machine-readable environment diagnosis payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DoctorReport {
    pub schema_version: String,
    pub product: String,
    pub platform: String,
    pub arch: String,
    pub runtime_reachable: bool,
    pub private_entry: PrivateEntryStatus,
    pub permissions: Vec<PermissionCheck>,
    pub blockers: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrivateEntryStatus {
    pub kind: String,
    pub listens_tcp: bool,
    pub path: Option<String>,
    pub directory_mode: Option<String>,
    pub socket_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionCheck {
    pub name: String,
    pub state: String,
    pub required_for: Vec<String>,
}

/// Private IPC protocol version between CLI/GUI and Runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternalProtocolVersion(pub u32);

impl InternalProtocolVersion {
    pub const CURRENT: Self = Self(1);

    pub fn is_compatible(self, other: Self) -> bool {
        self.0 == other.0
    }
}
