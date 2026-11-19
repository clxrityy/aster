//! Runtime records: turns + traces, feedback, outbound questions/requests,
//! regulatory history, and mind snapshots. Every consequential mutation shares
//! a transaction with its payload-free audit event.

use aster_core::{OutboundDraft, OutboundStatus, RegulatoryState, ThoughtTrace};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{StorageError, Store, memory::memory_identity};

#[derive(Debug, Clone, Serialize)]
pub struct TurnRecord {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub role: String,
    pub mode: String,
    pub text: String,
    pub reply_to: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutboundRecord {
    pub id: Uuid,
    pub kind: String,
    pub draft: OutboundDraft,
    pub needs_approval: bool,
    pub origin_turn: Option<Uuid>,
    pub status: OutboundStatus,
    pub answer: Option<String>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateHistoryEntry {
    pub recorded_at: DateTime<Utc>,
    pub state: RegulatoryState,
    pub cause: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotInfo {
    pub version: i64,
    pub engine: String,
    pub reason: String,
    pub created_at: DateTime<Utc>,
    pub stats: Value,
}

pub struct StoredSnapshot {
    pub info: SnapshotInfo,
    pub model: Vec<u8>,
}

pub struct Exchange<'a> {
    pub conversation_id: Uuid,
    pub owner_turn_id: Uuid,
    pub mode: &'a str,
    pub owner_text: &'a str,
    pub aster_text: &'a str,
    pub trace: &'a ThoughtTrace,
    pub question: Option<&'a OutboundDraft>,
    pub state: &'a RegulatoryState,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExchangeOutcome {
    pub owner: TurnRecord,
    pub aster: TurnRecord,
    pub outbound: Option<OutboundRecord>,
}

type TurnRow = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    Option<Uuid>,
    DateTime<Utc>,
);
type OutboundRow = (
    Uuid,
    String,
    Value,
    Option<Uuid>,
    String,
    Option<String>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);
type SnapshotRow = (i64, String, String, DateTime<Utc>, Value);

// Column lists as macros so every query stays a static string.
macro_rules! turn_columns {
    () => {
        "id, conversation_id, role, mode, text, reply_to, created_at"
    };
}
macro_rules! outbound_columns {
    () => {
        "id, kind, draft, origin_turn, status, answer, created_at, resolved_at"
    };
}
const MAX_TURN_BYTES: usize = 16384;

fn turn((id, conversation_id, role, mode, text, reply_to, created_at): TurnRow) -> TurnRecord {
    TurnRecord {
        id,
        conversation_id,
        role,
        mode,
        text,
        reply_to,
        created_at,
    }
}

fn outbound(row: OutboundRow) -> Result<OutboundRecord, StorageError> {
    let (id, kind, draft, origin_turn, status, answer, created_at, resolved_at) = row;
    let draft: OutboundDraft = serde_json::from_value(draft)?;
    let status = OutboundStatus::parse(&status).ok_or(StorageError::InconsistentFoundation)?;
    Ok(OutboundRecord {
        id,
        kind,
        needs_approval: draft.needs_approval(),
        draft,
        origin_turn,
        status,
        answer,
        created_at,
        resolved_at,
    })
}

fn snapshot_info((version, engine, reason, created_at, stats): SnapshotRow) -> SnapshotInfo {
    SnapshotInfo {
        version,
        engine,
        reason,
        created_at,
        stats,
    }
}

/// Keep stored text within the column bound on a character boundary.
fn bounded(text: &str) -> &str {
    if text.len() <= MAX_TURN_BYTES {
        return text;
    }
    let mut end = MAX_TURN_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

async fn audit(
    connection: &mut PgConnection,
    identity_id: Uuid,
    kind: &str,
    metadata: Value,
) -> Result<(), StorageError> {
    sqlx::query("INSERT INTO aster_audit_events (identity_id, kind, metadata) VALUES ($1, $2, $3)")
        .bind(identity_id)
        .bind(kind)
        .bind(metadata)
        .execute(connection)
        .await?;
    Ok(())
}

async fn insert_outbound(
    connection: &mut PgConnection,
    identity_id: Uuid,
    draft: &OutboundDraft,
    origin_turn: Option<Uuid>,
) -> Result<OutboundRecord, StorageError> {
    let row = sqlx::query_as::<_, OutboundRow>(concat!(
        "INSERT INTO aster_outbound (id, identity_id, kind, draft, origin_turn)
         VALUES ($1, $2, $3, $4, $5) RETURNING ",
        outbound_columns!()
    ))
    .bind(Uuid::new_v4())
    .bind(identity_id)
    .bind(draft.kind.as_str())
    .bind(serde_json::to_value(draft)?)
    .bind(origin_turn)
    .fetch_one(&mut *connection)
    .await?;
    let record = outbound(row)?;
    audit(
        connection,
        identity_id,
        "outbound_created",
        serde_json::json!({ "outbound_id": record.id, "kind": record.kind }),
    )
    .await?;
    Ok(record)
}

async fn write_state(
    connection: &mut PgConnection,
    identity_id: Uuid,
    state: &RegulatoryState,
    cause: &str,
) -> Result<(), StorageError> {
    let snapshot = serde_json::to_value(state)?;
    sqlx::query(
        "UPDATE aster_regulatory_state SET snapshot = $2, updated_at = now() WHERE identity_id = $1",
    )
    .bind(identity_id)
    .bind(&snapshot)
    .execute(&mut *connection)
    .await?;
    sqlx::query(
        "INSERT INTO aster_state_history (identity_id, snapshot, cause) VALUES ($1, $2, $3)",
    )
    .bind(identity_id)
    .bind(&snapshot)
    .bind(&cause[..cause.len().min(256)])
    .execute(connection)
    .await?;
    Ok(())
}

impl Store {
    /// Persist one owner→Aster exchange, its trace, any question, and new levels.
    pub async fn record_exchange(
        &self,
        exchange: &Exchange<'_>,
    ) -> Result<ExchangeOutcome, StorageError> {
        let mut tx = self.pool.begin().await?;
        let identity_id = memory_identity(&mut tx).await?;
        let owner = sqlx::query_as::<_, TurnRow>(concat!(
            "INSERT INTO aster_turns (id, identity_id, conversation_id, role, mode, text)
             VALUES ($1, $2, $3, 'owner', $4, $5) RETURNING ",
            turn_columns!()
        ))
        .bind(exchange.owner_turn_id)
        .bind(identity_id)
        .bind(exchange.conversation_id)
        .bind(exchange.mode)
        .bind(bounded(exchange.owner_text))
        .fetch_one(&mut *tx)
        .await?;
        let aster = sqlx::query_as::<_, TurnRow>(concat!(
            "INSERT INTO aster_turns (id, identity_id, conversation_id, role, mode, text, reply_to)
             VALUES ($1, $2, $3, 'aster', $4, $5, $6) RETURNING ",
            turn_columns!()
        ))
        .bind(Uuid::new_v4())
        .bind(identity_id)
        .bind(exchange.conversation_id)
        .bind(exchange.mode)
        .bind(bounded(exchange.aster_text))
        .bind(exchange.owner_turn_id)
        .fetch_one(&mut *tx)
        .await?;
        let aster = turn(aster);
        sqlx::query("INSERT INTO aster_thoughts (turn_id, trace) VALUES ($1, $2)")
            .bind(aster.id)
            .bind(serde_json::to_value(exchange.trace)?)
            .execute(&mut *tx)
            .await?;
        let outbound = match exchange.question {
            Some(draft) => {
                Some(insert_outbound(&mut tx, identity_id, draft, Some(aster.id)).await?)
            }
            None => None,
        };
        write_state(&mut tx, identity_id, exchange.state, "exchange").await?;
        audit(
            &mut tx,
            identity_id,
            "exchange_recorded",
            serde_json::json!({ "owner_turn": exchange.owner_turn_id, "aster_turn": aster.id }),
        )
        .await?;
        tx.commit().await?;
        Ok(ExchangeOutcome {
            owner: turn(owner),
            aster,
            outbound,
        })
    }

    pub async fn recent_turns(&self, limit: i64) -> Result<Vec<TurnRecord>, StorageError> {
        let mut rows = sqlx::query_as::<_, TurnRow>(concat!(
            "SELECT ",
            turn_columns!(),
            " FROM aster_turns ORDER BY created_at DESC, id DESC LIMIT $1"
        ))
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?;
        rows.reverse();
        Ok(rows.into_iter().map(turn).collect())
    }

    pub async fn thought(&self, turn_id: Uuid) -> Result<Option<Value>, StorageError> {
        Ok(
            sqlx::query_scalar::<_, Value>("SELECT trace FROM aster_thoughts WHERE turn_id = $1")
                .bind(turn_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Record a rating for an Aster turn. Returns false if no such Aster turn.
    pub async fn record_feedback(
        &self,
        turn_id: Uuid,
        rating: i8,
        correction: Option<&str>,
    ) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await?;
        let identity_id = memory_identity(&mut tx).await?;
        let inserted = sqlx::query(
            "INSERT INTO aster_feedback (turn_id, rating, correction)
             SELECT id, $2, $3 FROM aster_turns WHERE id = $1 AND role = 'aster'",
        )
        .bind(turn_id)
        .bind(i16::from(rating.signum()))
        .bind(correction.map(bounded))
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if inserted {
            audit(
                &mut tx,
                identity_id,
                "feedback_recorded",
                serde_json::json!({ "turn_id": turn_id, "rating": rating.signum() }),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(inserted)
    }

    pub async fn create_outbound(
        &self,
        draft: &OutboundDraft,
    ) -> Result<OutboundRecord, StorageError> {
        let mut tx = self.pool.begin().await?;
        let identity_id = memory_identity(&mut tx).await?;
        let record = insert_outbound(&mut tx, identity_id, draft, None).await?;
        tx.commit().await?;
        Ok(record)
    }

    pub async fn list_outbound(
        &self,
        status: Option<OutboundStatus>,
        limit: i64,
    ) -> Result<Vec<OutboundRecord>, StorageError> {
        sqlx::query_as::<_, OutboundRow>(concat!(
            "SELECT ",
            outbound_columns!(),
            " FROM aster_outbound
             WHERE ($1::text IS NULL OR status = $1)
             ORDER BY created_at DESC, id DESC LIMIT $2"
        ))
        .bind(status.map(OutboundStatus::as_str))
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(outbound)
        .collect()
    }

    /// Resolve a pending item. Returns None if absent or already resolved.
    pub async fn resolve_outbound(
        &self,
        id: Uuid,
        status: OutboundStatus,
        answer: Option<&str>,
    ) -> Result<Option<OutboundRecord>, StorageError> {
        let mut tx = self.pool.begin().await?;
        let identity_id = memory_identity(&mut tx).await?;
        let row = sqlx::query_as::<_, OutboundRow>(concat!(
            "UPDATE aster_outbound SET status = $2, answer = $3, resolved_at = clock_timestamp()
             WHERE id = $1 AND status = 'pending' RETURNING ",
            outbound_columns!()
        ))
        .bind(id)
        .bind(status.as_str())
        .bind(answer.map(bounded))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let record = outbound(row)?;
        audit(
            &mut tx,
            identity_id,
            "outbound_resolved",
            serde_json::json!({ "outbound_id": id, "status": status.as_str() }),
        )
        .await?;
        tx.commit().await?;
        Ok(Some(record))
    }

    pub async fn outbound(&self, id: Uuid) -> Result<Option<OutboundRecord>, StorageError> {
        sqlx::query_as::<_, OutboundRow>(concat!(
            "SELECT ",
            outbound_columns!(),
            " FROM aster_outbound WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .map(outbound)
        .transpose()
    }

    /// Keep only the newest `keep` mind snapshots; returns how many were removed.
    pub async fn prune_mind_snapshots(&self, keep: i64) -> Result<u64, StorageError> {
        Ok(sqlx::query(
            "DELETE FROM aster_mind_snapshots WHERE version NOT IN
             (SELECT version FROM aster_mind_snapshots ORDER BY version DESC LIMIT $1)",
        )
        .bind(keep.max(1))
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// Expire pending items created before `cutoff`; returns how many.
    pub async fn expire_outbound(&self, cutoff: DateTime<Utc>) -> Result<u64, StorageError> {
        Ok(sqlx::query(
            "UPDATE aster_outbound SET status = 'expired', resolved_at = clock_timestamp()
             WHERE status = 'pending' AND created_at < $1",
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await?
        .rows_affected())
    }

    /// Persist new levels and append history; `audit_kind` adds an audit event.
    pub async fn apply_regulation(
        &self,
        state: &RegulatoryState,
        cause: &str,
        audit_kind: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await?;
        let identity_id = memory_identity(&mut tx).await?;
        write_state(&mut tx, identity_id, state, cause).await?;
        if let Some(kind) = audit_kind {
            audit(
                &mut tx,
                identity_id,
                kind,
                serde_json::json!({ "cause": cause }),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn state_history(&self, limit: i64) -> Result<Vec<StateHistoryEntry>, StorageError> {
        sqlx::query_as::<_, (DateTime<Utc>, Value, String)>(
            "SELECT recorded_at, snapshot, cause FROM aster_state_history
             ORDER BY recorded_at DESC, id DESC LIMIT $1",
        )
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(|(recorded_at, snapshot, cause)| {
            Ok(StateHistoryEntry {
                recorded_at,
                state: serde_json::from_value(snapshot)?,
                cause,
            })
        })
        .collect()
    }

    pub async fn save_mind_snapshot(
        &self,
        engine: &str,
        reason: &str,
        model: &[u8],
        stats: Value,
    ) -> Result<SnapshotInfo, StorageError> {
        let mut tx = self.pool.begin().await?;
        let identity_id = memory_identity(&mut tx).await?;
        let row = sqlx::query_as::<_, SnapshotRow>(
            "INSERT INTO aster_mind_snapshots (identity_id, engine, reason, model, stats)
             VALUES ($1, $2, $3, $4, $5) RETURNING version, engine, reason, created_at, stats",
        )
        .bind(identity_id)
        .bind(engine)
        .bind(reason)
        .bind(model)
        .bind(stats)
        .fetch_one(&mut *tx)
        .await?;
        let info = snapshot_info(row);
        audit(
            &mut tx,
            identity_id,
            "mind_snapshot_saved",
            serde_json::json!({ "version": info.version, "reason": reason }),
        )
        .await?;
        tx.commit().await?;
        Ok(info)
    }

    /// The given version, or the latest when `version` is None.
    pub async fn mind_snapshot(
        &self,
        version: Option<i64>,
    ) -> Result<Option<StoredSnapshot>, StorageError> {
        let row = sqlx::query_as::<_, (i64, String, String, DateTime<Utc>, Value, Vec<u8>)>(
            "SELECT version, engine, reason, created_at, stats, model FROM aster_mind_snapshots
             WHERE ($1::bigint IS NULL OR version = $1) ORDER BY version DESC LIMIT 1",
        )
        .bind(version)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(version, engine, reason, created_at, stats, model)| StoredSnapshot {
                info: snapshot_info((version, engine, reason, created_at, stats)),
                model,
            },
        ))
    }

    pub async fn list_mind_snapshots(&self, limit: i64) -> Result<Vec<SnapshotInfo>, StorageError> {
        Ok(sqlx::query_as::<_, SnapshotRow>(
            "SELECT version, engine, reason, created_at, stats FROM aster_mind_snapshots
             ORDER BY version DESC LIMIT $1",
        )
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .map(snapshot_info)
        .collect())
    }

    /// Generic payload-free audit event for runtime operations.
    pub async fn record_audit(&self, kind: &str, metadata: Value) -> Result<(), StorageError> {
        let mut tx = self.pool.begin().await?;
        let identity_id = memory_identity(&mut tx).await?;
        audit(&mut tx, identity_id, kind, metadata).await?;
        tx.commit().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_respects_char_boundaries() {
        let text = "é".repeat(10_000);
        let cut = bounded(&text);
        assert!(cut.len() <= MAX_TURN_BYTES);
        assert!(cut.chars().all(|c| c == 'é'));
        assert_eq!(bounded("short"), "short");
    }
}
