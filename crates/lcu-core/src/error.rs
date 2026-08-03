//! Stable error codes shared by CLI JSON output and Runtime.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::protocol::{ExitCode, JsonStatus};

/// Stable, machine-readable error codes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Error)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    #[error("invalid_request")]
    InvalidRequest,
    #[error("usage_error")]
    UsageError,
    #[error("runtime_unavailable")]
    RuntimeUnavailable,
    #[error("protocol_mismatch")]
    ProtocolMismatch,
    #[error("unauthorized_client")]
    UnauthorizedClient,
    #[error("permission_denied")]
    PermissionDenied,
    #[error("waiting_user")]
    WaitingUser,
    #[error("task_failed")]
    TaskFailed,
    #[error("task_not_found")]
    TaskNotFound,
    #[error("approval_required")]
    ApprovalRequired,
    #[error("approval_invalid")]
    ApprovalInvalid,
    #[error("unsupported_capability")]
    UnsupportedCapability,
    #[error("internal_error")]
    InternalError,
    #[error("not_implemented")]
    NotImplemented,
}

impl ErrorCode {
    pub fn exit_code(self) -> ExitCode {
        match self {
            Self::WaitingUser | Self::ApprovalRequired => ExitCode::WaitingUser,
            Self::PermissionDenied | Self::UnauthorizedClient => ExitCode::PermissionDenied,
            Self::TaskFailed
            | Self::TaskNotFound
            | Self::ApprovalInvalid
            | Self::UnsupportedCapability
            | Self::NotImplemented => ExitCode::TaskFailed,
            Self::InvalidRequest | Self::UsageError => ExitCode::UsageError,
            Self::RuntimeUnavailable | Self::ProtocolMismatch => ExitCode::RuntimeUnavailable,
            Self::InternalError => ExitCode::InternalError,
        }
    }

    pub fn json_status(self) -> JsonStatus {
        match self {
            Self::WaitingUser | Self::ApprovalRequired => JsonStatus::WaitingUser,
            Self::PermissionDenied | Self::UnauthorizedClient => JsonStatus::PermissionDenied,
            Self::RuntimeUnavailable | Self::ProtocolMismatch => JsonStatus::Unavailable,
            _ => JsonStatus::Failed,
        }
    }
}

/// Primary error type for shared layers.
#[derive(Debug, Error)]
pub enum LcuError {
    #[error("{code}: {message}")]
    Coded { code: ErrorCode, message: String },

    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

impl LcuError {
    pub fn coded(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Coded {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Coded { code, .. } => *code,
            Self::Other(_) => ErrorCode::InternalError,
        }
    }
}

pub type LcuResult<T> = Result<T, LcuError>;

