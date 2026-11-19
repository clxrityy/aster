//! Opt-in isolated databases only. Exercises the full runtime pipeline.

use std::time::Duration;

use aster_core::{Decision, OutboundStatus};
use aster_runtime::{AsterConfig, ConfigHandle, Event, Mode, Runtime};
use aster_storage::Store;
use sqlx::PgPool;

fn test_config(name: &str) -> (ConfigHandle, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("aster-rt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("ingest")).unwrap();
    let mut config = AsterConfig::default();
    config.logs.dir = dir.join("logs").display().to_string();
    config.server.ingest_dir = dir.join("ingest").display().to_string();
    config.cognition.rng_seed = Some(1);
    // Deterministic choices: never explore the second-best candidate.
    config.cognition.exploration_min = 0.0;
    config.cognition.exploration_max = 0.0;
    config.runtime.tick_interval_ms = 100;
    config.runtime.idle_before_initiative_secs = 0;
    (ConfigHandle::from_config(config), dir)
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn chat_teach_feedback_answer_and_snapshots(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.initialize().await.unwrap();
    let (config, dir) = test_config("pipeline");
    let runtime = Runtime::start(store.clone(), config).await.unwrap();
    let mut events = runtime.subscribe();

    // Untrained: Aster asks, and the question lands in the inbox.
    let first = runtime
        .chat("Tell me about nebulae", Mode::Train)
        .await
        .unwrap();
    assert_eq!(first.trace.decision, Decision::Ask);
    let question = first.outbound.clone().unwrap();
    assert_eq!(question.status, OutboundStatus::Pending);
    assert!(store.thought(first.aster.id).await.unwrap().is_some());

    // Answering teaches the subject.
    runtime
        .answer(question.id, "clouds of gas and dust in space")
        .await
        .unwrap();
    let recalled = runtime.chat("what is nebulae", Mode::Chat).await.unwrap();
    assert_eq!(recalled.trace.decision, Decision::Reply);
    assert!(recalled.aster.text.contains("gas and dust"));

    // Teaching and feedback.
    runtime.teach("hello", "Hi, I'm Aster.").await.unwrap();
    let hello = runtime.chat("hello", Mode::Train).await.unwrap();
    assert_eq!(hello.aster.text, "Hi, I'm Aster.");
    runtime
        .feedback(hello.aster.id, -1, Some("Hello! Good to see you."))
        .await
        .unwrap();
    assert!(runtime.feedback(hello.owner.id, 1, None).await.is_err());
    let corrected = runtime.chat("hello", Mode::Chat).await.unwrap();
    assert_eq!(corrected.aster.text, "Hello! Good to see you.");

    // Snapshots persist learning across a restart and can roll back.
    let saved = runtime.snapshot("test").await.unwrap();
    runtime.shutdown().await;
    let (config, _) = test_config("restart");
    let restarted = Runtime::start(store.clone(), config).await.unwrap();
    assert_eq!(
        restarted
            .chat("hello", Mode::Chat)
            .await
            .unwrap()
            .aster
            .text,
        "Hello! Good to see you."
    );
    restarted.rollback(saved.version).await.unwrap();
    assert!(restarted.snapshots(10).await.unwrap().len() >= 2);

    // Levels history and audit are written; events were broadcast.
    assert!(!store.state_history(10).await.unwrap().is_empty());
    let mut saw_thought = false;
    while let Ok(envelope) = events.try_recv() {
        saw_thought |= matches!(envelope.event, Event::Thought { .. });
    }
    assert!(saw_thought);

    // JSONL mirrors exist for conversations and thoughts.
    tokio::time::sleep(Duration::from_millis(200)).await;
    for category in ["conversations", "thoughts", "levels", "requests"] {
        let entries = std::fs::read_dir(dir.join("logs").join(category)).unwrap();
        assert!(entries.count() > 0, "{category}");
    }
    restarted.shutdown().await;
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn idle_initiative_is_bounded_and_deduplicated(pool: PgPool) {
    let store = Store::from_pool(pool);
    store.initialize().await.unwrap();
    let (config, _) = test_config("initiative");
    let runtime = Runtime::start(store.clone(), config).await.unwrap();
    for _ in 0..5 {
        runtime.tick().await.unwrap();
    }
    let pending = runtime
        .outbound(Some(OutboundStatus::Pending), 50)
        .await
        .unwrap();
    // Only the one "teach me first" question; never duplicates.
    assert_eq!(pending.len(), 1);
    assert!(pending[0].draft.body.contains("teach me first"));
    assert!(runtime.decide(pending[0].id, true).await.is_err());
    runtime.shutdown().await;
}
