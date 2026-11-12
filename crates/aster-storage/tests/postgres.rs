//! Each ignored SQLx test gets an isolated migrated database. Opt in with
//! --include-ignored and DATABASE_URL pointing to a PostgreSQL test server.
//! Direct SQL mutations below model imported/corrupt data and test-only faults;
//! production storage exposes no identity or regulatory mutation methods.

use std::sync::Arc;

use aster_core::{Level, RegulatoryState};
use aster_storage::{StorageError, Store};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{sync::Barrier, task::JoinSet};
use uuid::Uuid;

async fn counts(pool: &PgPool) -> (i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM aster_identity),
                (SELECT count(*) FROM aster_regulatory_state),
                (SELECT count(*) FROM aster_audit_events)",
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn initialize_is_idempotent(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let first = store.initialize().await.unwrap();
    let state = store.regulatory_state().await.unwrap().unwrap();
    assert!(first.created);
    for _ in 0..3 {
        let next = store.initialize().await.unwrap();
        assert!(!next.created);
        assert_eq!(next.identity, first.identity);
        assert_eq!(store.regulatory_state().await.unwrap().unwrap(), state);
    }
    assert_eq!(counts(&pool).await, (1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn concurrent_initializers_share_one_foundation(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let barrier = Arc::new(Barrier::new(12));
    let mut workers = JoinSet::new();
    for _ in 0..12 {
        let store = store.clone();
        let barrier = barrier.clone();
        workers.spawn(async move {
            barrier.wait().await;
            store.initialize().await.unwrap()
        });
    }
    let mut identities = Vec::new();
    let mut created_count = 0;
    while let Some(result) = workers.join_next().await {
        let outcome = result.unwrap();
        created_count += usize::from(outcome.created);
        identities.push(outcome.identity);
    }
    assert_eq!(created_count, 1);
    assert!(identities.iter().all(|identity| identity == &identities[0]));
    assert_eq!(counts(&pool).await, (1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn fresh_pool_after_close_restores_same_identity(pool: PgPool) {
    let options = pool.connect_options().as_ref().clone();
    let store = Store::from_pool(pool);
    let first = store.initialize().await.unwrap().identity;
    let state = store.regulatory_state().await.unwrap().unwrap();
    store.close().await;
    let reopened_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .unwrap();
    let reopened = Store::from_pool(reopened_pool);
    assert_eq!(reopened.identity().await.unwrap(), Some(first.clone()));
    assert_eq!(reopened.regulatory_state().await.unwrap(), Some(state));
    let outcome = reopened.initialize().await.unwrap();
    assert!(!outcome.created);
    assert_eq!(outcome.identity, first);
    reopened.close().await;
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn numeric_state_and_timestamp_survive_reads_and_initialize(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let identity = store.initialize().await.unwrap().identity;
    let state = RegulatoryState {
        energy: Level::new(0.0).unwrap(),
        focus: Level::new(100.0).unwrap(),
        confidence: Level::new(42.125).unwrap(),
        ..RegulatoryState::default()
    };
    let updated_at = DateTime::<Utc>::from_timestamp(1_800_000_000, 123_456_000).unwrap();
    let snapshot = serde_json::to_value(&state).unwrap();
    assert!(snapshot.as_object().unwrap().values().all(Value::is_number));
    sqlx::query(
        "UPDATE aster_regulatory_state SET snapshot = $1, updated_at = $2 WHERE identity_id = $3",
    )
    .bind(snapshot)
    .bind(updated_at)
    .bind(identity.id)
    .execute(&pool)
    .await
    .unwrap();
    let restored = store.regulatory_state().await.unwrap().unwrap();
    assert_eq!(restored.state, state);
    assert_eq!(restored.updated_at, updated_at);
    assert!(!store.initialize().await.unwrap().created);
    assert_eq!(store.regulatory_state().await.unwrap().unwrap(), restored);
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn initialization_audit_is_single_and_minimal(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let identity = store.initialize().await.unwrap().identity;
    store.initialize().await.unwrap();
    let audits = sqlx::query_as::<_, (Uuid, String, Value, DateTime<Utc>)>(
        "SELECT identity_id, kind, metadata, recorded_at FROM aster_audit_events",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(audits.len(), 1);
    assert_eq!(audits[0].0, identity.id);
    assert_eq!(audits[0].1, "identity_initialized");
    assert_eq!(audits[0].2, json!({ "revision": 1 }));
    assert!(audits[0].3 <= Utc::now());
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn failing_audit_rolls_back_identity_and_state(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    // A valid empty-table constraint deliberately rejects the final insert.
    // No production constraint is disabled or relaxed for this fault injection.
    sqlx::query(
        "ALTER TABLE aster_audit_events ADD CONSTRAINT injected_audit_failure
         CHECK (kind <> 'identity_initialized')",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        store.initialize().await,
        Err(StorageError::Database(_))
    ));
    assert_eq!(counts(&pool).await, (0, 0, 0));
    assert_eq!(store.identity().await.unwrap(), None);
    assert_eq!(store.regulatory_state().await.unwrap(), None);
    assert!(!store.is_ready().await.unwrap());
    sqlx::query("ALTER TABLE aster_audit_events DROP CONSTRAINT injected_audit_failure")
        .execute(&pool)
        .await
        .unwrap();
    assert!(store.initialize().await.unwrap().created);
    assert_eq!(counts(&pool).await, (1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn readiness_and_reads_do_not_implicitly_initialize(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    assert_eq!(store.identity().await.unwrap(), None);
    assert_eq!(store.regulatory_state().await.unwrap(), None);
    assert!(!store.is_ready().await.unwrap());
    store.migrate().await.unwrap();
    store.migrate().await.unwrap();
    assert_eq!(counts(&pool).await, (0, 0, 0));
    store.initialize().await.unwrap();
    assert!(store.is_ready().await.unwrap());
    assert_eq!(counts(&pool).await, (1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn graceful_close_affects_clones_and_readiness_queries_database(pool: PgPool) {
    let store = Store::from_pool(pool);
    let cloned = store.clone();
    store.initialize().await.unwrap();
    assert!(cloned.is_ready().await.unwrap());
    store.close().await;
    assert!(matches!(
        cloned.is_ready().await,
        Err(StorageError::Database(_))
    ));
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn corrupt_identity_snapshot_is_rejected_without_reset(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let identity = store.initialize().await.unwrap().identity;
    sqlx::query("UPDATE aster_identity SET snapshot = snapshot #- '{protected,purpose}'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.identity().await,
        Err(StorageError::Snapshot(_))
    ));
    assert!(matches!(
        store.initialize().await,
        Err(StorageError::Snapshot(_))
    ));
    assert!(store.is_ready().await.is_err());
    let stored_id: Uuid = sqlx::query_scalar("SELECT id FROM aster_identity")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored_id, identity.id);
    assert_eq!(counts(&pool).await, (1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn every_regulatory_field_rejects_invalid_persisted_levels(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.initialize().await.unwrap();
    let valid = serde_json::to_value(RegulatoryState::default()).unwrap();
    for field in [
        "energy",
        "focus",
        "cognitive_load",
        "curiosity",
        "stimulation",
        "stress",
        "confidence",
    ] {
        for invalid in [
            json!(-0.001),
            json!(100.001),
            json!("NaN"),
            json!("Infinity"),
            Value::Null,
        ] {
            let mut snapshot = valid.clone();
            snapshot[field] = invalid;
            sqlx::query("UPDATE aster_regulatory_state SET snapshot = $1")
                .bind(snapshot)
                .execute(&pool)
                .await
                .unwrap();
            assert!(matches!(
                store.regulatory_state().await,
                Err(StorageError::Snapshot(_))
            ));
            assert!(matches!(
                store.initialize().await,
                Err(StorageError::Snapshot(_))
            ));
            assert!(store.is_ready().await.is_err());
            assert_eq!(counts(&pool).await, (1, 1, 1));
        }
    }
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn missing_state_is_not_repaired(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let identity = store.initialize().await.unwrap().identity;
    sqlx::query("DELETE FROM aster_regulatory_state")
        .execute(&pool)
        .await
        .unwrap();
    assert!(!store.is_ready().await.unwrap());
    assert_eq!(store.regulatory_state().await.unwrap(), None);
    assert!(matches!(
        store.initialize().await,
        Err(StorageError::InconsistentFoundation)
    ));
    assert_eq!(store.identity().await.unwrap(), Some(identity));
    assert_eq!(counts(&pool).await, (1, 0, 1));
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn missing_or_duplicate_initialization_audit_is_not_repaired(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let identity = store.initialize().await.unwrap().identity;
    sqlx::query("DELETE FROM aster_audit_events")
        .execute(&pool)
        .await
        .unwrap();
    assert!(matches!(
        store.is_ready().await,
        Err(StorageError::InconsistentFoundation)
    ));
    assert!(matches!(
        store.initialize().await,
        Err(StorageError::InconsistentFoundation)
    ));
    assert_eq!(counts(&pool).await, (1, 1, 0));
    sqlx::query(
        "INSERT INTO aster_audit_events (identity_id, kind, metadata)
         VALUES ($1, 'identity_initialized', '{\"revision\":1}'::jsonb),
                ($1, 'identity_initialized', '{\"revision\":1}'::jsonb)",
    )
    .bind(identity.id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(matches!(
        store.is_ready().await,
        Err(StorageError::InconsistentFoundation)
    ));
    assert!(matches!(
        store.initialize().await,
        Err(StorageError::InconsistentFoundation)
    ));
    assert_eq!(counts(&pool).await, (1, 1, 2));
    assert_eq!(store.identity().await.unwrap(), Some(identity));
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn singleton_constraint_rejects_false_and_second_identity(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let identity = store.initialize().await.unwrap().identity;
    assert!(
        sqlx::query("UPDATE aster_identity SET singleton = FALSE")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO aster_identity (id, created_at, snapshot)
         SELECT id, created_at, snapshot FROM aster_identity",
        )
        .execute(&pool)
        .await
        .is_err()
    );
    assert_eq!(store.identity().await.unwrap(), Some(identity));
    assert_eq!(counts(&pool).await, (1, 1, 1));
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn identity_constraints_reject_missing_or_mismatched_metadata(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let identity = store.initialize().await.unwrap().identity;
    let valid = serde_json::to_value(&identity).unwrap();
    let mut missing_id = valid.clone();
    missing_id.as_object_mut().unwrap().remove("id");
    let mut wrong_id = valid.clone();
    wrong_id["id"] = json!(Uuid::new_v4());
    let mut missing_date = valid.clone();
    missing_date.as_object_mut().unwrap().remove("created_at");
    let mut wrong_date = valid.clone();
    wrong_date["created_at"] = json!(identity.created_at + chrono::Duration::seconds(1));
    let mut invalid_date = valid.clone();
    invalid_date["created_at"] = json!("not a timestamp");
    let mut null_id = valid.clone();
    null_id["id"] = Value::Null;
    let mut null_date = valid;
    null_date["created_at"] = Value::Null;
    for invalid in [
        missing_id,
        wrong_id,
        missing_date,
        wrong_date,
        invalid_date,
        null_id,
        null_date,
        json!([]),
    ] {
        assert!(
            sqlx::query("UPDATE aster_identity SET snapshot = $1")
                .bind(invalid)
                .execute(&pool)
                .await
                .is_err()
        );
    }
    assert_eq!(store.identity().await.unwrap(), Some(identity));
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn snapshots_and_audit_metadata_must_be_objects(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.initialize().await.unwrap();
    for invalid in [Value::Null, json!([]), json!(42), json!("snapshot")] {
        assert!(
            sqlx::query("UPDATE aster_regulatory_state SET snapshot = $1")
                .bind(invalid.clone())
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("UPDATE aster_audit_events SET metadata = $1")
                .bind(invalid)
                .execute(&pool)
                .await
                .is_err()
        );
    }
    assert!(store.is_ready().await.unwrap());
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn state_and_audit_foreign_keys_require_the_identity(pool: PgPool) {
    let orphan = Uuid::new_v4();
    assert!(
        sqlx::query("INSERT INTO aster_regulatory_state (identity_id, snapshot) VALUES ($1, $2)",)
            .bind(orphan)
            .bind(serde_json::to_value(RegulatoryState::default()).unwrap())
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(sqlx::query(
        "INSERT INTO aster_audit_events (identity_id, kind, metadata) VALUES ($1, 'test', '{}')",
    )
    .bind(orphan)
    .execute(&pool)
    .await
    .is_err());
    assert_eq!(counts(&pool).await, (0, 0, 0));
}
