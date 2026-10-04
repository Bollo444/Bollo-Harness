//! Durable store: SQLite (WAL, `synchronous=FULL`, foreign keys) for the event
//! journal, runs and the operations ledger, plus a content-addressed artifact
//! directory.
//!
//! Durability rules (docs/architecture/data-model.md, runtime.md):
//! - `seq` strictly increases per session; `(session_id, seq)` is the key;
//! - an operation is persisted `started` *before* its effect is initiated;
//! - on restart, stale `prepared`/`started` operations become `unknown` and are
//!   never automatically replayed.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use bollo_protocol::ids::{ArtifactId, RunId, SessionId};
use bollo_protocol::timeutil;
use bollo_protocol::vocab::{TerminalState, VerificationStatus};
use bollo_protocol::{EventEnvelope, EventType, ProtocolError};

pub const SCHEMA_VERSION: i64 = 2;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(String),
    #[error("serialization: {0}")]
    Serialization(String),
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("io: {0}")]
    Io(String),
    #[error("not found: {0}")]
    NotFound(String),
}

impl From<rusqlite::Error> for StoreError {
    fn from(err: rusqlite::Error) -> Self {
        StoreError::Sqlite(err.to_string())
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(err: serde_json::Error) -> Self {
        StoreError::Serialization(err.to_string())
    }
}

impl From<ProtocolError> for StoreError {
    fn from(err: ProtocolError) -> Self {
        StoreError::Protocol(err.to_string())
    }
}

impl From<std::io::Error> for StoreError {
    fn from(err: std::io::Error) -> Self {
        StoreError::Io(err.to_string())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Prepared,
    Started,
    Succeeded,
    Failed,
    Unknown,
}

impl OperationState {
    pub fn as_str(self) -> &'static str {
        match self {
            OperationState::Prepared => "prepared",
            OperationState::Started => "started",
            OperationState::Succeeded => "succeeded",
            OperationState::Failed => "failed",
            OperationState::Unknown => "unknown",
        }
    }

    fn parse(value: &str) -> OperationState {
        match value {
            "prepared" => OperationState::Prepared,
            "started" => OperationState::Started,
            "succeeded" => OperationState::Succeeded,
            "failed" => OperationState::Failed,
            _ => OperationState::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    pub tool_call_id: String,
    pub tool_name: String,
    pub intent_hash: String,
    pub state: OperationState,
    pub result_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRef {
    pub id: ArtifactId,
    pub hash: String,
    pub bytes: u64,
    pub media_type: String,
    pub relative_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub id: RunId,
    pub session_id: SessionId,
    pub state: String,
    pub policy_revision: u64,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub workspace_identity: String,
    pub created_at: String,
    pub last_seq: u64,
    pub label: Option<String>,
}

/// A durable patch checkpoint: captured preimage, recorded postimage and the
/// restore marker. Restores are conditional on the postimage still matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointRecord {
    pub id: String,
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub path: String,
    pub existed: bool,
    pub pre_sha256: Option<String>,
    pub post_sha256: Option<String>,
    pub pre_text: Option<String>,
    pub captured_at: String,
    pub restored_at: Option<String>,
}

pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    /// Open (creating if needed) a database file.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::configure(&conn, true)?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// In-memory database for tests and ephemeral runs.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn, false)?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn configure(conn: &Connection, wal: bool) -> Result<(), StoreError> {
        if wal {
            let _ = conn.pragma_update(None, "journal_mode", "WAL");
        }
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    /// Forward-only, checksum-recorded migrations. Each migration is applied
    /// with its own recorded checksum, so an N-1 database upgrades in place.
    pub fn migrate(&mut self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations(
                 version INTEGER PRIMARY KEY,
                 checksum TEXT NOT NULL,
                 applied_at TEXT NOT NULL
             );",
        )?;
        let current: Option<i64> = self.conn.query_row(
            "SELECT MAX(version) FROM schema_migrations",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        let current = current.unwrap_or(0);
        for (version, migration) in MIGRATIONS {
            if *version <= current {
                continue;
            }
            let checksum = hex::encode(Sha256::digest(migration.as_bytes()));
            let tx = self.conn.transaction()?;
            tx.execute_batch(migration)?;
            tx.execute(
                "INSERT INTO schema_migrations(version, checksum, applied_at) VALUES (?1, ?2, ?3)",
                params![version, checksum, timeutil::now_rfc3339()],
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn create_session(
        &mut self,
        workspace_identity: &str,
        label: Option<&str>,
    ) -> Result<SessionId, StoreError> {
        let session = SessionId::generate();
        let now = timeutil::now_rfc3339();
        self.conn.execute(
            "INSERT INTO sessions(id, workspace_identity, created_at, last_seq, label)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![session.as_str(), workspace_identity, now, label],
        )?;
        Ok(session)
    }

    pub fn last_seq(&self, session: &SessionId) -> Result<u64, StoreError> {
        let seq: Option<u64> = self
            .conn
            .query_row(
                "SELECT last_seq FROM sessions WHERE id = ?1",
                params![session.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        seq.ok_or_else(|| StoreError::NotFound(format!("session {session}")))
    }

    /// Append one durable event, allocating the session's next `seq`.
    pub fn append_event(
        &mut self,
        session: &SessionId,
        run: Option<&RunId>,
        event_type: EventType,
        data: &Value,
    ) -> Result<EventEnvelope, StoreError> {
        let seq = self.last_seq(session)? + 1;
        let envelope = EventEnvelope::new(session, run, seq, event_type, data)?;
        let payload = serde_json::to_string(&envelope)?;
        let run_id = envelope.run_id.as_ref().map(|id| id.as_str());
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO events(session_id, seq, event_id, run_id, type, timestamp, envelope_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                session.as_str(),
                seq as i64,
                envelope.event_id.as_str(),
                run_id,
                envelope.event_type.as_str(),
                envelope.timestamp,
                payload
            ],
        )?;
        tx.execute(
            "UPDATE sessions SET last_seq = ?1 WHERE id = ?2",
            params![seq as i64, session.as_str()],
        )?;
        tx.commit()?;
        Ok(envelope)
    }

    /// Replay events after a cursor; clients deduplicate by `event_id`.
    pub fn replay(&self, session: &SessionId, after: u64) -> Result<Vec<EventEnvelope>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT envelope_json FROM events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq ASC",
        )?;
        let rows = statement.query_map(params![session.as_str(), after as i64], |row| {
            let json: String = row.get(0)?;
            Ok(json)
        })?;
        let mut events = Vec::new();
        for row in rows {
            let json = row?;
            let envelope: EventEnvelope =
                serde_json::from_str(&json).map_err(|err| StoreError::Serialization(err.to_string()))?;
            events.push(envelope);
        }
        Ok(events)
    }

    pub fn start_run(
        &mut self,
        session: &SessionId,
        provider: &str,
        model: &str,
        policy_revision: u64,
    ) -> Result<RunId, StoreError> {
        let run = RunId::generate();
        self.conn.execute(
            "INSERT INTO runs(id, session_id, state, policy_revision, model_id, started_at, provider)
             VALUES (?1, ?2, 'running', ?3, ?4, ?5, ?6)",
            params![
                run.as_str(),
                session.as_str(),
                policy_revision as i64,
                model,
                timeutil::now_rfc3339(),
                provider
            ],
        )?;
        Ok(run)
    }

    pub fn finish_run(
        &mut self,
        run: &RunId,
        state: TerminalState,
        _reason: Option<&str>,
        _verification: VerificationStatus,
    ) -> Result<(), StoreError> {
        let state_text = serde_json::to_value(state)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "failed".into());
        let updated = self.conn.execute(
            "UPDATE runs SET state = ?1, ended_at = ?2 WHERE id = ?3",
            params![state_text, timeutil::now_rfc3339(), run.as_str()],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound(format!("run {run}")));
        }
        Ok(())
    }

    /// Persist the intent *before* any effect is initiated.
    pub fn record_operation_intent(
        &mut self,
        run: &RunId,
        tool_call_id: &str,
        tool_name: &str,
        intent_hash: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO operations(run_id, tool_call_id, tool_name, intent_hash, state, updated_at)
             VALUES (?1, ?2, ?3, ?4, 'started', ?5)",
            params![
                run.as_str(),
                tool_call_id,
                tool_name,
                intent_hash,
                timeutil::now_rfc3339()
            ],
        )?;
        Ok(())
    }

    pub fn record_operation_result(
        &mut self,
        run: &RunId,
        tool_call_id: &str,
        state: OperationState,
        result_ref: Option<&str>,
    ) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE operations SET state = ?1, result_ref = ?2, updated_at = ?3
             WHERE run_id = ?4 AND tool_call_id = ?5",
            params![
                state.as_str(),
                result_ref,
                timeutil::now_rfc3339(),
                run.as_str(),
                tool_call_id
            ],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound(format!(
                "operation {tool_call_id} of run {run}"
            )));
        }
        Ok(())
    }

    pub fn operations_for_run(&self, run: &RunId) -> Result<Vec<OperationRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT tool_call_id, tool_name, intent_hash, state, result_ref
             FROM operations WHERE run_id = ?1 ORDER BY id ASC",
        )?;
        let rows = statement.query_map(params![run.as_str()], |row| {
            let state: String = row.get(3)?;
            Ok(OperationRecord {
                tool_call_id: row.get(0)?,
                tool_name: row.get(1)?,
                intent_hash: row.get(2)?,
                state: OperationState::parse(&state),
                result_ref: row.get(4)?,
            })
        })?;
        let mut operations = Vec::new();
        for row in rows {
            operations.push(row?);
        }
        Ok(operations)
    }

    /// Every operation recorded for a session, in journal order. Used by
    /// recovery inspection before a session is resumed.
    pub fn operations_for_session(
        &self,
        session: &SessionId,
    ) -> Result<Vec<OperationRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT o.tool_call_id, o.tool_name, o.intent_hash, o.state, o.result_ref
             FROM operations o JOIN runs r ON r.id = o.run_id
             WHERE r.session_id = ?1 ORDER BY o.id ASC",
        )?;
        let rows = statement.query_map(params![session.as_str()], |row| {
            let state: String = row.get(3)?;
            Ok(OperationRecord {
                tool_call_id: row.get(0)?,
                tool_name: row.get(1)?,
                intent_hash: row.get(2)?,
                state: OperationState::parse(&state),
                result_ref: row.get(4)?,
            })
        })?;
        let mut operations = Vec::new();
        for row in rows {
            operations.push(row?);
        }
        Ok(operations)
    }

    /// Crash recovery: every `prepared`/`started` operation becomes `unknown`
    /// and is never auto-replayed.
    pub fn mark_stale_operations_unknown(&mut self) -> Result<usize, StoreError> {
        let updated = self.conn.execute(
            "UPDATE operations SET state = 'unknown', updated_at = ?1
             WHERE state IN ('prepared', 'started')",
            params![timeutil::now_rfc3339()],
        )?;
        Ok(updated)
    }

    /// Nonterminal runs left behind by a crash.
    pub fn interrupted_runs(&self, session: &SessionId) -> Result<Vec<RunRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, session_id, state, policy_revision, model_id FROM runs
             WHERE session_id = ?1 AND state IN ('running', 'waiting_approval', 'queued')",
        )?;
        let rows = statement.query_map(params![session.as_str()], |row| {
            let id: String = row.get(0)?;
            let session_id: String = row.get(1)?;
            Ok(RunRecord {
                id: RunId::parse(id).unwrap_or_else(|_| RunId::generate()),
                session_id: SessionId::parse(session_id)
                    .unwrap_or_else(|_| SessionId::generate()),
                state: row.get(2)?,
                policy_revision: row.get::<_, i64>(3)? as u64,
                model_id: row.get(4)?,
            })
        })?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?);
        }
        Ok(runs)
    }

    /// Local session metadata for `bollo sessions list`. Reads metadata only;
    /// no workspace content is touched.
    pub fn list_sessions(&self) -> Result<Vec<SessionRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, workspace_identity, created_at, last_seq, label
             FROM sessions ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map([], |row| {
            let id: String = row.get(0)?;
            Ok(SessionRecord {
                id: SessionId::parse(id).unwrap_or_else(|_| SessionId::generate()),
                workspace_identity: row.get(1)?,
                created_at: row.get(2)?,
                last_seq: row.get::<_, i64>(3)? as u64,
                label: row.get(4)?,
            })
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(row?);
        }
        Ok(sessions)
    }

    /// Delete local session state (events, operations, runs, checkpoints).
    /// Source files are never touched by this operation.
    pub fn delete_session(&mut self, session: &SessionId) -> Result<usize, StoreError> {
        let tx = self.conn.transaction()?;
        let mut deleted = 0usize;
        deleted += tx.execute(
            "DELETE FROM operations WHERE run_id IN (SELECT id FROM runs WHERE session_id = ?1)",
            params![session.as_str()],
        )?;
        deleted += tx.execute(
            "DELETE FROM checkpoints WHERE session_id = ?1",
            params![session.as_str()],
        )?;
        deleted += tx.execute(
            "DELETE FROM events WHERE session_id = ?1",
            params![session.as_str()],
        )?;
        deleted += tx.execute(
            "DELETE FROM runs WHERE session_id = ?1",
            params![session.as_str()],
        )?;
        deleted += tx.execute(
            "DELETE FROM sessions WHERE id = ?1",
            params![session.as_str()],
        )?;
        tx.commit()?;
        Ok(deleted)
    }

    /// Persist a captured checkpoint. Re-recording is idempotent and never
    /// clears a previous restore marker.
    pub fn record_checkpoint(&mut self, record: &CheckpointRecord) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO checkpoints(id, session_id, run_id, path, existed, pre_sha256,
                                    post_sha256, pre_text, captured_at, restored_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)
             ON CONFLICT(id) DO UPDATE SET
                 pre_sha256 = excluded.pre_sha256,
                 post_sha256 = excluded.post_sha256,
                 pre_text = excluded.pre_text",
            params![
                record.id,
                record.session_id.as_str(),
                record.run_id.as_ref().map(|run| run.as_str()),
                record.path,
                record.existed as i64,
                record.pre_sha256,
                record.post_sha256,
                record.pre_text,
                record.captured_at,
            ],
        )?;
        Ok(())
    }

    pub fn checkpoints_for_session(
        &self,
        session: &SessionId,
    ) -> Result<Vec<CheckpointRecord>, StoreError> {
        let mut statement = self.conn.prepare(
            "SELECT id, session_id, run_id, path, existed, pre_sha256, post_sha256,
                    pre_text, captured_at, restored_at
             FROM checkpoints WHERE session_id = ?1 ORDER BY captured_at ASC",
        )?;
        let rows = statement.query_map(params![session.as_str()], read_checkpoint_row)?;
        let mut checkpoints = Vec::new();
        for row in rows {
            checkpoints.push(row?);
        }
        Ok(checkpoints)
    }

    pub fn checkpoint(&self, id: &str) -> Result<CheckpointRecord, StoreError> {
        self.conn
            .query_row(
                "SELECT id, session_id, run_id, path, existed, pre_sha256, post_sha256,
                        pre_text, captured_at, restored_at
                 FROM checkpoints WHERE id = ?1",
                params![id],
                read_checkpoint_row,
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound(format!("checkpoint {id}")))
    }

    /// Record that a conditional restore actually happened.
    pub fn mark_checkpoint_restored(&mut self, id: &str) -> Result<(), StoreError> {
        let updated = self.conn.execute(
            "UPDATE checkpoints SET restored_at = ?1 WHERE id = ?2",
            params![timeutil::now_rfc3339(), id],
        )?;
        if updated == 0 {
            return Err(StoreError::NotFound(format!("checkpoint {id}")));
        }
        Ok(())
    }

    /// Content-addressed artifact write: temp file, fsync, rename, then DB row.
    pub fn write_artifact(
        &mut self,
        directory: &Path,
        bytes: &[u8],
        media_type: &str,
        retention_class: &str,
    ) -> Result<ArtifactRef, StoreError> {
        std::fs::create_dir_all(directory)?;
        let hash = hex::encode(Sha256::digest(bytes));
        let relative_path = format!("sha256-{hash}");
        let final_path = directory.join(&relative_path);
        if !final_path.exists() {
            let temp_path = directory.join(format!(".tmp-{hash}-{}", std::process::id()));
            {
                use std::io::Write;
                let mut file = std::fs::File::create(&temp_path)?;
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            std::fs::rename(&temp_path, &final_path)?;
        }
        let id = ArtifactId::generate();
        self.conn.execute(
            "INSERT INTO artifacts(id, hash, bytes, media_type, relative_path, retention_class, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id.as_str(),
                hash,
                bytes.len() as i64,
                media_type,
                relative_path,
                retention_class,
                timeutil::now_rfc3339()
            ],
        )?;
        Ok(ArtifactRef {
            id,
            hash,
            bytes: bytes.len() as u64,
            media_type: media_type.to_string(),
            relative_path,
        })
    }

    pub fn read_artifact(
        &self,
        directory: &Path,
        id: &ArtifactId,
    ) -> Result<Vec<u8>, StoreError> {
        let relative: Option<String> = self
            .conn
            .query_row(
                "SELECT relative_path FROM artifacts WHERE id = ?1",
                params![id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let relative =
            relative.ok_or_else(|| StoreError::NotFound(format!("artifact {id}")))?;
        std::fs::read(directory.join(relative)).map_err(StoreError::from)
    }

    pub fn artifact_count(&self) -> Result<u64, StoreError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM artifacts", [], |row| row.get(0))?;
        Ok(count as u64)
    }

    pub fn schema_version(&self) -> Result<i64, StoreError> {
        let version: Option<i64> = self.conn.query_row(
            "SELECT MAX(version) FROM schema_migrations",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )?;
        Ok(version.unwrap_or(0))
    }
}

/// Durable schema v1. Additive changes get a new migration; never edit this.
const MIGRATION_1: &str = r#"
CREATE TABLE IF NOT EXISTS sessions(
    id TEXT PRIMARY KEY,
    workspace_identity TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_seq INTEGER NOT NULL DEFAULT 0,
    label TEXT
);
CREATE TABLE IF NOT EXISTS runs(
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    state TEXT NOT NULL,
    policy_revision INTEGER NOT NULL,
    model_id TEXT NOT NULL,
    provider TEXT NOT NULL,
    started_at TEXT NOT NULL,
    ended_at TEXT
);
CREATE TABLE IF NOT EXISTS events(
    session_id TEXT NOT NULL REFERENCES sessions(id),
    seq INTEGER NOT NULL,
    event_id TEXT NOT NULL UNIQUE,
    run_id TEXT,
    type TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    envelope_json TEXT NOT NULL,
    PRIMARY KEY(session_id, seq)
);
CREATE TABLE IF NOT EXISTS operations(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL REFERENCES runs(id),
    tool_call_id TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    intent_hash TEXT NOT NULL,
    state TEXT NOT NULL,
    result_ref TEXT,
    updated_at TEXT NOT NULL,
    UNIQUE(run_id, tool_call_id)
);
CREATE TABLE IF NOT EXISTS artifacts(
    id TEXT PRIMARY KEY,
    hash TEXT NOT NULL,
    bytes INTEGER NOT NULL,
    media_type TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    retention_class TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_events_session_seq ON events(session_id, seq);
CREATE INDEX IF NOT EXISTS idx_operations_run ON operations(run_id);
"#;

/// Durable schema v2: patch checkpoints survive the process so a later client
/// can preview and conditionally restore them.
const MIGRATION_2: &str = r#"
CREATE TABLE IF NOT EXISTS checkpoints(
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    run_id TEXT,
    path TEXT NOT NULL,
    existed INTEGER NOT NULL,
    pre_sha256 TEXT,
    post_sha256 TEXT,
    pre_text TEXT,
    captured_at TEXT NOT NULL,
    restored_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_checkpoints_session ON checkpoints(session_id);
"#;

/// Ordered, forward-only migrations applied in version order.
const MIGRATIONS: &[(i64, &str)] = &[(1, MIGRATION_1), (2, MIGRATION_2)];

fn read_checkpoint_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CheckpointRecord> {
    let session_id: String = row.get(1)?;
    let run_id: Option<String> = row.get(2)?;
    Ok(CheckpointRecord {
        id: row.get(0)?,
        session_id: SessionId::parse(session_id).unwrap_or_else(|_| SessionId::generate()),
        run_id: run_id.and_then(|value| RunId::parse(value).ok()),
        path: row.get(3)?,
        existed: row.get::<_, i64>(4)? != 0,
        pre_sha256: row.get(5)?,
        post_sha256: row.get(6)?,
        pre_text: row.get(7)?,
        captured_at: row.get(8)?,
        restored_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollo_protocol::events::{AssistantDeltaData, RunFinishedData};
    use bollo_protocol::vocab::VerificationStatus;
    use serde_json::json;

    fn store() -> SqliteStore {
        SqliteStore::open_in_memory().unwrap()
    }

    #[test]
    fn migrates_and_reports_schema_version() {
        let store = store();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        // Idempotent.
        let mut store = store;
        store.migrate().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn events_append_with_monotonic_seq_and_replay_in_order() {
        let mut store = store();
        let session = store.create_session("ws-1", Some("test")).unwrap();
        let run = store.start_run(&session, "anthropic", "model", 1).unwrap();
        store
            .append_event(
                &session,
                Some(&run),
                EventType::AssistantDelta,
                &serde_json::to_value(AssistantDeltaData { text: "a".into() }).unwrap(),
            )
            .unwrap();
        let second = store
            .append_event(
                &session,
                Some(&run),
                EventType::AssistantDelta,
                &serde_json::to_value(AssistantDeltaData { text: "b".into() }).unwrap(),
            )
            .unwrap();
        assert_eq!(second.seq, 2);
        assert_eq!(store.last_seq(&session).unwrap(), 2);
        let replayed = store.replay(&session, 0).unwrap();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].seq, 1);
        assert_eq!(replayed[1].data["text"], "b");
        let after = store.replay(&session, 1).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].seq, 2);
    }

    #[test]
    fn operation_intent_is_journaled_before_result() {
        let mut store = store();
        let session = store.create_session("ws-1", None).unwrap();
        let run = store.start_run(&session, "xai", "model", 1).unwrap();
        store
            .record_operation_intent(&run, "call_1", "write_file", &"a".repeat(64))
            .unwrap();
        let pending = store.operations_for_run(&run).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, OperationState::Started);

        store
            .record_operation_result(&run, "call_1", OperationState::Succeeded, Some("art_1"))
            .unwrap();
        let done = store.operations_for_run(&run).unwrap();
        assert_eq!(done[0].state, OperationState::Succeeded);
        assert_eq!(done[0].result_ref.as_deref(), Some("art_1"));
    }

    #[test]
    fn crash_recovery_marks_stale_operations_unknown() {
        let mut store = store();
        let session = store.create_session("ws-1", None).unwrap();
        let run = store.start_run(&session, "anthropic", "model", 1).unwrap();
        store
            .record_operation_intent(&run, "call_1", "exec", &"b".repeat(64))
            .unwrap();
        let updated = store.mark_stale_operations_unknown().unwrap();
        assert_eq!(updated, 1);
        let operations = store.operations_for_run(&run).unwrap();
        assert_eq!(operations[0].state, OperationState::Unknown);
    }

    #[test]
    fn interrupted_runs_are_detectable() {
        let mut store = store();
        let session = store.create_session("ws-1", None).unwrap();
        let run = store.start_run(&session, "anthropic", "model", 1).unwrap();
        let interrupted = store.interrupted_runs(&session).unwrap();
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].id, run);
        store
            .finish_run(&run, TerminalState::Completed, None, VerificationStatus::Passed)
            .unwrap();
        assert!(store.interrupted_runs(&session).unwrap().is_empty());
    }

    #[test]
    fn run_finished_event_persists_terminal_state() {
        let mut store = store();
        let session = store.create_session("ws-1", None).unwrap();
        let run = store.start_run(&session, "anthropic", "model", 1).unwrap();
        store
            .append_event(
                &session,
                Some(&run),
                EventType::RunFinished,
                &serde_json::to_value(RunFinishedData {
                    state: TerminalState::Completed,
                    reason: None,
                    verification: VerificationStatus::Skipped,
                })
                .unwrap(),
            )
            .unwrap();
        let replayed = store.replay(&session, 0).unwrap();
        assert_eq!(replayed[0].event_type, EventType::RunFinished);
        assert_eq!(replayed[0].data["state"], "completed");
    }

    #[test]
    fn artifacts_are_content_addressed_and_readable() {
        let mut store = store();
        let dir = tempfile::tempdir().unwrap();
        let first = store
            .write_artifact(dir.path(), b"same bytes", "text/plain", "bounded_output")
            .unwrap();
        let second = store
            .write_artifact(dir.path(), b"same bytes", "text/plain", "bounded_output")
            .unwrap();
        assert_eq!(first.hash, second.hash);
        assert!(dir.path().join(&first.relative_path).exists());
        let bytes = store.read_artifact(dir.path(), &first.id).unwrap();
        assert_eq!(bytes, b"same bytes");
        assert_eq!(store.artifact_count().unwrap(), 2);
    }

    #[test]
    fn invalid_event_data_is_rejected_before_persistence() {
        let mut store = store();
        let session = store.create_session("ws-1", None).unwrap();
        // usage.updated with inconsistent cost must not be journaled.
        let result = store.append_event(
            &session,
            None,
            EventType::PolicyChanged,
            &json!({"policy_revision": 0, "profile": "balanced", "sandbox": "workspace"}),
        );
        assert!(result.is_err());
        assert_eq!(store.replay(&session, 0).unwrap().len(), 0);
    }

    #[test]
    fn sessions_can_be_listed_and_deleted_without_touching_source() {
        let mut store = store();
        let first = store.create_session("ws-1", Some("one")).unwrap();
        let second = store.create_session("ws-2", None).unwrap();
        let listed = store.list_sessions().unwrap();
        assert_eq!(listed.len(), 2);
        assert!(listed.iter().any(|entry| entry.id == first));
        assert!(listed.iter().any(|entry| entry.id == second));

        store.delete_session(&first).unwrap();
        let remaining = store.list_sessions().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, second);
        assert!(store.replay(&first, 0).is_err() || store.last_seq(&first).is_err());
    }

    #[test]
    fn checkpoints_round_trip_and_keep_restore_marker_on_re_record() {
        let mut store = store();
        let session = store.create_session("ws-1", None).unwrap();
        let run = store.start_run(&session, "anthropic", "model", 1).unwrap();
        let mut record = CheckpointRecord {
            id: "ck_0001".into(),
            session_id: session.clone(),
            run_id: Some(run.clone()),
            path: "src/main.rs".into(),
            existed: true,
            pre_sha256: Some("a".repeat(64)),
            post_sha256: None,
            pre_text: Some("original".into()),
            captured_at: timeutil::now_rfc3339(),
            restored_at: None,
        };
        store.record_checkpoint(&record).unwrap();
        record.post_sha256 = Some("b".repeat(64));
        store.record_checkpoint(&record).unwrap();
        store.mark_checkpoint_restored("ck_0001").unwrap();
        // Re-recording after a restore must not clear the marker.
        store.record_checkpoint(&record).unwrap();

        let stored = store.checkpoint("ck_0001").unwrap();
        assert_eq!(stored.post_sha256.as_deref(), Some("b".repeat(64).as_str()));
        assert!(stored.restored_at.is_some());
        let for_session = store.checkpoints_for_session(&session).unwrap();
        assert_eq!(for_session.len(), 1);
        assert!(store.checkpoint("missing").is_err());
    }

    #[test]
    fn migration_upgrades_an_existing_v1_database_in_place() {
        // Simulate an N-1 database by hand: only migration 1 is recorded, then
        // the normal `migrate()` path must bring it to the current version.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_migrations(
                 version INTEGER PRIMARY KEY,
                 checksum TEXT NOT NULL,
                 applied_at TEXT NOT NULL
             );",
        )
        .unwrap();
        conn.execute_batch(MIGRATION_1).expect("v1 schema applies");
        let checksum = hex::encode(Sha256::digest(MIGRATION_1.as_bytes()));
        conn.execute(
            "INSERT INTO schema_migrations(version, checksum, applied_at) VALUES (1, ?1, ?2)",
            params![checksum, timeutil::now_rfc3339()],
        )
        .unwrap();
        let mut store = SqliteStore { conn };
        assert_eq!(store.schema_version().unwrap(), 1);

        store.migrate().unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        // The upgraded database can hold checkpoints.
        let session = store.create_session("ws-1", None).unwrap();
        store
            .record_checkpoint(&CheckpointRecord {
                id: "ck_upgrade".into(),
                session_id: session,
                run_id: None,
                path: "a.txt".into(),
                existed: false,
                pre_sha256: None,
                post_sha256: Some("c".repeat(64)),
                pre_text: None,
                captured_at: timeutil::now_rfc3339(),
                restored_at: None,
            })
            .unwrap();
    }
}
