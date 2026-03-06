use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use chrono::{DateTime, Utc};
use remoterm_proto::{SessionStatus, SessionSummary};
use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::SessionConfig;

#[derive(Debug, Clone)]
pub struct PersistedSession {
    pub config: SessionConfig,
    pub summary: SessionSummary,
}

#[derive(Debug, Clone)]
pub struct PersistedOutputEvent {
    pub seq: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Storage {
    path: PathBuf,
}

impl Storage {
    pub fn open(path: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create database directory {}", parent.display())
                })?;
            }
        }

        let storage = Self { path };
        storage.init()?;
        Ok(storage)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_sessions(&self) -> anyhow::Result<Vec<PersistedSession>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, cwd, shell, args_json, status, pid, exit_code, archived, \
                    created_at, updated_at, last_activity_at \
             FROM sessions \
             ORDER BY created_at ASC",
        )?;

        let mut rows = stmt.query([])?;
        let mut sessions = Vec::new();
        while let Some(row) = rows.next()? {
            let id = parse_uuid(row.get::<_, String>(0)?).context("invalid session id")?;
            let name: String = row.get(1)?;
            let cwd: String = row.get(2)?;
            let shell: String = row.get(3)?;
            let args_json: String = row.get(4)?;
            let args: Vec<String> = serde_json::from_str(&args_json)
                .with_context(|| format!("invalid args_json for session {}", id))?;
            let status = parse_status(&row.get::<_, String>(5)?)
                .with_context(|| format!("invalid status for session {}", id))?;
            let pid = row.get::<_, Option<u32>>(6)?;
            let exit_code = row.get::<_, Option<u32>>(7)?;
            let archived = row.get::<_, i64>(8)? != 0;
            let created_at = parse_timestamp(&row.get::<_, String>(9)?)
                .with_context(|| format!("invalid created_at for session {}", id))?;
            let updated_at = parse_timestamp(&row.get::<_, String>(10)?)
                .with_context(|| format!("invalid updated_at for session {}", id))?;
            let last_activity_at = parse_timestamp(&row.get::<_, String>(11)?)
                .with_context(|| format!("invalid last_activity_at for session {}", id))?;

            let config = SessionConfig {
                name: name.clone(),
                cwd: cwd.clone(),
                shell: shell.clone(),
                args: args.clone(),
            };

            let summary = SessionSummary {
                id,
                name,
                cwd,
                shell,
                args,
                status,
                pid,
                exit_code,
                archived,
                attached_clients: 0,
                last_activity_at,
                created_at,
                updated_at,
            };

            sessions.push(PersistedSession { config, summary });
        }

        Ok(sessions)
    }

    pub fn upsert_session(
        &self,
        summary: &SessionSummary,
        config: &SessionConfig,
    ) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO sessions (
                id, name, cwd, shell, args_json, status, pid, exit_code, archived,
                created_at, updated_at, last_activity_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                cwd = excluded.cwd,
                shell = excluded.shell,
                args_json = excluded.args_json,
                status = excluded.status,
                pid = excluded.pid,
                exit_code = excluded.exit_code,
                archived = excluded.archived,
                updated_at = excluded.updated_at,
                last_activity_at = excluded.last_activity_at",
            params![
                summary.id.to_string(),
                &config.name,
                &config.cwd,
                &config.shell,
                serde_json::to_string(&config.args).context("failed to encode session args")?,
                status_to_str(&summary.status),
                summary.pid,
                summary.exit_code,
                summary.archived,
                summary.created_at.to_rfc3339(),
                summary.updated_at.to_rfc3339(),
                summary.last_activity_at.to_rfc3339(),
            ],
        )?;

        Ok(())
    }

    pub fn delete_session(&self, id: Uuid) -> anyhow::Result<()> {
        let conn = self.connect()?;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM session_events WHERE session_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute("DELETE FROM sessions WHERE id = ?1", params![id.to_string()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn load_output_history(&self, session_id: Uuid) -> anyhow::Result<Vec<PersistedOutputEvent>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT seq, payload
             FROM session_events
             WHERE session_id = ?1
             ORDER BY seq ASC",
        )?;
        let mut rows = stmt.query(params![session_id.to_string()])?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            events.push(PersistedOutputEvent {
                seq: row.get(0)?,
                data: row.get(1)?,
            });
        }
        Ok(events)
    }

    pub fn append_output(
        &self,
        session_id: Uuid,
        event: &PersistedOutputEvent,
        max_bytes: usize,
        activity_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO session_events (session_id, seq, payload, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                session_id.to_string(),
                event.seq,
                &event.data,
                activity_at.to_rfc3339(),
            ],
        )?;
        tx.execute(
            "UPDATE sessions
             SET updated_at = ?2, last_activity_at = ?2
             WHERE id = ?1",
            params![session_id.to_string(), activity_at.to_rfc3339()],
        )?;
        prune_history_in_tx(&tx, session_id, max_bytes)?;
        tx.commit()?;
        Ok(())
    }

    pub fn clear_output_history(&self, session_id: Uuid) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute(
            "DELETE FROM session_events WHERE session_id = ?1",
            params![session_id.to_string()],
        )?;
        Ok(())
    }

    fn init(&self) -> anyhow::Result<()> {
        let conn = self.connect()?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS sessions (
                 id TEXT PRIMARY KEY,
                 name TEXT NOT NULL,
                 cwd TEXT NOT NULL,
                 shell TEXT NOT NULL,
                 args_json TEXT NOT NULL,
                 status TEXT NOT NULL,
                 pid INTEGER,
                 exit_code INTEGER,
                 archived INTEGER NOT NULL DEFAULT 0,
                 created_at TEXT NOT NULL,
                 updated_at TEXT NOT NULL,
                 last_activity_at TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS session_events (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 session_id TEXT NOT NULL,
                 seq INTEGER NOT NULL,
                 payload BLOB NOT NULL,
                 created_at TEXT NOT NULL
             );
             CREATE UNIQUE INDEX IF NOT EXISTS idx_session_events_session_seq
                 ON session_events(session_id, seq);",
        )?;
        add_column_if_missing(
            &conn,
            "sessions",
            "archived",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        Ok(())
    }

    fn connect(&self) -> anyhow::Result<Connection> {
        let conn = Connection::open(&self.path)
            .with_context(|| format!("failed to open sqlite database {}", self.path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        Ok(conn)
    }
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> anyhow::Result<()> {
    if table_has_column(conn, table, column)? {
        return Ok(());
    }
    conn.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {definition};"
    ))?;
    Ok(())
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn prune_history_in_tx(
    tx: &rusqlite::Transaction<'_>,
    session_id: Uuid,
    max_bytes: usize,
) -> anyhow::Result<()> {
    if max_bytes == 0 {
        tx.execute(
            "DELETE FROM session_events WHERE session_id = ?1",
            params![session_id.to_string()],
        )?;
        return Ok(());
    }

    let total_bytes: i64 = tx.query_row(
        "SELECT COALESCE(SUM(length(payload)), 0)
         FROM session_events
         WHERE session_id = ?1",
        params![session_id.to_string()],
        |row| row.get(0),
    )?;
    if total_bytes <= max_bytes as i64 {
        return Ok(());
    }

    let mut to_remove = Vec::new();
    let mut bytes_to_remove = total_bytes - max_bytes as i64;
    let mut stmt = tx.prepare(
        "SELECT id, length(payload)
         FROM session_events
         WHERE session_id = ?1
         ORDER BY seq ASC",
    )?;
    let mut rows = stmt.query(params![session_id.to_string()])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let len: i64 = row.get(1)?;
        to_remove.push(id);
        bytes_to_remove -= len;
        if bytes_to_remove <= 0 {
            break;
        }
    }
    drop(rows);
    drop(stmt);

    for id in to_remove {
        tx.execute("DELETE FROM session_events WHERE id = ?1", params![id])?;
    }
    Ok(())
}

fn parse_uuid(value: String) -> anyhow::Result<Uuid> {
    Uuid::parse_str(&value).with_context(|| format!("invalid uuid {}", value))
}

fn parse_timestamp(value: &str) -> anyhow::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|ts| ts.with_timezone(&Utc))
        .with_context(|| format!("invalid rfc3339 timestamp {}", value))
}

fn parse_status(value: &str) -> anyhow::Result<SessionStatus> {
    match value {
        "running" => Ok(SessionStatus::Running),
        "exited" => Ok(SessionStatus::Exited),
        "starting" => Ok(SessionStatus::Starting),
        "stopped" => Ok(SessionStatus::Stopped),
        other => anyhow::bail!("unsupported session status {}", other),
    }
}

fn status_to_str(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Running => "running",
        SessionStatus::Exited => "exited",
        SessionStatus::Starting => "starting",
        SessionStatus::Stopped => "stopped",
    }
}
