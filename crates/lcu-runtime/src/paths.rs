//! Per-user runtime directory layout. Never a public network service.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use lcu_core::error::{ErrorCode, LcuError, LcuResult};

/// Locations owned by the desktop app / runtime for the current login user.
#[derive(Debug, Clone)]
pub struct RuntimePaths {
    pub root: PathBuf,
    pub socket: PathBuf,
    pub logs: PathBuf,
}

impl RuntimePaths {
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            socket: root.join("runtime.sock"),
            logs: root.join("logs"),
            root,
        }
    }

    /// Default per-user runtime root.
    ///
    /// `LCU_RUNTIME_ROOT` is the documented override (shared with the Chrome
    /// native-messaging host and install scripts); `LCU_RUNTIME_DIR` remains a
    /// compatible alias used by older local setups. Without either, defaults to
    /// `~/Library/Application Support/AnythingUse`.
    pub fn default_user() -> LcuResult<Self> {
        if let Some(dir) = std::env::var_os("LCU_RUNTIME_ROOT")
            .or_else(|| std::env::var_os("LCU_RUNTIME_DIR"))
        {
            return Ok(Self::from_root(dir));
        }
        let base = dirs::data_dir().ok_or_else(|| {
            LcuError::coded(
                ErrorCode::InternalError,
                "cannot resolve user data directory",
            )
        })?;
        Ok(Self::from_root(base.join("AnythingUse")))
    }

    pub fn ensure_layout(&self) -> LcuResult<()> {
        fs::create_dir_all(&self.root).map_err(|e| {
            LcuError::coded(
                ErrorCode::InternalError,
                format!("create runtime root: {e}"),
            )
        })?;
        fs::create_dir_all(&self.logs).map_err(|e| {
            LcuError::coded(
                ErrorCode::InternalError,
                format!("create runtime logs: {e}"),
            )
        })?;
        set_mode_0700(&self.root)?;
        Ok(())
    }
}

pub fn set_mode_0700(path: &Path) -> LcuResult<()> {
    let mut perms = fs::metadata(path)
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("stat {path:?}: {e}")))?
        .permissions();
    perms.set_mode(0o700);
    fs::set_permissions(path, perms).map_err(|e| {
        LcuError::coded(
            ErrorCode::InternalError,
            format!("chmod 0700 {path:?}: {e}"),
        )
    })?;
    Ok(())
}

pub fn set_mode_0600(path: &Path) -> LcuResult<()> {
    let mut perms = fs::metadata(path)
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("stat {path:?}: {e}")))?
        .permissions();
    perms.set_mode(0o600);
    fs::set_permissions(path, perms).map_err(|e| {
        LcuError::coded(
            ErrorCode::InternalError,
            format!("chmod 0600 {path:?}: {e}"),
        )
    })?;
    Ok(())
}

