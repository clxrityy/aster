//! Aster's infrastructure-independent domain contracts.
//!
//! Identity, owner-provided memory, regulatory snapshots and their bounded
//! dynamics, authorization, outbound questions/requests, and the cognition
//! contract. No IO, providers, or scheduling live here.

pub mod cognition;
pub mod identity;
pub mod memory;
pub mod outbound;
pub mod permissions;
pub mod regulation;
pub mod trace;
pub mod world;

pub use cognition::{
    Cognition, CognitionParams, ENGINES, InvalidParams, LearnOutcome, LearningSignal, NeuralParams,
    Response, SnapshotError, TrainPoint, TrainReport, TurnContext,
};
pub use identity::{EvolvingIdentity, IdentitySnapshot, ProtectedIdentity};
pub use memory::{
    InvalidMemory, InvalidMemoryPage, MemoryCursor, MemoryDraft, MemoryId, MemoryPageSize,
    MemoryRecord,
};
pub use outbound::{OutboundDraft, OutboundKind, OutboundRejected, OutboundStatus, RequestEffect};
pub use permissions::{Action, Actor, PermissionDecision, PermissionPolicy};
pub use regulation::{InvalidRules, LevelRule, RegulationEvent, RegulationRules};
pub use trace::{Candidate, CandidateSource, Decision, Recalled, ThoughtTrace};
pub use world::{InvalidLevel, Level, LevelName, RegulatoryState};
