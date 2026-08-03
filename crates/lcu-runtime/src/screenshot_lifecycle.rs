//! Screenshot lifecycle: temp storage, TTL, and cleanup (never SQLite).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use lcu_core::error::{ErrorCode, LcuError, LcuResult};

/// Metadata for a captured frame stored outside the task DB.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScreenshotRecord {
    pub id: String,
    pub path: PathBuf,
    pub task_id: Option<String>,
    pub observation_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ScreenshotStore {
    root: PathBuf,
    default_ttl: Duration,
}

impl ScreenshotStore {
    pub fn open(root: impl Into<PathBuf>, default_ttl: Duration) -> LcuResult<Self> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|e| {
            LcuError::coded(ErrorCode::InternalError, format!("screenshot dir: {e}"))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&root, fs::Permissions::from_mode(0o700));
        }
        Ok(Self { root, default_ttl })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn save_png(
        &self,
        bytes: &[u8],
        task_id: Option<&str>,
        observation_id: Option<&str>,
        ttl: Option<Duration>,
    ) -> LcuResult<ScreenshotRecord> {
        let id = format!("shot_{}", Uuid::new_v4());
        let path = self.root.join(format!("{id}.png"));
        fs::write(&path, bytes).map_err(|e| {
            LcuError::coded(ErrorCode::InternalError, format!("write screenshot: {e}"))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
        let now = Utc::now();
        let ttl = ttl.unwrap_or(self.default_ttl);
        Ok(ScreenshotRecord {
            id,
            path,
            task_id: task_id.map(str::to_string),
            observation_id: observation_id.map(str::to_string),
            created_at: now,
            expires_at: now + ttl,
        })
    }

    /// Delete a screenshot file if present.
    pub fn delete(&self, record: &ScreenshotRecord) -> LcuResult<()> {
        if record.path.exists() {
            fs::remove_file(&record.path).map_err(|e| {
                LcuError::coded(ErrorCode::InternalError, format!("delete screenshot: {e}"))
            })?;
        }
        Ok(())
    }

    /// Remove files older than TTL based on filename mtime fallback scan.
    pub fn cleanup_expired(&self, now: DateTime<Utc>) -> LcuResult<usize> {
        let mut removed = 0usize;
        let entries = fs::read_dir(&self.root).map_err(|e| {
            LcuError::coded(ErrorCode::InternalError, format!("read screenshots: {e}"))
        })?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("png") {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let modified = meta.modified().ok().and_then(|t| {
                DateTime::<Utc>::from_timestamp(
                    t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64,
                    0,
                )
            });
            let Some(modified) = modified else { continue };
            if now - modified > self.default_ttl && fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Sleep-friendly helper for tests: default TTL for production is hours.
    pub fn default_ttl(&self) -> Duration {
        self.default_ttl
    }
}

/// Convert chrono Duration to std for wait helpers (tests).
pub fn chrono_to_std(d: Duration) -> StdDuration {
    StdDuration::from_secs(d.num_seconds().max(0) as u64)
}

