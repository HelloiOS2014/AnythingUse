//! Private local entry: Unix domain socket only. Never TCP.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::protocol::{InternalProtocolVersion, PrivateEntryStatus};

use crate::paths::{set_mode_0600, set_mode_0700};

#[derive(Debug, Clone)]
pub struct PrivateEntryConfig {
    pub root: PathBuf,
    pub protocol_version: InternalProtocolVersion,
}

/// Prepared private entry metadata. M0 validates permissions; full accept loop is M2.
#[derive(Debug, Clone)]
pub struct PrivateEntry {
    pub root: PathBuf,
    pub socket_path: PathBuf,
    pub protocol_version: InternalProtocolVersion,
    /// Hard invariant: this product never binds a TCP listener for the private entry.
    pub listens_tcp: bool,
}

impl PrivateEntry {
    pub fn prepare(config: PrivateEntryConfig) -> LcuResult<Self> {
        fs::create_dir_all(&config.root).map_err(|e| {
            LcuError::coded(
                ErrorCode::InternalError,
                format!("create private entry root: {e}"),
            )
        })?;
        set_mode_0700(&config.root)?;

        let socket_path = config.root.join("runtime.sock");
        // Remove stale socket file from a previous crash so bind can succeed later.
        if socket_path.exists() {
            let _ = fs::remove_file(&socket_path);
        }

        // Create a placeholder socket file with 0600 so doctor can report mode.
        // Real bind happens when the desktop-owned runtime starts its accept loop (M2).
        fs::File::create(&socket_path).map_err(|e| {
            LcuError::coded(
                ErrorCode::InternalError,
                format!("create socket placeholder: {e}"),
            )
        })?;
        set_mode_0600(&socket_path)?;

        Ok(Self {
            root: config.root,
            socket_path,
            protocol_version: config.protocol_version,
            listens_tcp: false,
        })
    }

    pub fn status(&self) -> PrivateEntryStatus {
        let directory_mode = mode_octal(&self.root);
        let socket_mode = mode_octal(&self.socket_path);
        PrivateEntryStatus {
            kind: "unix_socket".into(),
            listens_tcp: self.listens_tcp,
            path: Some(self.socket_path.display().to_string()),
            directory_mode,
            socket_mode,
        }
    }

    pub fn reject_if_tcp_requested(listen_tcp: bool) -> LcuResult<()> {
        if listen_tcp {
            return Err(LcuError::coded(
                ErrorCode::InvalidRequest,
                "private entry must not listen on TCP",
            ));
        }
        Ok(())
    }
}

fn mode_octal(path: &PathBuf) -> Option<String> {
    fs::metadata(path)
        .ok()
        .map(|m| format!("{:o}", m.permissions().mode() & 0o777))
}

