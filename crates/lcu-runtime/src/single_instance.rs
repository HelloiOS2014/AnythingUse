//! Single-instance lock for the desktop-owned runtime process.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use lcu_core::error::{ErrorCode, LcuError, LcuResult};

/// Holds an exclusive flock on `{runtime_root}/runtime.lock` for process lifetime.
#[derive(Debug)]
pub struct SingleInstanceLock {
    path: PathBuf,
    file: File,
}

impl SingleInstanceLock {
    pub fn acquire(runtime_root: impl AsRef<Path>) -> LcuResult<Self> {
        let path = runtime_root.as_ref().join("runtime.lock");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&path)
            .map_err(|e| {
                LcuError::coded(
                    ErrorCode::InternalError,
                    format!("open lock {}: {e}", path.display()),
                )
            })?;

        // LOCK_EX | LOCK_NB
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err(LcuError::coded(
                ErrorCode::RuntimeUnavailable,
                format!(
                    "another lcu-desktop runtime already holds {}",
                    path.display()
                ),
            ));
        }

        file.set_len(0).ok();
        writeln!(file, "{}", std::process::id()).ok();
        file.flush().ok();

        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        unsafe {
            let _ = libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

