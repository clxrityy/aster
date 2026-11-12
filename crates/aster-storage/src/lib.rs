//! PostgreSQL-backed Phase 1 identity and regulatory snapshots.
//!
//! Connecting, migrating, and reading never create an identity. Only explicit
//! initialization may write the foundation; there are no snapshot mutation APIs.

use std::time::Duration;

use aster_core::{IdentitySnapshot, RegulatoryState};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{PgPool, postgres::PgPoolOptions};
use thiserror::Error;
use uuid::Uuid;

/// Displays only redacted context. Sources are retained for trusted diagnostics;
/// callers must not expose source chains or Debug output to untrusted clients.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("storage database operation failed")]
    Database(#[from] sqlx::Error),
    #[error("storage migration failed")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("stored snapshot is invalid")]
    Snapshot(#[from] serde_json::Error),
    #[error("stored foundation is incomplete or inconsistent")]
    InconsistentFoundation,
}

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InitOutcome {
    pub identity: IdentitySnapshot,
    pub created: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct StoredRegulatoryState {
    pub state: RegulatoryState,
    pub updated_at: DateTime<Utc>,
}

impl Store {
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn connect(database_url: &str) -> Result<Self, StorageError> {
        // SQLx's acquisition deadline includes opening a new connection, and
        // connect() acquires its initial connection within the same deadline.
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(5))
            .after_connect(|connection, _| {
                Box::pin(async move {
                    // Acquisition timeouts do not bound a query waiting on locks.
                    sqlx::query("SET statement_timeout = '5s'")
                        .execute(&mut *connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(database_url)
            .await?;
        Ok(Self::from_pool(pool))
    }

    pub async fn migrate(&self) -> Result<(), StorageError> {
        sqlx::migrate!("../../migrations").run(&self.pool).await?;
        Ok(())
    }

    pub async fn initialize(&self) -> Result<InitOutcome, StorageError> {
        let mut tx = self.pool.begin().await?;
        // Do not inherit a server/role REPEATABLE READ default: after a losing
        // ON CONFLICT waits, the next statement must see the winning commit.
        sqlx::query("SET TRANSACTION ISOLATION LEVEL READ COMMITTED")
            .execute(&mut *tx)
            .await?;

        let mut candidate = IdentitySnapshot::foundation();
        // PostgreSQL stores microseconds. Normalize before serializing so the
        // snapshot and its relational timestamp are exactly equal on reload.
        candidate.created_at =
            DateTime::from_timestamp_micros(candidate.created_at.timestamp_micros())
                .ok_or(StorageError::InconsistentFoundation)?;
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO aster_identity (id, created_at, snapshot)
             VALUES ($1, $2, $3)
             ON CONFLICT (singleton) DO NOTHING RETURNING id",
        )
        .bind(candidate.id)
        .bind(candidate.created_at)
        .bind(serde_json::to_value(&candidate)?)
        .fetch_optional(&mut *tx)
        .await?;

        let created = inserted.is_some();
        if let Some(id) = inserted {
            sqlx::query(
                "INSERT INTO aster_regulatory_state (identity_id, snapshot) VALUES ($1, $2)",
            )
            .bind(id)
            .bind(serde_json::to_value(RegulatoryState::default())?)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO aster_audit_events (identity_id, kind, metadata)
                 VALUES ($1, 'identity_initialized', $2)",
            )
            .bind(id)
            .bind(serde_json::json!({ "revision": 1 }))
            .execute(&mut *tx)
            .await?;
        }

        let row = sqlx::query_as::<_, (Uuid, DateTime<Utc>, Value)>(
            "SELECT id, created_at, snapshot FROM aster_identity WHERE singleton = TRUE",
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StorageError::InconsistentFoundation)?;
        let identity = decode_identity(row)?;
        let state = sqlx::query_scalar::<_, Value>(
            "SELECT snapshot FROM aster_regulatory_state WHERE identity_id = $1",
        )
        .bind(identity.id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StorageError::InconsistentFoundation)?;
        // Decode through the core's validated Level Serde implementation. Never
        // restore malformed data by substituting default levels or a new UUID.
        serde_json::from_value::<RegulatoryState>(state)?;
        let audit_count = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM aster_audit_events
             WHERE identity_id = $1 AND kind = 'identity_initialized'",
        )
        .bind(identity.id)
        .fetch_one(&mut *tx)
        .await?;
        if audit_count != 1 {
            return Err(StorageError::InconsistentFoundation);
        }
        tx.commit().await?;
        Ok(InitOutcome { identity, created })
    }

    pub async fn identity(&self) -> Result<Option<IdentitySnapshot>, StorageError> {
        sqlx::query_as::<_, (Uuid, DateTime<Utc>, Value)>(
            "SELECT id, created_at, snapshot FROM aster_identity WHERE singleton = TRUE",
        )
        .fetch_optional(&self.pool)
        .await?
        .map(decode_identity)
        .transpose()
    }

    pub async fn regulatory_state(&self) -> Result<Option<StoredRegulatoryState>, StorageError> {
        let row = sqlx::query_as::<_, (Value, DateTime<Utc>)>(
            "SELECT s.snapshot, s.updated_at FROM aster_regulatory_state s
             JOIN aster_identity i ON i.id = s.identity_id WHERE i.singleton = TRUE",
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(|(snapshot, updated_at)| {
            Ok(StoredRegulatoryState {
                state: serde_json::from_value(snapshot)?,
                updated_at,
            })
        })
        .transpose()
    }

    pub async fn is_ready(&self) -> Result<bool, StorageError> {
        // A real database query checks reachability and the matching pair in
        // one statement. Invalid snapshots fail closed rather than claim ready.
        let row = sqlx::query_as::<_, (Uuid, DateTime<Utc>, Value, Value, i64)>(
            "SELECT i.id, i.created_at, i.snapshot, s.snapshot,
                    (SELECT count(*) FROM aster_audit_events a
                     WHERE a.identity_id = i.id AND a.kind = 'identity_initialized')
             FROM aster_identity i
             JOIN aster_regulatory_state s ON s.identity_id = i.id
             WHERE i.singleton = TRUE",
        )
        .fetch_optional(&self.pool)
        .await?;
        let Some((id, created_at, identity, state, audit_count)) = row else {
            return Ok(false);
        };
        decode_identity((id, created_at, identity))?;
        serde_json::from_value::<RegulatoryState>(state)?;
        if audit_count != 1 {
            return Err(StorageError::InconsistentFoundation);
        }
        Ok(true)
    }

    /// Waits for in-flight connections to return; closes all clones' shared pool.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

fn decode_identity(
    (id, created_at, snapshot): (Uuid, DateTime<Utc>, Value),
) -> Result<IdentitySnapshot, StorageError> {
    let identity: IdentitySnapshot = serde_json::from_value(snapshot)?;
    if identity.id != id || identity.created_at != created_at {
        return Err(StorageError::InconsistentFoundation);
    }
    Ok(identity)
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[test]
    fn error_display_is_redacted_and_retains_sources() {
        let secret = "postgres://owner:secret@example.test/aster";
        let database = StorageError::Database(sqlx::Error::Protocol(secret.into()));
        let migration = StorageError::Migration(sqlx::migrate::MigrateError::Execute(
            sqlx::Error::Protocol(secret.into()),
        ));
        let snapshot =
            StorageError::Snapshot(serde_json::from_str::<RegulatoryState>("{}").unwrap_err());
        for error in [database, migration, snapshot] {
            assert!(error.source().is_some());
            assert!(!error.to_string().contains(secret));
            assert!(!error.to_string().contains("secret"));
        }
        assert!(StorageError::InconsistentFoundation.source().is_none());
    }

    #[test]
    fn stored_state_serde_preserves_numeric_bounds() {
        let stored = StoredRegulatoryState {
            state: RegulatoryState::default(),
            updated_at: Utc::now(),
        };
        let value = serde_json::to_value(&stored).unwrap();
        assert!(value["state"]["energy"].is_number());
        assert_eq!(
            serde_json::from_value::<StoredRegulatoryState>(value.clone()).unwrap(),
            stored
        );
        for invalid in [
            serde_json::json!(-1),
            serde_json::json!(101),
            serde_json::json!("NaN"),
            Value::Null,
        ] {
            let mut corrupt = value.clone();
            corrupt["state"]["energy"] = invalid;
            assert!(serde_json::from_value::<StoredRegulatoryState>(corrupt).is_err());
        }
    }

    #[test]
    fn identity_decoder_rejects_metadata_mismatch() {
        let identity = IdentitySnapshot::foundation();
        let snapshot = serde_json::to_value(&identity).unwrap();
        assert!(decode_identity((Uuid::new_v4(), identity.created_at, snapshot.clone())).is_err());
        assert!(
            decode_identity((
                identity.id,
                identity.created_at + chrono::Duration::seconds(1),
                snapshot
            ))
            .is_err()
        );
    }
}
