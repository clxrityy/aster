//! Opt-in isolated databases only. Never load the owner's CLI .env here.

use aster_core::{MemoryCursor, MemoryDraft, MemoryId, MemoryPageSize};
use aster_storage::{StorageError, Store};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::task::JoinSet;
use uuid::Uuid;

fn draft() -> MemoryDraft {
    MemoryDraft::new(
        "Private project context".into(),
        "Owner's local project note".into(),
    )
    .unwrap()
}

async fn memory_audits(pool: &PgPool) -> Vec<(String, Value)> {
    sqlx::query_as(
        "SELECT kind, metadata FROM aster_audit_events
         WHERE kind IN ('memory_created', 'memory_deleted') ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn memory_persists_and_delete_keeps_only_payload_free_audit(pool: PgPool) {
    let options = pool.connect_options().as_ref().clone();
    let store = Store::from_pool(pool.clone());
    let identity = store.initialize().await.unwrap().identity;
    let state = store.regulatory_state().await.unwrap().unwrap();
    let record = store.create_memory(draft()).await.unwrap();
    assert_eq!(record.content(), draft().content());
    assert_eq!(record.provenance(), draft().provenance());
    assert_eq!(record.created_at().timestamp_subsec_nanos() % 1000, 0);
    let owner: Uuid = sqlx::query_scalar("SELECT identity_id FROM aster_memories WHERE id = $1")
        .bind(record.id().get())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(owner, identity.id);
    store.migrate().await.unwrap();
    assert!(!store.initialize().await.unwrap().created);
    assert_eq!(store.identity().await.unwrap(), Some(identity));
    assert_eq!(store.regulatory_state().await.unwrap(), Some(state));
    assert_eq!(
        memory_audits(&pool).await,
        vec![("memory_created".into(), json!({"memory_id":record.id()}))]
    );
    store.close().await;
    let reopened_pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let reopened = Store::from_pool(reopened_pool.clone());
    let page = reopened
        .list_memories(MemoryPageSize::default(), None)
        .await
        .unwrap();
    assert_eq!(page.memories, vec![record.clone()]);
    assert!(page.next_cursor.is_none());
    assert!(reopened.delete_memory(record.id()).await.unwrap());
    assert!(!reopened.delete_memory(record.id()).await.unwrap());
    assert!(
        reopened
            .list_memories(MemoryPageSize::default(), None)
            .await
            .unwrap()
            .memories
            .is_empty()
    );
    assert_eq!(
        memory_audits(&reopened_pool).await,
        vec![
            ("memory_created".into(), json!({"memory_id":record.id()})),
            ("memory_deleted".into(), json!({"memory_id":record.id()})),
        ]
    );
    assert!(reopened.is_ready().await.unwrap());
    reopened.close().await;
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn memory_mutations_roll_back_when_their_audit_fails(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.initialize().await.unwrap();
    sqlx::query("ALTER TABLE aster_audit_events ADD CONSTRAINT injected_create_audit_failure CHECK (kind <> 'memory_created')")
        .execute(&pool).await.unwrap();
    assert!(matches!(
        store.create_memory(draft()).await,
        Err(StorageError::Database(_))
    ));
    assert!(
        store
            .list_memories(MemoryPageSize::default(), None)
            .await
            .unwrap()
            .memories
            .is_empty()
    );
    assert!(memory_audits(&pool).await.is_empty());
    sqlx::query("ALTER TABLE aster_audit_events DROP CONSTRAINT injected_create_audit_failure")
        .execute(&pool)
        .await
        .unwrap();
    let memory = store.create_memory(draft()).await.unwrap();
    sqlx::query("ALTER TABLE aster_audit_events ADD CONSTRAINT injected_delete_audit_failure CHECK (kind <> 'memory_deleted')")
        .execute(&pool).await.unwrap();
    assert!(matches!(
        store.delete_memory(memory.id()).await,
        Err(StorageError::Database(_))
    ));
    assert_eq!(
        store
            .list_memories(MemoryPageSize::default(), None)
            .await
            .unwrap()
            .memories,
        vec![memory]
    );
    assert_eq!(memory_audits(&pool).await.len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn concurrent_deletes_emit_exactly_one_audit(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.initialize().await.unwrap();
    let memory = store.create_memory(draft()).await.unwrap();
    let mut workers = JoinSet::new();
    for _ in 0..12 {
        let store = store.clone();
        let id = memory.id();
        workers.spawn(async move { store.delete_memory(id).await.unwrap() });
    }
    let mut deletes = 0;
    while let Some(result) = workers.join_next().await {
        deletes += usize::from(result.unwrap());
    }
    assert_eq!(deletes, 1);
    assert_eq!(memory_audits(&pool).await.len(), 2);
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn keyset_pages_handle_ties_new_inserts_and_deleted_boundary(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.initialize().await.unwrap();
    let timestamp = DateTime::<Utc>::from_timestamp(1_800_000_000, 123_456_000).unwrap();
    let mut records = Vec::new();
    for _ in 0..5 {
        records.push(store.create_memory(draft()).await.unwrap());
    }
    sqlx::query("UPDATE aster_memories SET created_at = $1")
        .bind(timestamp)
        .execute(&pool)
        .await
        .unwrap();
    records.sort_by_key(|record| std::cmp::Reverse(record.id().get()));
    let limit = MemoryPageSize::new(2).unwrap();
    let first = store.list_memories(limit, None).await.unwrap();
    assert_eq!(
        first
            .memories
            .iter()
            .map(|record| record.id())
            .collect::<Vec<_>>(),
        records[..2]
            .iter()
            .map(|record| record.id())
            .collect::<Vec<_>>()
    );
    let cursor = first.next_cursor.unwrap().parse::<MemoryCursor>().unwrap();
    assert!(store.delete_memory(cursor.id()).await.unwrap());
    let inserted = store.create_memory(draft()).await.unwrap();
    sqlx::query("UPDATE aster_memories SET created_at = $1 WHERE id = $2")
        .bind(timestamp + chrono::Duration::seconds(1))
        .bind(inserted.id().get())
        .execute(&pool)
        .await
        .unwrap();
    let second = store.list_memories(limit, Some(cursor)).await.unwrap();
    assert_eq!(
        second
            .memories
            .iter()
            .map(|record| record.id())
            .collect::<Vec<_>>(),
        records[2..4]
            .iter()
            .map(|record| record.id())
            .collect::<Vec<_>>()
    );
    let third = store
        .list_memories(limit, Some(second.next_cursor.unwrap().parse().unwrap()))
        .await
        .unwrap();
    assert_eq!(third.memories.len(), 1);
    assert_eq!(third.memories[0].id(), records[4].id());
    assert!(third.next_cursor.is_none());
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn memory_requires_existing_valid_foundation_without_implicit_repair(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let id = MemoryId::new(Uuid::new_v4()).unwrap();
    assert!(matches!(
        store.create_memory(draft()).await,
        Err(StorageError::NotInitialized)
    ));
    assert!(matches!(
        store.list_memories(MemoryPageSize::default(), None).await,
        Err(StorageError::NotInitialized)
    ));
    assert!(matches!(
        store.delete_memory(id).await,
        Err(StorageError::NotInitialized)
    ));
    assert_eq!(store.identity().await.unwrap(), None);
    store.initialize().await.unwrap();
    let memory = store.create_memory(draft()).await.unwrap();
    sqlx::query("DELETE FROM aster_audit_events WHERE kind = 'identity_initialized'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.create_memory(draft()).await,
        Err(StorageError::InconsistentFoundation)
    ));
    assert!(matches!(
        store.list_memories(MemoryPageSize::default(), None).await,
        Err(StorageError::InconsistentFoundation)
    ));
    assert!(matches!(
        store.delete_memory(memory.id()).await,
        Err(StorageError::InconsistentFoundation)
    ));
    assert_eq!(memory_audits(&pool).await.len(), 1);
    sqlx::query("INSERT INTO aster_audit_events (identity_id, kind, metadata) SELECT id, 'identity_initialized', '{\"revision\":1}'::jsonb FROM aster_identity")
        .execute(&pool).await.unwrap();
    sqlx::query("DELETE FROM aster_regulatory_state")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.create_memory(draft()).await,
        Err(StorageError::InconsistentFoundation)
    ));
    assert!(matches!(
        store.list_memories(MemoryPageSize::default(), None).await,
        Err(StorageError::InconsistentFoundation)
    ));
    assert!(matches!(
        store.delete_memory(memory.id()).await,
        Err(StorageError::InconsistentFoundation)
    ));
    assert_eq!(memory_audits(&pool).await.len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn corrupt_identity_prevents_memory_mutation(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.initialize().await.unwrap();
    let memory = store.create_memory(draft()).await.unwrap();
    sqlx::query("UPDATE aster_identity SET snapshot = snapshot #- '{protected,purpose}'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.create_memory(draft()).await,
        Err(StorageError::Snapshot(_))
    ));
    assert!(matches!(
        store.list_memories(MemoryPageSize::default(), None).await,
        Err(StorageError::Snapshot(_))
    ));
    assert!(matches!(
        store.delete_memory(memory.id()).await,
        Err(StorageError::Snapshot(_))
    ));
    assert_eq!(memory_audits(&pool).await.len(), 1);
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn corrupt_memory_is_rejected_but_owner_can_delete_without_repair(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.initialize().await.unwrap();
    let memory = store.create_memory(draft()).await.unwrap();
    // Basic SQL shape constraints are not the complete Rust validation boundary.
    sqlx::query("UPDATE aster_memories SET provenance = $1 WHERE id = $2")
        .bind("\u{2003}")
        .bind(memory.id().get())
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.list_memories(MemoryPageSize::default(), None).await,
        Err(StorageError::InvalidStoredMemory(_))
    ));
    assert!(store.is_ready().await.unwrap());
    assert!(!store.initialize().await.unwrap().created);
    assert!(store.delete_memory(memory.id()).await.unwrap());
    assert_eq!(memory_audits(&pool).await.len(), 2);
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn memory_constraints_enforce_identity_metadata_and_basic_text_bounds(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.initialize().await.unwrap();
    // PostgreSQL escape strings require octal \013, not \v (which means 'v').
    let memory = store
        .create_memory(MemoryDraft::new("v".into(), "v".into()).unwrap())
        .await
        .unwrap();
    for (content, provenance) in [
        ("".into(), "note".into()),
        (" \t\n".into(), "note".into()),
        ("\u{b}".into(), "note".into()),
        ("x".repeat(16385), "note".into()),
        ("text".into(), "".into()),
        ("text".into(), "\u{b}".into()),
        ("text".into(), "x".repeat(1025)),
    ] {
        assert!(
            sqlx::query("UPDATE aster_memories SET content = $1, provenance = $2 WHERE id = $3")
                .bind(content)
                .bind(provenance)
                .bind(memory.id().get())
                .execute(&pool)
                .await
                .is_err()
        );
    }
    assert!(
        sqlx::query("UPDATE aster_memories SET id = $1")
            .bind(Uuid::nil())
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE aster_memories SET identity_id = $1")
            .bind(Uuid::new_v4())
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE aster_memories SET created_at = 'infinity'::timestamptz")
            .execute(&pool)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .list_memories(MemoryPageSize::default(), None)
            .await
            .unwrap()
            .memories,
        vec![memory]
    );
}
