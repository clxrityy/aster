//! The persistent runtime: turn pipeline, learning operations, regulation
//! ticks, initiative, and mind snapshots. Owns no truth of its own beyond the
//! in-memory mind, which is snapshotted to PostgreSQL.

use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use aster_core::CandidateSource;
use aster_core::{
    Cognition, LearnOutcome, LearningSignal, MemoryCursor, MemoryPageSize, MemoryRecord,
    OutboundDraft, OutboundStatus, RegulationEvent, RegulatoryState, RequestEffect, ThoughtTrace,
    TurnContext,
};
use aster_storage::{
    Exchange, OutboundRecord, Reconsideration, SnapshotInfo, StorageError, Store, StoredExchange,
    TurnRecord, TurnSignal,
};
use chrono::Utc;
use serde::Serialize;
use thiserror::Error;
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

use crate::{
    config::ConfigHandle,
    events::{Envelope, Event},
    logs, web,
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
pub struct ReadResult {
    pub url: String,
    pub title: String,
    /// Where the text was saved, inside `server.ingest_dir`.
    pub saved_as: String,
    pub bytes: usize,
    pub links: usize,
    pub truncated: bool,
    pub note: String,
    pub levels: RegulatoryState,
}

/// File types `upload` accepts. HTML is read down to its text first.
pub const UPLOAD_EXTENSIONS: [&str; 5] = ["txt", "md", "markdown", "aster", "html"];

#[derive(Debug, Clone, Serialize)]
pub struct IngestFile {
    pub path: String,
    pub bytes: u64,
    pub too_large: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrainingStatus {
    pub engine: String,
    /// Engines compiled into this build.
    pub available: Vec<String>,
    pub history: Vec<aster_core::TrainPoint>,
    pub stats: BTreeMap<String, f64>,
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
    mind: Arc<tokio::sync::Mutex<Box<dyn Cognition>>>,
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
            mind: Arc::new(tokio::sync::Mutex::new(mind)),
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
        self.reply(text, mode, None).await
    }

    /// A chat turn, optionally threaded as a reply to an earlier turn.
    pub async fn reply(
        &self,
        text: &str,
        mode: Mode,
        reply_to: Option<Uuid>,
    ) -> Result<ChatResult, RuntimeError> {
        let text = validate_text("text", text)?;
        if let Some(id) = reply_to
            && self.store.turn(id).await?.is_none()
        {
            return Err(RuntimeError::Invalid("reply_to is not a known turn".into()));
        }
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
                avoid: None,
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
                owner_reply_to: reply_to,
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

    // ----- steering -----------------------------------------------------

    async fn stored(
        &self,
        aster_turn: Uuid,
    ) -> Result<(StoredExchange, ThoughtTrace), RuntimeError> {
        let exchange = self
            .store
            .exchange(aster_turn)
            .await?
            .ok_or(RuntimeError::NotFound)?;
        let trace: ThoughtTrace = serde_json::from_value(exchange.trace.clone())
            .map_err(|_| RuntimeError::Conflict("this turn's trace cannot be read".into()))?;
        Ok((exchange, trace))
    }

    fn candidate(
        trace: &ThoughtTrace,
        index: usize,
    ) -> Result<aster_core::Candidate, RuntimeError> {
        let candidate = trace.candidates.get(index).cloned().ok_or_else(|| {
            RuntimeError::Invalid(format!(
                "candidate must be within 0..{}",
                trace.candidates.len()
            ))
        })?;
        if candidate.source == CandidateSource::Question {
            return Err(RuntimeError::Invalid(
                "a question is not a reply; answer it instead".into(),
            ));
        }
        Ok(candidate)
    }

    /// The owner picked candidate `index` of an Aster turn as the best reply.
    pub async fn prefer(
        &self,
        aster_turn: Uuid,
        index: usize,
    ) -> Result<LearnResult, RuntimeError> {
        let (exchange, trace) = self.stored(aster_turn).await?;
        let candidate = Self::candidate(&trace, index)?;
        self.store
            .record_signal(&TurnSignal {
                turn_id: aster_turn,
                kind: "prefer",
                candidate_index: Some(index as u32),
                rating: None,
                detail: Some(&candidate.text),
            })
            .await?;
        let (outcome, levels) = self
            .learn(
                "prefer",
                LearningSignal::Prefer {
                    turn_id: exchange.owner.id,
                    prompt: exchange.owner.text,
                    text: candidate.text,
                    source: candidate.source,
                },
            )
            .await?;
        Ok(LearnResult {
            note: outcome.note,
            levels,
        })
    }

    /// Rate one candidate of an Aster turn, chosen or not.
    pub async fn rate_candidate(
        &self,
        aster_turn: Uuid,
        index: usize,
        rating: i8,
    ) -> Result<LearnResult, RuntimeError> {
        if rating == 0 {
            return Err(RuntimeError::Invalid(
                "rating must be good (+1) or bad (-1)".into(),
            ));
        }
        let (exchange, trace) = self.stored(aster_turn).await?;
        let candidate = Self::candidate(&trace, index)?;
        self.store
            .record_signal(&TurnSignal {
                turn_id: aster_turn,
                kind: "rate",
                candidate_index: Some(index as u32),
                rating: Some(rating.signum()),
                detail: Some(&candidate.text),
            })
            .await?;
        let (outcome, levels) = self
            .learn(
                "rate_candidate",
                LearningSignal::RateCandidate {
                    prompt: exchange.owner.text,
                    text: candidate.text,
                    source: candidate.source,
                    rating: rating.signum(),
                },
            )
            .await?;
        Ok(LearnResult {
            note: outcome.note,
            levels,
        })
    }

    /// A recalled document did not belong with this turn's prompt.
    pub async fn mark_irrelevant(
        &self,
        aster_turn: Uuid,
        index: usize,
    ) -> Result<LearnResult, RuntimeError> {
        let (exchange, trace) = self.stored(aster_turn).await?;
        let recalled = trace.recalled.get(index).cloned().ok_or_else(|| {
            RuntimeError::Invalid(format!(
                "recalled item must be within 0..{}",
                trace.recalled.len()
            ))
        })?;
        self.store
            .record_signal(&TurnSignal {
                turn_id: aster_turn,
                kind: "irrelevant",
                candidate_index: Some(index as u32),
                rating: None,
                detail: Some(&recalled.source),
            })
            .await?;
        let (outcome, levels) = self
            .learn(
                "irrelevant",
                LearningSignal::Irrelevant {
                    prompt: exchange.owner.text,
                    document: recalled.source,
                },
            )
            .await?;
        Ok(LearnResult {
            note: outcome.note,
            levels,
        })
    }

    /// Ask Aster for a different reply to the same owner turn. The new reply is
    /// a new Aster turn; nothing is learned until the owner rates or prefers.
    pub async fn reconsider(&self, aster_turn: Uuid) -> Result<ChatResult, RuntimeError> {
        let (exchange, _) = self.stored(aster_turn).await?;
        let cfg = self.config.get();
        let levels = self.levels();
        let response = {
            let mut mind = self.mind.lock().await;
            mind.respond(&TurnContext {
                turn_id: exchange.owner.id,
                text: &exchange.owner.text,
                levels: &levels,
                params: &cfg.cognition,
                avoid: Some(&exchange.aster.text),
            })
        };
        let aster = self
            .store
            .record_reconsideration(&Reconsideration {
                owner_turn_id: exchange.owner.id,
                previous_aster_turn: aster_turn,
                aster_text: &response.text,
                trace: &response.trace,
                state: &levels,
            })
            .await?;
        self.inner().last_activity = Instant::now();
        self.emit(Event::Turn {
            turn: aster.clone(),
        });
        self.emit(Event::Thought {
            turn_id: aster.id,
            trace: Box::new(response.trace.clone()),
        });
        self.after_updates(1).await;
        Ok(ChatResult {
            owner: exchange.owner,
            aster,
            trace: response.trace,
            outbound: None,
            levels,
        })
    }

    pub async fn turn_signals(
        &self,
        aster_turn: Uuid,
    ) -> Result<Vec<serde_json::Value>, RuntimeError> {
        Ok(self.store.turn_signals(aster_turn).await?)
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
        // Cognition knows an exchange by its owner turn; the owner rates the Aster turn.
        let exchange = self
            .store
            .record_feedback(turn_id, rating, correction.as_deref())
            .await?
            .ok_or(RuntimeError::NotFound)?;
        let (outcome, levels) = self
            .learn(
                "feedback",
                LearningSignal::Feedback {
                    turn_id: exchange,
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

    /// A safe file name for an upload: one plain path component with an
    /// accepted extension. HTML is stored as the text read from it.
    pub fn upload_name(name: &str) -> Result<String, RuntimeError> {
        let name = name.trim();
        let bad = |why: &str| Err(RuntimeError::Invalid(format!("file name {why}")));
        if name.is_empty() || name.len() > 120 {
            return bad("must be 1 to 120 characters");
        }
        if name.starts_with('.') || name.contains(['/', '\\', '\0']) || name.contains("..") {
            return bad("must be a plain name (no folders, no leading dot)");
        }
        let Some((stem, extension)) = name.rsplit_once('.') else {
            return bad("needs an extension (.txt, .md, .aster or .html)");
        };
        let extension = extension.to_ascii_lowercase();
        if stem.is_empty() || !UPLOAD_EXTENSIONS.contains(&extension.as_str()) {
            return bad("must end in .txt, .md, .markdown, .aster or .html");
        }
        if !name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        {
            return bad("may use letters, digits, spaces, '-', '_' and '.'");
        }
        Ok(if extension == "html" {
            format!("{stem}.txt")
        } else {
            name.to_string()
        })
    }

    /// Save owner-sent text (a file upload or a paste) under
    /// `ingest_dir/uploads/` and learn from it like `/ingest`.
    pub async fn upload(&self, name: &str, text: &str) -> Result<LearnResult, RuntimeError> {
        let server = self.config.get().server.clone();
        if text.len() as u64 > server.ingest_max_bytes {
            return Err(RuntimeError::Invalid(format!(
                "text exceeds server.ingest_max_bytes ({})",
                server.ingest_max_bytes
            )));
        }
        if text.trim().is_empty() || text.contains('\0') {
            return Err(RuntimeError::Invalid(
                "text must be nonblank and NUL-free".into(),
            ));
        }
        let stored = Self::upload_name(name)?;
        let text = if name.to_ascii_lowercase().ends_with(".html") {
            let page = web::read_html(text, None);
            format!("{}\n\n{}", page.title, page.text)
        } else {
            text.to_string()
        };
        let dir = PathBuf::from(&server.ingest_dir).join("uploads");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|_| RuntimeError::Invalid("could not create the uploads folder".into()))?;
        tokio::fs::write(dir.join(&stored), text.as_bytes())
            .await
            .map_err(|_| RuntimeError::Invalid("could not save the upload".into()))?;
        self.ingest(&format!("uploads/{stored}")).await
    }

    /// Fetch a page the owner sent or approved, save its text under
    /// `ingest_dir/web/`, and learn from it (text, and links to ask about).
    pub async fn read_url(&self, url: &str, origin: &str) -> Result<ReadResult, RuntimeError> {
        let cfg = self.config.get();
        let page = web::fetch(url, &cfg.web)
            .await
            .map_err(RuntimeError::Invalid)?;
        if page.text.trim().is_empty() {
            return Err(RuntimeError::Invalid(format!(
                "found no readable text at {}",
                page.url
            )));
        }
        let saved_as = format!("web/{}", web_file_name(&page.url));
        let dir = PathBuf::from(&cfg.server.ingest_dir).join("web");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|_| RuntimeError::Invalid("could not create the web folder".into()))?;
        let file = format!("# {}\n{}\n\n{}\n", page.title, page.url, page.text);
        tokio::fs::write(
            PathBuf::from(&cfg.server.ingest_dir).join(&saved_as),
            file.as_bytes(),
        )
        .await
        .map_err(|_| RuntimeError::Invalid("could not save the page".into()))?;
        let host = url::Url::parse(&page.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .unwrap_or_default();
        self.store
            .record_audit(
                "read_url",
                serde_json::json!({
                    "host": host,
                    "bytes": page.text.len(),
                    "origin": origin,
                }),
            )
            .await?;
        let links = page.links.len();
        let (outcome, levels) = self
            .learn(
                "read",
                LearningSignal::Read {
                    url: page.url.clone(),
                    title: page.title.clone(),
                    text: page.text.clone(),
                    links: page.links,
                },
            )
            .await?;
        Ok(ReadResult {
            url: page.url,
            title: page.title,
            saved_as,
            bytes: page.text.len(),
            links,
            truncated: page.truncated,
            note: outcome.note,
            levels,
        })
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

    /// Files the owner could ingest: regular files inside `server.ingest_dir`
    /// (two levels deep), with sizes, sorted by path.
    pub async fn ingest_files(&self) -> Result<Vec<IngestFile>, RuntimeError> {
        let server = self.config.get().server.clone();
        let root = PathBuf::from(&server.ingest_dir);
        let max = server.ingest_max_bytes;
        tokio::task::spawn_blocking(move || {
            let mut files = Vec::new();
            let Ok(root) = root.canonicalize() else {
                return files;
            };
            let mut stack = vec![(root.clone(), 0)];
            while let Some((dir, depth)) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    // symlink_metadata: never follow links out of the folder.
                    let Ok(meta) = entry.path().symlink_metadata() else {
                        continue;
                    };
                    let path = entry.path();
                    if meta.is_dir() && depth < 1 {
                        stack.push((path, depth + 1));
                    } else if meta.is_file()
                        && let Ok(relative) = path.strip_prefix(&root)
                        && let Some(relative) = relative.to_str()
                        && !relative.split('/').any(|p| p.starts_with('.'))
                    {
                        files.push(IngestFile {
                            path: relative.to_string(),
                            bytes: meta.len(),
                            too_large: meta.len() > max,
                        });
                    }
                }
            }
            files.sort_by(|a, b| a.path.cmp(&b.path));
            files.truncate(500);
            files
        })
        .await
        .map_err(|_| RuntimeError::Invalid("could not list the ingest folder".into()))
    }

    /// The newest `limit` entries of a log category (today's file, else the
    /// latest day that has one), oldest first.
    pub async fn log_tail(
        &self,
        category: &str,
        limit: usize,
    ) -> Result<Vec<serde_json::Value>, RuntimeError> {
        if !crate::config::LOG_CATEGORIES.contains(&category) {
            return Err(RuntimeError::Invalid(format!(
                "category must be one of {}",
                crate::config::LOG_CATEGORIES.join(", ")
            )));
        }
        let dir = PathBuf::from(&self.config.get().logs.dir).join(category);
        let limit = limit.clamp(1, 1000);
        tokio::task::spawn_blocking(move || {
            let newest = std::fs::read_dir(&dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .max();
            let Some(file) = newest else {
                return Vec::new();
            };
            let text = std::fs::read_to_string(file).unwrap_or_default();
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(limit)..]
                .iter()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        })
        .await
        .map_err(|_| RuntimeError::Invalid("could not read the log".into()))
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
        // A link as the answer: read it (before resolving, so a page that
        // can't be read leaves the question open).
        let read = if web::parse_url(&text).is_ok() && !text.contains(char::is_whitespace) {
            Some(self.read_url(&text, "answer").await?)
        } else {
            None
        };
        let resolved = self
            .store
            .resolve_outbound(id, OutboundStatus::Answered, Some(&text))
            .await?
            .ok_or_else(|| RuntimeError::Conflict("already resolved".into()))?;
        self.emit(Event::Outbound {
            item: Box::new(resolved.clone()),
        });
        if let Some(read) = read {
            return Ok(LearnResult {
                note: read.note,
                levels: read.levels,
            });
        }
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
        if approve
            && resolved.draft.effect == Some(RequestEffect::Fetch)
            && let Some(url) = resolved.draft.source.clone()
        {
            // The fetcher checks the URL and every redirect again here.
            let message = match self.read_url(&url, "approved").await {
                Ok(read) => read.note,
                Err(error) => format!("could not read {url}: {error}"),
            };
            self.emit(Event::Runtime {
                message,
                paused: self.config.get().runtime.paused,
            });
        }
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

    /// The engine's recent training curve (empty for untrained engines).
    pub async fn training(&self) -> TrainingStatus {
        let mind = self.mind.lock().await;
        TrainingStatus {
            engine: mind.engine().to_string(),
            available: aster_mind::available_engines()
                .iter()
                .map(|e| e.to_string())
                .collect(),
            history: mind.training_history(),
            stats: mind.stats(),
        }
    }

    /// Rebuild the mind when `cognition.engine` changed: snapshot what Aster
    /// knows, then restore it into the configured engine.
    async fn follow_engine(&self) -> Result<(), RuntimeError> {
        let params = self.config.get().cognition.clone();
        let from = {
            let mut mind = self.mind.lock().await;
            if mind.engine() == params.engine {
                return Ok(());
            }
            let bytes = mind.snapshot().map_err(|_| RuntimeError::Snapshot)?;
            let rebuilt =
                aster_mind::build(&params, Some(&bytes)).map_err(|_| RuntimeError::Snapshot)?;
            std::mem::replace(&mut *mind, rebuilt).engine()
        };
        self.emit(Event::Runtime {
            message: format!("cognition engine switched from {from} to {}", params.engine),
            paused: self.config.get().runtime.paused,
        });
        self.snapshot(&format!("engine switched to {}", params.engine))
            .await?;
        Ok(())
    }

    /// One bounded background training slice on a blocking thread.
    async fn train(&self) {
        let params = self.config.get().cognition.clone();
        let budget = Duration::from_millis(params.neural.max_train_ms_per_tick);
        let mut mind = self.mind.clone().lock_owned().await;
        let report = tokio::task::spawn_blocking(move || mind.train(&params, budget)).await;
        match report {
            Ok(Some(report)) if report.steps > 0 => self.emit(Event::Trained {
                steps: report.steps,
                total_steps: report.total_steps,
                loss: report.loss,
                corpus_bytes: report.corpus_bytes,
            }),
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "training task failed"),
        }
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
        self.follow_engine().await?;
        if !resting {
            self.train().await;
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
        // Don't ask to read pages that couldn't be read anyway.
        let draft = draft.filter(|d| cfg.web.enabled || d.effect != Some(RequestEffect::Fetch));
        if let Some(draft) = draft {
            self.propose(draft).await?;
        }
        Ok(())
    }
}

/// `<host>-<path slug>-<hash>.txt`: readable, stable per URL, and safe.
fn web_file_name(url: &str) -> String {
    use sha2::{Digest, Sha256};
    let parsed = url::Url::parse(url).ok();
    let host = parsed
        .as_ref()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let path = parsed
        .as_ref()
        .map(|u| u.path().to_string())
        .unwrap_or_default();
    let slug = |text: &str| -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .split('-')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    };
    let mut path = slug(&path);
    path.truncate(60);
    let hash = Sha256::digest(url.as_bytes());
    let hash: String = hash[..4].iter().map(|b| format!("{b:02x}")).collect();
    let name = [slug(&host), path, hash]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    format!("{name}.txt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_names_are_plain_and_typed() {
        assert_eq!(Runtime::upload_name("notes.md").unwrap(), "notes.md");
        assert_eq!(
            Runtime::upload_name("Page One.HTML").unwrap(),
            "Page One.txt"
        );
        for bad in [
            "../x.txt",
            ".env",
            "a/b.txt",
            "a\\b.txt",
            "x",
            "run.sh",
            ".txt",
            "a..txt",
            "",
            "we;ird.txt",
        ] {
            assert!(Runtime::upload_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn web_files_are_named_after_the_url() {
        let name = web_file_name("https://en.wikipedia.org/wiki/Nebula");
        assert!(name.starts_with("en-wikipedia-org-wiki-nebula-"), "{name}");
        assert!(name.ends_with(".txt"));
        assert_ne!(
            name,
            web_file_name("https://en.wikipedia.org/wiki/Nebula?x=1")
        );
        assert!(!web_file_name("https://x.org/../../etc").contains('/'));
    }

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
