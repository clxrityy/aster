//! Trusted callers may manage owner memories; authentication belongs in the API.
//! Mutations and payload-free audit events share a transaction. No automatic
//! initialization, capture, repair, or autonomous writer is provided.

use aster_core::{
    MemoryCursor, MemoryDraft, MemoryId, MemoryPageSize, MemoryRecord, RegulatoryState,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{StorageError, Store, decode_identity};

#[derive(Debug, Serialize)]
pub struct MemoryPage {
    pub memories: Vec<MemoryRecord>,
    pub next_cursor: Option<String>,
}

impl Store {
    pub async fn create_memory(&self, draft: MemoryDraft) -> Result<MemoryRecord, StorageError> {
        let mut tx = self.pool.begin().await?;
        let identity_id = memory_identity(&mut tx).await?;
        let row = sqlx::query_as::<_, (Uuid, DateTime<Utc>, String, String)>(
            "INSERT INTO aster_memories (id, identity_id, content, provenance)
             VALUES ($1, $2, $3, $4) RETURNING id, created_at, content, provenance",
        )
        .bind(Uuid::new_v4())
        .bind(identity_id)
        .bind(draft.content())
        .bind(draft.provenance())
        .fetch_one(&mut *tx)
        .await?;
        let memory = decode_memory(row)?;
        audit_memory(&mut tx, identity_id, "memory_created", memory.id()).await?;
        tx.commit().await?;
        Ok(memory)
    }

    pub async fn list_memories(
        &self,
        limit: MemoryPageSize,
        before: Option<MemoryCursor>,
    ) -> Result<MemoryPage, StorageError> {
        let mut tx = self.pool.begin().await?;
        let identity_id = memory_identity(&mut tx).await?;
        // Fetch one extra row to distinguish the last page without a count scan.
        // A tuple boundary also works when the previous page's last row is deleted.
        let rows = sqlx::query_as::<_, (Uuid, DateTime<Utc>, String, String)>(
            "SELECT id, created_at, content, provenance FROM aster_memories
             WHERE identity_id = $1
               AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3::uuid))
             ORDER BY created_at DESC, id DESC LIMIT $4",
        )
        .bind(identity_id)
        .bind(before.map(MemoryCursor::created_at))
        .bind(before.map(|cursor| cursor.id().get()))
        .bind(i64::from(limit.get()) + 1)
        .fetch_all(&mut *tx)
        .await?;
        let mut memories = rows
            .into_iter()
            .map(decode_memory)
            .collect::<Result<Vec<_>, _>>()?;
        let has_more = memories.len() > usize::from(limit.get());
        memories.truncate(usize::from(limit.get()));
        let next_cursor = if has_more {
            memories
                .last()
                .map(|record| MemoryCursor::from_record(record).encode())
        } else {
            None
        };
        tx.commit().await?;
        Ok(MemoryPage {
            memories,
            next_cursor,
        })
    }

    /// Hard-delete only the current identity's row. A repeated/concurrent delete
    /// reports not found and does not append a second audit event. Payloads are
    /// not read, so an owner can also remove a corrupt memory without repair.
    pub async fn delete_memory(&self, id: MemoryId) -> Result<bool, StorageError> {
        let mut tx = self.pool.begin().await?;
        let identity_id = memory_identity(&mut tx).await?;
        let deleted = sqlx::query("DELETE FROM aster_memories WHERE identity_id = $1 AND id = $2")
            .bind(identity_id)
            .bind(id.get())
            .execute(&mut *tx)
            .await?
            .rows_affected()
            == 1;
        if deleted {
            audit_memory(&mut tx, identity_id, "memory_deleted", id).await?;
        }
        tx.commit().await?;
        Ok(deleted)
    }
}

pub(crate) async fn memory_identity(connection: &mut PgConnection) -> Result<Uuid, StorageError> {
    // Never accept an identity UUID from a request or mutate an incomplete
    // foundation. A shared row lock keeps the identity key stable until commit;
    // database administrators remain an explicit trust boundary.
    let row = sqlx::query_as::<_, (Uuid, DateTime<Utc>, Value, Option<Value>, i64)>(
        "SELECT i.id, i.created_at, i.snapshot, s.snapshot,
                (SELECT count(*) FROM aster_audit_events a
                 WHERE a.identity_id = i.id AND a.kind = 'identity_initialized')
         FROM aster_identity i
         LEFT JOIN aster_regulatory_state s ON s.identity_id = i.id
         WHERE i.singleton = TRUE FOR SHARE OF i",
    )
    .fetch_optional(connection)
    .await?
    .ok_or(StorageError::NotInitialized)?;
    let (id, created_at, identity, state, audit_count) = row;
    decode_identity((id, created_at, identity))?;
    serde_json::from_value::<RegulatoryState>(state.ok_or(StorageError::InconsistentFoundation)?)?;
    if audit_count != 1 {
        return Err(StorageError::InconsistentFoundation);
    }
    Ok(id)
}

async fn audit_memory(
    connection: &mut PgConnection,
    identity_id: Uuid,
    kind: &'static str,
    id: MemoryId,
) -> Result<(), StorageError> {
    sqlx::query("INSERT INTO aster_audit_events (identity_id, kind, metadata) VALUES ($1, $2, $3)")
        .bind(identity_id)
        .bind(kind)
        .bind(serde_json::json!({ "memory_id": id }))
        .execute(connection)
        .await?;
    Ok(())
}

fn decode_memory(
    (id, created_at, content, provenance): (Uuid, DateTime<Utc>, String, String),
) -> Result<MemoryRecord, StorageError> {
    Ok(MemoryRecord::restore(
        MemoryId::new(id)?,
        created_at,
        MemoryDraft::new(content, provenance)?,
    ))
}
