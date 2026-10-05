//! Durable idempotency receipts for creation routes.
//!
//! Scope is `(token principal, route, key)` for 24 hours, stored separately
//! from the core journal so P2 API state never pollutes MVP session storage.
//! A same-key/different-body retry is a conflict; the same key and body
//! replays the original status and response, including across restarts.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use bollo_protocol::ids::is_valid_opaque_id;

use crate::error::ApiError;

/// Minimum accepted `Idempotency-Key` length.
pub const KEY_MIN_CHARS: usize = 16;
/// Maximum accepted `Idempotency-Key` length (shared opaque-id cap).
pub const KEY_MAX_CHARS: usize = 128;
/// Receipt lifetime in seconds (24 h).
pub const RECEIPT_TTL_SECONDS: i64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyRecord {
    pub body_sha256: String,
    pub status: u16,
    pub response_json: String,
}

pub struct IdempotencyStore {
    conn: Connection,
}

impl IdempotencyStore {
    pub fn open(path: &Path) -> Result<Self, ApiError> {
        let conn = Connection::open(path)
            .map_err(|err| ApiError::storage(format!("open receipts: {err}")))?;
        Self::from_connection(conn, true)
    }

    pub fn open_in_memory() -> Result<Self, ApiError> {
        let conn = Connection::open_in_memory()
            .map_err(|err| ApiError::storage(format!("open receipts: {err}")))?;
        Self::from_connection(conn, false)
    }

    fn from_connection(conn: Connection, wal: bool) -> Result<Self, ApiError> {
        if wal {
            let _ = conn.pragma_update(None, "journal_mode", "WAL");
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS idempotency_receipts(
                 principal TEXT NOT NULL,
                 route TEXT NOT NULL,
                 key TEXT NOT NULL,
                 body_sha256 TEXT NOT NULL,
                 status INTEGER NOT NULL,
                 response_json TEXT NOT NULL,
                 created_epoch INTEGER NOT NULL,
                 PRIMARY KEY(principal, route, key)
             );",
        )
        .map_err(|err| ApiError::storage(format!("migrate receipts: {err}")))?;
        Ok(Self { conn })
    }

    /// A live receipt for this scope, or `None` when absent or expired.
    pub fn lookup(
        &self,
        principal: &str,
        route: &str,
        key: &str,
        now_epoch: i64,
    ) -> Result<Option<IdempotencyRecord>, ApiError> {
        let row: Option<(String, i64, String, i64)> = self
            .conn
            .query_row(
                "SELECT body_sha256, status, response_json, created_epoch
                 FROM idempotency_receipts
                 WHERE principal = ?1 AND route = ?2 AND key = ?3",
                params![principal, route, key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|err| ApiError::storage(format!("read receipt: {err}")))?;
        let Some((body_sha256, status, response_json, created_epoch)) = row else {
            return Ok(None);
        };
        if created_epoch + RECEIPT_TTL_SECONDS <= now_epoch {
            return Ok(None);
        }
        Ok(Some(IdempotencyRecord {
            body_sha256,
            status: status as u16,
            response_json,
        }))
    }

    /// Persist (or refresh) a receipt. Duplicate creation attempts inside the
    /// TTL are caught by `lookup`; this write is the completion of a fresh one.
    pub fn record(
        &mut self,
        principal: &str,
        route: &str,
        key: &str,
        body_sha256: &str,
        status: u16,
        response_json: &str,
        now_epoch: i64,
    ) -> Result<(), ApiError> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO idempotency_receipts
                 (principal, route, key, body_sha256, status, response_json, created_epoch)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    principal,
                    route,
                    key,
                    body_sha256,
                    status as i64,
                    response_json,
                    now_epoch
                ],
            )
            .map_err(|err| ApiError::storage(format!("write receipt: {err}")))?;
        Ok(())
    }
}

/// Validate the documented 16–128 character opaque key.
pub fn validate_key(key: &str) -> Result<(), ApiError> {
    if key.len() < KEY_MIN_CHARS || key.len() > KEY_MAX_CHARS || !is_valid_opaque_id(key) {
        return Err(ApiError::invalid_request(format!(
            "Idempotency-Key must be an opaque string of {KEY_MIN_CHARS}–{KEY_MAX_CHARS} characters"
        )));
    }
    Ok(())
}

pub fn body_sha256(body: &[u8]) -> String {
    hex::encode(Sha256::digest(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_roundtrips_and_respects_ttl() {
        let mut store = IdempotencyStore::open_in_memory().unwrap();
        let key = "0123456789abcdef";
        validate_key(key).unwrap();
        assert!(store
            .lookup("tok", "/v1/sessions", key, 100)
            .unwrap()
            .is_none());
        store
            .record(
                "tok",
                "/v1/sessions",
                key,
                "hash",
                201,
                "{\"id\":\"s\"}",
                100,
            )
            .unwrap();
        let found = store
            .lookup("tok", "/v1/sessions", key, 200)
            .unwrap()
            .unwrap();
        assert_eq!(found.status, 201);
        assert_eq!(found.body_sha256, "hash");
        // Expired receipts are invisible.
        assert!(store
            .lookup("tok", "/v1/sessions", key, 100 + RECEIPT_TTL_SECONDS + 1)
            .unwrap()
            .is_none());
        // Scope isolation.
        assert!(store
            .lookup("other", "/v1/sessions", key, 200)
            .unwrap()
            .is_none());
        assert!(store
            .lookup("tok", "/v1/sessions/other", key, 200)
            .unwrap()
            .is_none());
    }

    #[test]
    fn key_validation_matches_documentation() {
        assert!(validate_key("0123456789abcdef").is_ok());
        assert!(validate_key("too-short").is_err());
        assert!(validate_key(&"x".repeat(129)).is_err());
        assert!(validate_key("has space 1234567890").is_err());
        assert!(validate_key("_leading0123456789").is_err());
    }
}
