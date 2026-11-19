//! Aster's infrastructure-independent domain contracts.
//!
//! Identity, owner-provided memory, regulatory snapshots, and authorization.
//! No state dynamics, autonomous runtime, or external capabilities run here.

pub mod identity;
pub mod memory;
pub mod permissions;
pub mod world;

pub use identity::{EvolvingIdentity, IdentitySnapshot, ProtectedIdentity};
pub use memory::{
    InvalidMemory, InvalidMemoryPage, MemoryCursor, MemoryDraft, MemoryId, MemoryPageSize,
    MemoryRecord,
};
pub use permissions::{Action, Actor, PermissionDecision, PermissionPolicy};
pub use world::{InvalidLevel, Level, RegulatoryState};
