//! The persistent runtime: turn pipeline, learning operations, regulation
//! ticks, initiative, and mind snapshots. Owns no truth of its own beyond the
//! in-memory mind, which is snapshotted to PostgreSQL.

use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aster_core::{
    Cognition, LearnOutcome, LearningSignal, MemoryCursor, MemoryPageSize, MemoryRecord,
    OutboundDraft, OutboundStatus, RegulationEvent, RegulatoryState, RequestEffect, ThoughtTrace,
    TurnContext,
};
use aster_storage::{Exchange, OutboundRecord, SnapshotInfo, StorageError, Store, TurnRecord};
use chrono::Utc;
use serde::Serialize;
use thiserror::Error;
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

use crate::{
    config::ConfigHandle,
    events::{Envelope, Event},
    logs,
};

pub const MAX_TEXT_BYTES: usize = 16 * 1024;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("storage unavailable")]
    Storage(#[from] StorageError),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("Aster has not been initialized")]
    NotInitialized,
    #[error("mind snapshot could not be restored")]
    Snapshot,
    #[error("{0}")]
    Config(#[from] crate::config::ConfigError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Chat,
    Train,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Chat => "chat",
            Mode::Train => "train",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatResult {
    pub owner: TurnRecord,
    pub aster: TurnRecord,
    pub trace: ThoughtTrace,
    pub outbound: Option<OutboundRecord>,
    pub levels: RegulatoryState,
}

#[derive(Debug, Clone, Serialize)]
pub struct LearnResult {
    pub note: String,
    pub levels: RegulatoryState,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeStatus {
    pub engine: String,
    pub paused: bool,
    pub resting: bool,
    pub levels: RegulatoryState,
    pub mind: BTreeMap<String, f64>,
    pub idle_secs: u64,
}

struct Inner {
    levels: RegulatoryState,
    persisted: RegulatoryState,
    last_activity: Instant,
    last_decay: Instant,
    outbound_times: VecDeque<Instant>,
    updates_since_snapshot: u32,
    rest_until: Option<Instant>,
    conversation_id: Uuid,
}

pub struct Runtime {
    store: Store,
    config: ConfigHandle,
    bus: broadcast::Sender<Envelope>,
    mind: tokio::sync::Mutex<Box<dyn Cognition>>,
    inner: Mutex<Inner>,
    stop: watch::Sender<bool>,
    log_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

fn validate_text(name: &str, text: &str) -> Result<String, RuntimeError> {
    let trimmed = text.trim();
    if trimmed.is_empty() || text.len() > MAX_TEXT_BYTES || text.contains('\0') {
        return Err(RuntimeError::Invalid(format!(
            "{name} must be nonblank, NUL-free, and at most {MAX_TEXT_BYTES} bytes"
        )));
    }
    Ok(trimmed.to_string())
}

impl Runtime {
    /// Load levels and the latest mind snapshot, then start ticking.
    pub async fn start(store: Store, config: ConfigHandle) -> Result<Arc<Self>, RuntimeError> {
        let cfg = config.get();
        let stored = store
            .regulatory_state()
            .await?
            .ok_or(RuntimeError::NotInitialized)?;
        let levels = cfg.regulation.bound(&stored.state);
        let (mind, fresh) = match store.mind_snapshot(None).await? {
            Some(snapshot) => (
                aster_mind::build(&cfg.cognition, Some(&snapshot.model))
                    .map_err(|_| RuntimeError::Snapshot)?,
                false,
            ),
            None => (
                aster_mind::build(&cfg.cognition, None).map_err(|_| RuntimeError::Snapshot)?,
                true,
            ),
        };
        let (bus, _) = broadcast::channel(1024);
        let (stop, _) = watch::channel(false);
        let now = Instant::now();
        let runtime = Arc::new(Self {
            store,
            config: config.clone(),
            bus: bus.clone(),
            mind: tokio::sync::Mutex::new(mind),
            inner: Mutex::new(Inner {
                levels: levels.clone(),
                persisted: levels,
                last_activity: now,
                last_decay: now,
                outbound_times: VecDeque::new(),
                updates_since_snapshot: 0,
                rest_until: None,
                conversation_id: Uuid::new_v4(),
            }),
            stop,
            log_task: Mutex::new(None),
        });
        *runtime.log_task.lock().unwrap_or_else(|e| e.into_inner()) = Some(logs::spawn_writer(
            config.clone(),
            bus.subscribe(),
            runtime.stop.subscribe(),
        ));
        config.spawn_watcher(bus, runtime.stop.subscribe());

        if fresh {
            runtime.index_existing_memories().await?;
        }
        let expired = runtime
            .store
            .expire_outbound(
                Utc::now() - chrono::Duration::hours(cfg.runtime.outbound_expiry_hours as i64),
            )
            .await?;
        runtime.emit(Event::Runtime {
            message: format!(
                "runtime started ({} mind, {expired} stale requests expired)",
                if fresh { "new" } else { "restored" },
            ),
            paused: cfg.runtime.paused,
        });
        runtime.clone().spawn_ticker();
        Ok(runtime)
    }

    /// Stop background tasks and save a final snapshot.
    pub async fn shutdown(&self) {
        if let Err(error) = self.snapshot("shutdown").await {
            tracing::warn!(%error, "unable to save mind snapshot at shutdown");
        }
        self.emit(Event::Runtime {
            message: "runtime stopping".into(),
            paused: self.config.get().runtime.paused,
        });
        let _ = self.stop.send(true);
        let task = self
            .log_task
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(task) = task {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
    }

    pub fn config(&self) -> &ConfigHandle {
        &self.config
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Envelope> {
        self.bus.subscribe()
    }

    fn emit(&self, event: Event) {
        let _ = self.bus.send(Envelope::now(event));
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn levels(&self) -> RegulatoryState {
        self.inner().levels.clone()
    }

    async fn index_existing_memories(&self) -> Result<(), RuntimeError> {
        let params = self.config.get().cognition.clone();
        let mut cursor: Option<MemoryCursor> = None;
        let mut mind = self.mind.lock().await;
        loop {
            let page = self
                .store
                .list_memories(MemoryPageSize::new(100).unwrap_or_default(), cursor)
                .await?;
            for memory in &page.memories {
                mind.learn(
                    LearningSignal::Memory {
                        id: memory.id().get().to_string(),
                        content: memory.content().to_string(),
                    },
                    &params,
                );
            }
            match page.next_cursor.and_then(|c| c.parse().ok()) {
                Some(next) => cursor = Some(next),
                None => return Ok(()),
            }
        }
    }

    /// Apply events to in-memory levels; returns the new levels.
    fn regulate(&self, events: &[RegulationEvent]) -> RegulatoryState {
        let rules = self.config.get().regulation.clone();
        let mut inner = self.inner();
        let mut levels = rules.bound(&inner.levels);
        for event in events {
            levels = rules.apply(&levels, *event);
        }
        inner.levels = levels.clone();
        levels
    }

    async fn persist_levels(&self, cause: &str) -> Result<RegulatoryState, RuntimeError> {
        let levels = self.levels();
        self.store.apply_regulation(&levels, cause, None).await?;
        self.inner().persisted = levels.clone();
        self.emit(Event::Levels {
            state: levels.clone(),
            cause: cause.into(),
        });
        Ok(levels)
    }

    async fn after_updates(&self, count: u32) {
        let every = self.config.get().cognition.snapshot_every_updates;
        let due = {
            let mut inner = self.inner();
            inner.updates_since_snapshot += count;
            inner.updates_since_snapshot >= every
        };
        if due && let Err(error) = self.snapshot("periodic").await {
            tracing::warn!(%error, "periodic mind snapshot failed");
        }
    }

    fn cause(events: &[RegulationEvent]) -> String {
        events
            .iter()
            .map(|e| e.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    // ----- conversation -------------------------------------------------

    pub async fn chat(&self, text: &str, mode: Mode) -> Result<ChatResult, RuntimeError> {
        let text = validate_text("text", text)?;
        let cfg = self.config.get();
        let turn_id = Uuid::new_v4();
        let (levels, conversation_id) = {
            let mut inner = self.inner();
            inner.last_activity = Instant::now();
            (inner.levels.clone(), inner.conversation_id)
        };
        let response = {
            let mut mind = self.mind.lock().await;
            mind.respond(&TurnContext {
                turn_id,
                text: &text,
                levels: &levels,
                params: &cfg.cognition,
            })
        };
        let levels = self.regulate(&response.events);
        let question = response.question.clone().and_then(|q| q.admit().ok());
        let outcome = self
            .store
            .record_exchange(&Exchange {
                conversation_id,
                owner_turn_id: turn_id,
                mode: mode.as_str(),
                owner_text: &text,
                aster_text: &response.text,
                trace: &response.trace,
                question: question.as_ref(),
                state: &levels,
            })
            .await?;
        self.inner().persisted = levels.clone();
        self.emit(Event::Turn {
            turn: outcome.owner.clone(),
        });
        self.emit(Event::Turn {
            turn: outcome.aster.clone(),
        });
        self.emit(Event::Thought {
            turn_id: outcome.aster.id,
            trace: Box::new(response.trace.clone()),
        });
        self.emit(Event::Levels {
            state: levels.clone(),
            cause: Self::cause(&response.events),
        });
        if let Some(item) = &outcome.outbound {
            self.inner().outbound_times.push_back(Instant::now());
            self.emit(Event::Outbound {
                item: Box::new(item.clone()),
            });
        }
        self.after_updates(1).await;
        Ok(ChatResult {
            owner: outcome.owner,
            aster: outcome.aster,
            trace: response.trace,
            outbound: outcome.outbound,
            levels,
        })
    }

    pub async fn recent_turns(&self, limit: i64) -> Result<Vec<TurnRecord>, RuntimeError> {
        Ok(self.store.recent_turns(limit).await?)
    }

    pub async fn thought(&self, turn_id: Uuid) -> Result<serde_json::Value, RuntimeError> {
        self.store
            .thought(turn_id)
            .await?
            .ok_or(RuntimeError::NotFound)
    }

    // ----- learning -----------------------------------------------------

    async fn learn(
        &self,
        kind: &str,
        signal: LearningSignal,
    ) -> Result<(LearnOutcome, RegulatoryState), RuntimeError> {
        let params = self.config.get().cognition.clone();
        let outcome = {
            let mut mind = self.mind.lock().await;
            mind.learn(signal, &params)
        };
        self.inner().last_activity = Instant::now();
        self.emit(Event::Learned {
            kind: kind.into(),
            note: outcome.note.clone(),
        });
        let levels = if outcome.events.is_empty() {
            self.levels()
        } else {
            self.regulate(&outcome.events);
            self.persist_levels(&Self::cause(&outcome.events)).await?
        };
        if outcome.updated {
            self.after_updates(1).await;
        }
        Ok((outcome, levels))
    }

    pub async fn feedback(
        &self,
        turn_id: Uuid,
        rating: i8,
        correction: Option<&str>,
    ) -> Result<LearnResult, RuntimeError> {
        if rating == 0 {
            return Err(RuntimeError::Invalid(
                "rating must be good (+1) or bad (-1)".into(),
            ));
        }
        let correction = correction
            .map(|c| validate_text("correction", c))
            .transpose()?;
        if !self
            .store
            .record_feedback(turn_id, rating, correction.as_deref())
            .await?
        {
            return Err(RuntimeError::NotFound);
        }
        let (outcome, levels) = self
            .learn(
                "feedback",
                LearningSignal::Feedback {
                    turn_id,
                    rating: rating.signum(),
                    correction,
                },
            )
            .await?;
        Ok(LearnResult {
            note: outcome.note,
            levels,
        })
    }

    pub async fn teach(&self, prompt: &str, reply: &str) -> Result<LearnResult, RuntimeError> {
        let prompt = validate_text("prompt", prompt)?;
        let reply = validate_text("reply", reply)?;
        self.store
            .record_audit("taught", serde_json::json!({}))
            .await?;
        let (outcome, levels) = self
            .learn("teach", LearningSignal::Teach { prompt, reply })
            .await?;
        Ok(LearnResult {
            note: outcome.note,
            levels,
        })
    }

    /// Resolve `relative` inside the configured ingest directory only.
    pub fn resolve_ingest_path(base: &Path, relative: &str) -> Result<PathBuf, RuntimeError> {
        let base = base
            .canonicalize()
            .map_err(|_| RuntimeError::Invalid("ingest directory does not exist".into()))?;
        let candidate = base
            .join(relative)
            .canonicalize()
            .map_err(|_| RuntimeError::NotFound)?;
        if !candidate.starts_with(&base) || !candidate.is_file() {
            return Err(RuntimeError::Invalid(
                "path must name a file inside server.ingest_dir".into(),
            ));
        }
        Ok(candidate)
    }

    pub async fn ingest(&self, relative: &str) -> Result<LearnResult, RuntimeError> {
        let server = self.config.get().server.clone();
        let path = Self::resolve_ingest_path(Path::new(&server.ingest_dir), relative)?;
        let size = tokio::fs::metadata(&path)
            .await
            .map_err(|_| RuntimeError::NotFound)?
            .len();
        if size > server.ingest_max_bytes {
            return Err(RuntimeError::Invalid(format!(
                "file exceeds server.ingest_max_bytes ({})",
                server.ingest_max_bytes
            )));
        }
        let text = tokio::fs::read_to_string(&path)
            .await
            .map_err(|_| RuntimeError::Invalid("file must be UTF-8 text".into()))?;
        self.store
            .record_audit("ingested", serde_json::json!({ "bytes": size }))
            .await?;
        let (outcome, levels) = self
            .learn(
                "ingest",
                LearningSignal::Ingest {
                    name: relative.to_string(),
                    text,
                },
            )
            .await?;
        Ok(LearnResult {
            note: outcome.note,
            levels,
        })
    }

    /// Called after the owner creates a memory through the memory API.
    pub async fn memory_created(&self, memory: &MemoryRecord) {
        self.emit(Event::Memory {
            action: "created".into(),
            id: memory.id().get().to_string(),
            content: Some(memory.content().to_string()),
            provenance: Some(memory.provenance().to_string()),
        });
        let _ = self
            .learn(
                "memory",
                LearningSignal::Memory {
                    id: memory.id().get().to_string(),
                    content: memory.content().to_string(),
                },
            )
            .await;
    }

    pub async fn memory_deleted(&self, id: Uuid) {
        self.emit(Event::Memory {
            action: "deleted".into(),
            id: id.to_string(),
            content: None,
            provenance: None,
        });
        let _ = self
            .learn("forget", LearningSignal::Forget { id: id.to_string() })
            .await;
    }

    // ----- outbound -----------------------------------------------------

    pub async fn outbound(
        &self,
        status: Option<OutboundStatus>,
        limit: i64,
    ) -> Result<Vec<OutboundRecord>, RuntimeError> {
        Ok(self.store.list_outbound(status, limit).await?)
    }

    async fn pending(&self, id: Uuid) -> Result<OutboundRecord, RuntimeError> {
        let record = self
            .store
            .outbound(id)
            .await?
            .ok_or(RuntimeError::NotFound)?;
        if record.status != OutboundStatus::Pending {
            return Err(RuntimeError::Conflict(format!(
                "already {}",
                record.status.as_str()
            )));
        }
        Ok(record)
    }

    pub async fn answer(&self, id: Uuid, text: &str) -> Result<LearnResult, RuntimeError> {
        let text = validate_text("answer", text)?;
        let record = self.pending(id).await?;
        if record.needs_approval {
            return Err(RuntimeError::Conflict(
                "this is a request; approve or deny it".into(),
            ));
        }
        let resolved = self
            .store
            .resolve_outbound(id, OutboundStatus::Answered, Some(&text))
            .await?
            .ok_or_else(|| RuntimeError::Conflict("already resolved".into()))?;
        self.emit(Event::Outbound {
            item: Box::new(resolved.clone()),
        });
        let (outcome, levels) = self
            .learn(
                "answer",
                LearningSignal::Answer {
                    subject: resolved.draft.subject.clone(),
                    prompt: resolved.draft.prompt.clone(),
                    answer: text,
                },
            )
            .await?;
        Ok(LearnResult {
            note: outcome.note,
            levels,
        })
    }

    pub async fn decide(&self, id: Uuid, approve: bool) -> Result<OutboundRecord, RuntimeError> {
        let record = self.pending(id).await?;
        if !record.needs_approval {
            return Err(RuntimeError::Conflict(
                "this is a question; answer it instead".into(),
            ));
        }
        let status = if approve {
            OutboundStatus::Approved
        } else {
            OutboundStatus::Denied
        };
        let resolved = self
            .store
            .resolve_outbound(id, status, None)
            .await?
            .ok_or_else(|| RuntimeError::Conflict("already resolved".into()))?;
        self.emit(Event::Outbound {
            item: Box::new(resolved.clone()),
        });
        if approve && resolved.draft.effect == Some(RequestEffect::Rest) {
            let minutes = self.config.get().runtime.rest_minutes;
            self.inner().rest_until = Some(Instant::now() + Duration::from_secs(minutes * 60));
            self.regulate(&[RegulationEvent::Rest]);
            self.persist_levels("rest").await?;
            self.emit(Event::Runtime {
                message: format!("resting for {minutes} minutes"),
                paused: self.config.get().runtime.paused,
            });
        }
        Ok(resolved)
    }

    async fn propose(&self, draft: OutboundDraft) -> Result<Option<OutboundRecord>, RuntimeError> {
        let Ok(draft) = draft.admit() else {
            return Ok(None);
        };
        let pending = self
            .store
            .list_outbound(Some(OutboundStatus::Pending), 500)
            .await?;
        if pending.iter().any(|p| p.draft.body == draft.body) {
            return Ok(None);
        }
        let record = self.store.create_outbound(&draft).await?;
        self.inner().outbound_times.push_back(Instant::now());
        self.emit(Event::Outbound {
            item: Box::new(record.clone()),
        });
        Ok(Some(record))
    }

    // ----- runtime control ----------------------------------------------

    pub async fn set_paused(&self, paused: bool) -> Result<(), RuntimeError> {
        self.config
            .set(&[("runtime.paused".into(), serde_json::json!(paused))])
            .await?;
        {
            // Do not count paused time as elapsed decay.
            let mut inner = self.inner();
            inner.last_decay = Instant::now();
        }
        self.emit(Event::Runtime {
            message: if paused { "paused" } else { "resumed" }.into(),
            paused,
        });
        Ok(())
    }

    pub async fn status(&self) -> RuntimeStatus {
        let (engine, mind) = {
            let mind = self.mind.lock().await;
            (mind.engine().to_string(), mind.stats())
        };
        let inner = self.inner();
        RuntimeStatus {
            engine,
            paused: self.config.get().runtime.paused,
            resting: inner.rest_until.is_some_and(|t| t > Instant::now()),
            levels: inner.levels.clone(),
            mind,
            idle_secs: inner.last_activity.elapsed().as_secs(),
        }
    }

    pub async fn state_history(
        &self,
        limit: i64,
    ) -> Result<Vec<aster_storage::StateHistoryEntry>, RuntimeError> {
        Ok(self.store.state_history(limit).await?)
    }

    pub async fn corpus(&self) -> String {
        self.mind.lock().await.corpus()
    }

    pub async fn snapshot(&self, reason: &str) -> Result<SnapshotInfo, RuntimeError> {
        let (engine, bytes, stats) = {
            let mind = self.mind.lock().await;
            (
                mind.engine().to_string(),
                mind.snapshot().map_err(|_| RuntimeError::Snapshot)?,
                serde_json::to_value(mind.stats()).unwrap_or_default(),
            )
        };
        let info = self
            .store
            .save_mind_snapshot(&engine, reason, &bytes, stats)
            .await?;
        self.inner().updates_since_snapshot = 0;
        let keep = self.config.get().runtime.keep_snapshots;
        let _ = self.store.prune_mind_snapshots(i64::from(keep)).await;
        self.emit(Event::Runtime {
            message: format!("mind snapshot v{} saved ({reason})", info.version),
            paused: self.config.get().runtime.paused,
        });
        Ok(info)
    }

    pub async fn snapshots(&self, limit: i64) -> Result<Vec<SnapshotInfo>, RuntimeError> {
        Ok(self.store.list_mind_snapshots(limit).await?)
    }

    /// Replace the live mind with an earlier snapshot, then save it as latest.
    pub async fn rollback(&self, version: i64) -> Result<SnapshotInfo, RuntimeError> {
        let stored = self
            .store
            .mind_snapshot(Some(version))
            .await?
            .ok_or(RuntimeError::NotFound)?;
        let params = self.config.get().cognition.clone();
        let restored =
            aster_mind::build(&params, Some(&stored.model)).map_err(|_| RuntimeError::Snapshot)?;
        *self.mind.lock().await = restored;
        self.snapshot(&format!("rollback to v{version}")).await
    }

    // ----- ticking ------------------------------------------------------

    fn spawn_ticker(self: Arc<Self>) {
        let mut stop = self.stop.subscribe();
        tokio::spawn(async move {
            loop {
                let interval = Duration::from_millis(self.config.get().runtime.tick_interval_ms);
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = stop.changed() => return,
                }
                if let Err(error) = self.tick().await {
                    tracing::warn!(%error, "runtime tick failed");
                }
            }
        });
    }

    /// One regulation + initiative step.
    pub async fn tick(&self) -> Result<(), RuntimeError> {
        let cfg = self.config.get();
        if cfg.runtime.paused {
            self.inner().last_decay = Instant::now();
            return Ok(());
        }
        let (changed, idle, resting) = {
            let mut inner = self.inner();
            let elapsed = inner.last_decay.elapsed();
            inner.last_decay = Instant::now();
            inner.levels = cfg.regulation.decay(&inner.levels, elapsed);
            let resting = match inner.rest_until {
                Some(until) if until > Instant::now() => true,
                Some(_) => {
                    inner.rest_until = None;
                    false
                }
                None => false,
            };
            let hour_ago = Instant::now().checked_sub(Duration::from_secs(3600));
            while let (Some(front), Some(hour_ago)) = (inner.outbound_times.front(), hour_ago) {
                if *front < hour_ago {
                    inner.outbound_times.pop_front();
                } else {
                    break;
                }
            }
            (
                inner.levels.max_difference(&inner.persisted) >= cfg.runtime.persist_level_epsilon,
                inner.last_activity.elapsed(),
                resting,
            )
        };
        if changed {
            self.persist_levels(if resting { "rest" } else { "decay" })
                .await?;
        }
        if resting || idle < Duration::from_secs(cfg.runtime.idle_before_initiative_secs) {
            return Ok(());
        }
        if self.inner().outbound_times.len() >= cfg.runtime.max_outbound_per_hour as usize {
            return Ok(());
        }
        let pending = self
            .store
            .list_outbound(Some(OutboundStatus::Pending), 500)
            .await?
            .len();
        if pending >= cfg.runtime.max_pending_outbound as usize {
            return Ok(());
        }
        let levels = self.levels();
        let draft = {
            let mut mind = self.mind.lock().await;
            mind.initiative(&levels, &cfg.cognition)
        };
        // Wait another idle period before the next initiative either way.
        self.inner().last_activity = Instant::now();
        if let Some(draft) = draft {
            self.propose(draft).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_paths_cannot_escape_the_directory() {
        let base = std::env::temp_dir().join(format!("aster-ingest-{}", std::process::id()));
        std::fs::create_dir_all(base.join("inner")).unwrap();
        std::fs::write(base.join("inner/notes.txt"), "hello").unwrap();
        std::fs::write(base.parent().unwrap().join("outside.txt"), "secret").unwrap();
        assert!(Runtime::resolve_ingest_path(&base, "inner/notes.txt").is_ok());
        assert!(Runtime::resolve_ingest_path(&base, "../outside.txt").is_err());
        assert!(Runtime::resolve_ingest_path(&base, "/etc/hosts").is_err());
        assert!(Runtime::resolve_ingest_path(&base, "inner").is_err());
        assert!(Runtime::resolve_ingest_path(&base, "missing.txt").is_err());
    }

    #[test]
    fn text_validation_bounds_input() {
        assert!(validate_text("t", "  ").is_err());
        assert!(validate_text("t", "a\0b").is_err());
        assert!(validate_text("t", &"x".repeat(MAX_TEXT_BYTES + 1)).is_err());
        assert_eq!(validate_text("t", " hi ").unwrap(), "hi");
    }
}
