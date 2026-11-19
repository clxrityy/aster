//! Live events broadcast to the console (SSE) and mirrored to JSONL logs.

use aster_core::{RegulatoryState, ThoughtTrace};
use aster_storage::{OutboundRecord, TurnRecord};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Turn {
        turn: TurnRecord,
    },
    Thought {
        turn_id: Uuid,
        trace: Box<ThoughtTrace>,
    },
    Levels {
        state: RegulatoryState,
        cause: String,
    },
    Outbound {
        item: Box<OutboundRecord>,
    },
    Memory {
        action: String,
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provenance: Option<String>,
    },
    Learned {
        kind: String,
        note: String,
    },
    ConfigChanged {
        keys: Vec<String>,
        restart_required: Vec<String>,
    },
    ConfigRejected {
        error: String,
    },
    Runtime {
        message: String,
        paused: bool,
    },
    /// A background training slice finished (neural engine).
    Trained {
        steps: u32,
        total_steps: u64,
        loss: f32,
        corpus_bytes: usize,
    },
}

impl Event {
    /// Log directory this event is mirrored into.
    pub fn category(&self) -> &'static str {
        match self {
            Event::Turn { .. } => "conversations",
            Event::Thought { .. } | Event::Learned { .. } => "thoughts",
            Event::Levels { .. } => "levels",
            Event::Outbound { .. } => "requests",
            Event::Memory { .. } => "memories",
            Event::ConfigChanged { .. } | Event::ConfigRejected { .. } => "config",
            Event::Runtime { .. } => "runtime",
            Event::Trained { .. } => "training",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub event: Event,
}

impl Envelope {
    pub fn now(event: Event) -> Self {
        Self {
            at: Utc::now(),
            event,
        }
    }
}
