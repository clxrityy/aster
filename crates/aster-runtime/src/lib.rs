//! Aster's persistent runtime: live configuration, event bus, JSONL log
//! mirrors, the conversation/learning pipeline, and regulation ticks.

pub mod config;
pub mod engine;
pub mod events;
pub mod logs;

pub use config::{AsterConfig, ConfigError, ConfigHandle, DashboardSection, LOG_CATEGORIES};
pub use engine::{
    ChatResult, IngestFile, LearnResult, Mode, Runtime, RuntimeError, RuntimeStatus, TrainingStatus,
};
pub use events::{Envelope, Event};
