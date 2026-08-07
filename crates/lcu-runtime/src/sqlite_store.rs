//! SQLite task persistence for M2.

use std::path::Path;

use chrono::{DateTime, Utc};
use lcu_core::error::{ErrorCode, LcuError, LcuResult};
use lcu_core::task::{TaskEvent, TaskId, TaskRecord, TaskState};
use lcu_core::types::CallerIdentity;
use rusqlite::{params, Connection};

pub struct SqliteTaskStore {
    pub(crate) conn: Connection,
}

impl SqliteTaskStore {
    pub fn open(path: impl AsRef<Path>) -> LcuResult<Self> {
        let conn = Connection::open(path)
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("sqlite open: {e}")))?;
        let store = Self { conn };
        store.migrate()?;
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
                  error TEXT
                );
                CREATE TABLE IF NOT EXISTS events (
                  id INTEGER PRIMARY KEY AUTOINCREMENT,
                  task_id TEXT NOT NULL,
                  state TEXT NOT NULL,
                  at TEXT NOT NULL,
                  message TEXT NOT NULL,
                  step INTEGER
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
        Ok(())
    }

    pub fn upsert_task(&self, record: &TaskRecord) -> LcuResult<()> {
        self.conn
            .execute(
                r#"
                INSERT INTO tasks (
                  task_id, goal, state, caller_json, created_at, updated_at,
                  app_selector_json, actor, step_count, last_observation_id, last_action_hash,
                  summary, error
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                ON CONFLICT(task_id) DO UPDATE SET
                  goal=excluded.goal,
                  state=excluded.state,
                  caller_json=excluded.caller_json,
                  updated_at=excluded.updated_at,
                  app_selector_json=excluded.app_selector_json,
                  actor=excluded.actor,
                  step_count=excluded.step_count,
                  last_observation_id=excluded.last_observation_id,
                  last_action_hash=excluded.last_action_hash,
                  summary=excluded.summary,
                  error=excluded.error
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
                    record.step_count as i64,
                    record.last_observation_id.as_ref().map(|o| o.0.clone()),
                    record.last_action_hash.clone(),
                    record.summary.clone(),
                    record.error.clone(),
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
                          app_selector_json, actor, step_count, last_observation_id, last_action_hash,
                          summary, error FROM tasks WHERE task_id=?1"#,
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
                          app_selector_json, actor, step_count, last_observation_id, last_action_hash,
                          summary, error FROM tasks ORDER BY created_at ASC"#,
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
    let step: i64 = row.get(8).map_err(sql_err)?;
    let last_obs: Option<String> = row.get(9).map_err(sql_err)?;
    let last_hash: Option<String> = row.get(10).map_err(sql_err)?;
    let summary: Option<String> = row.get(11).map_err(sql_err)?;
    let error: Option<String> = row.get(12).map_err(sql_err)?;

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
        caller,
        created_at: DateTime::parse_from_rfc3339(&created)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("created: {e}")))?,
        updated_at: DateTime::parse_from_rfc3339(&updated)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| LcuError::coded(ErrorCode::InternalError, format!("updated: {e}")))?,
        app_selector,
        actor,
        step_count: step as u32,
        last_observation_id: last_obs.map(lcu_core::observation::ObservationId),
        last_action_hash: last_hash,
        summary,
        error,
    })
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
    }
}

