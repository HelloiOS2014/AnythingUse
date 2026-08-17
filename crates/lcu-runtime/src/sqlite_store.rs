//! SQLite task persistence for M2.

use std::path::Path;

use chrono::{DateTime, Utc};
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::approval::{AppAccessDecision, AppPermission};
use lcu_core::task::{ControlMode, TaskEvent, TaskId, TaskRecord, TaskState};
use lcu_core::types::CallerIdentity;
use rusqlite::{params, Connection};

pub struct SqliteTaskStore {
    pub(crate) conn: Connection,
}

impl SqliteTaskStore {
    pub fn open(path: impl AsRef<Path>) -> LcuResult<Self> {
        let conn = Connection::open(path)
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("sqlite open: {e}")))?;
        let mut store = Self { conn };
        store.migrate()?;
        store.prune_terminal_tasks(Utc::now(), 1000)?;
        Ok(store)
    }

    pub fn open_in_memory() -> LcuResult<Self> {
        let conn = Connection::open_in_memory().map_err(|e| {
            LcuError::coded(ErrorCode::InternalError, format!("sqlite memory: {e}"))
        })?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> LcuResult<()> {
        self.conn
            .execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS tasks (
                  task_id TEXT PRIMARY KEY,
                  goal TEXT NOT NULL,
                  state TEXT NOT NULL,
                  caller_json TEXT NOT NULL,
                  created_at TEXT NOT NULL,
                  updated_at TEXT NOT NULL,
                  app_selector_json TEXT,
                  actor TEXT,
                  step_count INTEGER NOT NULL,
                  last_observation_id TEXT,
                  last_action_hash TEXT,
                  summary TEXT,
                  error TEXT,
                  wait_reason TEXT
                );
                CREATE TABLE IF NOT EXISTS events (
                  id INTEGER PRIMARY KEY AUTOINCREMENT,
                  task_id TEXT NOT NULL,
                  state TEXT NOT NULL,
                  at TEXT NOT NULL,
                  message TEXT NOT NULL,
                  step INTEGER
                );
                CREATE TABLE IF NOT EXISTS app_permissions (
                  app_key TEXT PRIMARY KEY,
                  decision TEXT NOT NULL,
                  created_at TEXT NOT NULL
                );
                "#,
            )
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("migrate: {e}")))?;
        // Older databases predate the per-task actor column; add it if missing.
        if let Err(e) = self
            .conn
            .execute("ALTER TABLE tasks ADD COLUMN actor TEXT", [])
        {
            tracing::debug!(error = %e, "tasks.actor column already present");
        }
        // Older databases predate the task control_mode column.
        if let Err(e) = self
            .conn
            .execute(
                "ALTER TABLE tasks ADD COLUMN control_mode TEXT NOT NULL DEFAULT 'auto'",
                [],
            )
        {
            tracing::debug!(error = %e, "tasks.control_mode column already present");
        }
        if let Err(e) = self
            .conn
            .execute("ALTER TABLE tasks ADD COLUMN wait_reason TEXT", [])
        {
            tracing::debug!(error = %e, "tasks.wait_reason column already present");
        }
        Ok(())
    }

    /// Startup-only retention: keep terminal tasks only when they are both
    /// within 30 days and among the newest `max_count`. Active tasks are never touched.
    fn prune_terminal_tasks(&mut self, now: DateTime<Utc>, max_count: usize) -> LcuResult<()> {
        let cutoff = (now - chrono::Duration::days(30)).to_rfc3339();
        let terminal = r#"state IN ('"succeeded"','"failed"','"cancelled"')"#;
        let stale = format!(
            "{terminal} AND (updated_at < ?1 OR task_id IN (\
             SELECT task_id FROM tasks WHERE {terminal} \
             ORDER BY updated_at DESC LIMIT -1 OFFSET ?2))"
        );
        let tx = self.conn.transaction().map_err(sql_err)?;
        tx.execute(
            &format!("DELETE FROM events WHERE task_id IN (SELECT task_id FROM tasks WHERE {stale})"),
            params![cutoff, max_count as i64],
        )
        .map_err(sql_err)?;
        tx.execute(
            &format!("DELETE FROM tasks WHERE {stale}"),
            params![cutoff, max_count as i64],
        )
        .map_err(sql_err)?;
        tx.commit().map_err(sql_err)?;
        Ok(())
    }

    pub fn upsert_task(&self, record: &TaskRecord) -> LcuResult<()> {
        self.conn
            .execute(
                r#"
                INSERT INTO tasks (
                  task_id, goal, state, caller_json, created_at, updated_at,
                  app_selector_json, actor, control_mode, step_count, last_observation_id, last_action_hash,
                  summary, error, wait_reason
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)
                ON CONFLICT(task_id) DO UPDATE SET
                  goal=excluded.goal,
                  state=excluded.state,
                  caller_json=excluded.caller_json,
                  updated_at=excluded.updated_at,
                  app_selector_json=excluded.app_selector_json,
                  actor=excluded.actor,
                  control_mode=excluded.control_mode,
                  step_count=excluded.step_count,
                  last_observation_id=excluded.last_observation_id,
                  last_action_hash=excluded.last_action_hash,
                  summary=excluded.summary,
                  error=excluded.error,
                  wait_reason=excluded.wait_reason
                "#,
                params![
                    record.task_id.0,
                    record.goal,
                    serde_json::to_string(&record.state).unwrap(),
                    serde_json::to_string(&record.caller).unwrap(),
                    record.created_at.to_rfc3339(),
                    record.updated_at.to_rfc3339(),
                    record
                        .app_selector
                        .as_ref()
                        .map(|s| serde_json::to_string(s).unwrap()),
                    record.actor.clone(),
                    serde_json::to_string(&record.control_mode).unwrap(),
                    record.step_count as i64,
                    record.last_observation_id.as_ref().map(|o| o.0.clone()),
                    record.last_action_hash.clone(),
                    record.summary.clone(),
                    record.error.clone(),
                    record.wait_reason.map(|reason| serde_json::to_string(&reason).unwrap()),
                ],
            )
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("upsert: {e}")))?;
        Ok(())
    }

    pub fn get_task(&self, task_id: &TaskId) -> LcuResult<Option<TaskRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                r#"SELECT task_id, goal, state, caller_json, created_at, updated_at,
                          app_selector_json, actor, control_mode, step_count, last_observation_id, last_action_hash,
                          summary, error, wait_reason FROM tasks WHERE task_id=?1"#,
            )
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("prepare: {e}")))?;
        let mut rows = stmt
            .query(params![task_id.0])
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("query: {e}")))?;
        if let Some(row) = rows
            .next()
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("row: {e}")))?
        {
            Ok(Some(row_to_task(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn list_tasks(&self) -> LcuResult<Vec<TaskRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                r#"SELECT task_id, goal, state, caller_json, created_at, updated_at,
                          app_selector_json, actor, control_mode, step_count, last_observation_id, last_action_hash,
                          summary, error, wait_reason FROM tasks ORDER BY created_at ASC"#,
            )
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("prepare: {e}")))?;
        let rows = stmt
            .query_map([], |row| Ok(row_to_task(row)))
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("map: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            let rec =
                r.map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("row: {e}")))??;
            out.push(rec);
        }
        Ok(out)
    }

    pub fn push_event(&self, event: &TaskEvent) -> LcuResult<()> {
        self.conn
            .execute(
                r#"INSERT INTO events (task_id, state, at, message, step)
                   VALUES (?1,?2,?3,?4,?5)"#,
                params![
                    event.task_id.0,
                    serde_json::to_string(&event.state).unwrap(),
                    event.at.to_rfc3339(),
                    event.message,
                    event.step.map(|s| s as i64),
                ],
            )
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("event insert: {e}")))?;
        Ok(())
    }

    pub fn events_for(&self, task_id: &TaskId) -> LcuResult<Vec<TaskEvent>> {
        let mut stmt = self
            .conn
            .prepare(
                r#"SELECT task_id, state, at, message, step FROM events
                   WHERE task_id=?1 ORDER BY id ASC"#,
            )
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("prepare: {e}")))?;
        let rows = stmt
            .query_map(params![task_id.0], |row| {
                let task_id: String = row.get(0)?;
                let state_s: String = row.get(1)?;
                let at_s: String = row.get(2)?;
                let message: String = row.get(3)?;
                let step: Option<i64> = row.get(4)?;
                Ok((task_id, state_s, at_s, message, step))
            })
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("map: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            let (task_id, state_s, at_s, message, step) =
                r.map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("row: {e}")))?;
            let state: TaskState = serde_json::from_str(&state_s).map_err(|e| {
                LcuError::coded(ErrorCode::InternalError, format!("state json: {e}"))
            })?;
            let at = DateTime::parse_from_rfc3339(&at_s)
                .map(|d| d.with_timezone(&Utc))
                .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("at: {e}")))?;
            out.push(TaskEvent {
                task_id: TaskId(task_id),
                state,
                at,
                message,
                step: step.map(|s| s as u32),
            });
        }
        Ok(out)
    }
}

fn row_to_task(row: &rusqlite::Row<'_>) -> LcuResult<TaskRecord> {
    let task_id: String = row.get(0).map_err(sql_err)?;
    let goal: String = row.get(1).map_err(sql_err)?;
    let state_s: String = row.get(2).map_err(sql_err)?;
    let caller_s: String = row.get(3).map_err(sql_err)?;
    let created: String = row.get(4).map_err(sql_err)?;
    let updated: String = row.get(5).map_err(sql_err)?;
    let sel_s: Option<String> = row.get(6).map_err(sql_err)?;
    let actor: Option<String> = row.get(7).map_err(sql_err)?;
    let control_mode_s: String = row.get(8).map_err(sql_err)?;
    let step: i64 = row.get(9).map_err(sql_err)?;
    let last_obs: Option<String> = row.get(10).map_err(sql_err)?;
    let last_hash: Option<String> = row.get(11).map_err(sql_err)?;
    let summary: Option<String> = row.get(12).map_err(sql_err)?;
    let error: Option<String> = row.get(13).map_err(sql_err)?;
    let wait_reason_s: Option<String> = row.get(14).map_err(sql_err)?;

    let control_mode: ControlMode = serde_json::from_str(&control_mode_s).or_else(|_| match control_mode_s.as_str() {
        "auto" => Ok(ControlMode::Auto),
        "background_only" => Ok(ControlMode::BackgroundOnly),
        "foreground" | "\"foreground\"" => Ok(ControlMode::Auto),
        _ => Err(serde_json::Error::io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "unknown legacy control_mode",
        ))),
    }).map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("control_mode json: {e}")))?;

    let state: TaskState = serde_json::from_str(&state_s)
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("state json: {e}")))?;
    let caller: CallerIdentity = serde_json::from_str(&caller_s)
        .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("caller json: {e}")))?;
    let app_selector = match sel_s {
        Some(s) => Some(serde_json::from_str(&s).map_err(|e| {
            LcuError::coded(ErrorCode::InternalError, format!("selector json: {e}"))
        })?),
        None => None,
    };
    Ok(TaskRecord {
        task_id: TaskId(task_id),
        goal,
        state,
        wait_reason: wait_reason_s
            .map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("wait_reason json: {e}")))?,
        caller,
        created_at: DateTime::parse_from_rfc3339(&created)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("created: {e}")))?,
        updated_at: DateTime::parse_from_rfc3339(&updated)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("updated: {e}")))?,
        app_selector,
        actor,
        control_mode,
        step_count: step as u32,
        last_observation_id: last_obs.map(lcu_core::observation::ObservationId),
        last_action_hash: last_hash,
        summary,
        error,
    })
}

// ---------------------------------------------------------------------------
// Persistent app permissions (always_allow; revocable via settings)
// ---------------------------------------------------------------------------

impl SqliteTaskStore {
    pub fn list_app_permissions(&self) -> LcuResult<Vec<AppPermission>> {
        let mut stmt = self
            .conn
            .prepare(r#"SELECT app_key, decision, created_at FROM app_permissions"#)
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("prepare: {e}")))?;
        let rows = stmt
            .query_map([], |row| {
                let app_key: String = row.get(0)?;
                let decision: String = row.get(1)?;
                let created: String = row.get(2)?;
                Ok((app_key, decision, created))
            })
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("map: {e}")))?;
        let mut out = Vec::new();
        for r in rows {
            let (app_key, decision, created) =
                r.map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("row: {e}")))?;
            let decision: AppAccessDecision = serde_json::from_str(&decision).map_err(|e| {
                LcuError::coded(ErrorCode::InternalError, format!("decision json: {e}"))
            })?;
            let created_at = DateTime::parse_from_rfc3339(&created)
                .map(|d| d.with_timezone(&Utc))
                .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("created: {e}")))?;
            out.push(AppPermission {
                app_key,
                decision,
                created_at,
            });
        }
        Ok(out)
    }

    pub fn upsert_app_permission(&self, permission: &AppPermission) -> LcuResult<()> {
        self.conn
            .execute(
                r#"INSERT INTO app_permissions (app_key, decision, created_at)
                   VALUES (?1,?2,?3)
                   ON CONFLICT(app_key) DO UPDATE SET
                     decision=excluded.decision,
                     created_at=excluded.created_at"#,
                params![
                    permission.app_key,
                    serde_json::to_string(&permission.decision).unwrap(),
                    permission.created_at.to_rfc3339(),
                ],
            )
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("perm upsert: {e}")))?;
        Ok(())
    }

    pub fn delete_app_permission(&self, app_key: &str) -> LcuResult<bool> {
        self.conn
            .execute(
                "DELETE FROM app_permissions WHERE app_key = ?1",
                params![app_key],
            )
            .map(|changed| changed > 0)
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("perm delete: {e}")))
    }
}

fn sql_err(e: rusqlite::Error) -> LcuError {
    LcuError::coded(ErrorCode::InternalError, format!("sqlite: {e}"))
}

#[cfg(test)]
mod tests {

    use super::*;
    use lcu_core::types::CallerIdentity;

    
    #[test]
    fn sqlite_roundtrip_task() {
        let store = SqliteTaskStore::open_in_memory().unwrap();
        let mut rec = TaskRecord::new("demo", CallerIdentity::HumanCli, None);
        store.upsert_task(&rec).unwrap();
        let got = store.get_task(&rec.task_id).unwrap().unwrap();
        assert_eq!(got.goal, "demo");
        rec.step_count = 3;
        store.upsert_task(&rec).unwrap();
        let got = store.get_task(&rec.task_id).unwrap().unwrap();
        assert_eq!(got.step_count, 3);
        assert_eq!(store.list_tasks().unwrap().len(), 1);

        store.conn.execute(
            "UPDATE tasks SET control_mode='auto' WHERE task_id=?1",
            params![rec.task_id.0],
        ).unwrap();
        assert_eq!(store.get_task(&rec.task_id).unwrap().unwrap().control_mode, ControlMode::Auto);
    }

    #[test]
    fn startup_prunes_only_old_or_excess_terminal_tasks() {
        let mut store = SqliteTaskStore::open_in_memory().unwrap();
        let now = Utc::now();
        for (name, age_days, state) in [
            ("active-old", 90, TaskState::Running),
            ("terminal-old", 40, TaskState::Succeeded),
            ("terminal-3", 3, TaskState::Failed),
            ("terminal-2", 2, TaskState::Cancelled),
            ("terminal-1", 1, TaskState::Succeeded),
        ] {
            let mut rec = TaskRecord::new(name, CallerIdentity::HumanCli, None);
            rec.state = state;
            rec.updated_at = now - chrono::Duration::days(age_days);
            store.upsert_task(&rec).unwrap();
            store.push_event(&TaskEvent {
                task_id: rec.task_id,
                state,
                at: rec.updated_at,
                message: name.into(),
                step: None,
            }).unwrap();
        }

        store.prune_terminal_tasks(now, 2).unwrap();
        let goals = store.list_tasks().unwrap().into_iter().map(|task| task.goal).collect::<Vec<_>>();
        assert_eq!(goals, vec!["active-old", "terminal-2", "terminal-1"]);
        let event_count: i64 = store.conn.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0)).unwrap();
        assert_eq!(event_count, 3);
    }
}
