//! Aster's infrastructure-independent Phase 1 domain contracts.
//!
//! These types describe identity, initial regulatory state, and authorization
//! decisions only. They implement no state dynamics or external capabilities.

pub mod identity;
pub mod permissions;
pub mod world;

pub use identity::{EvolvingIdentity, IdentitySnapshot, ProtectedIdentity};
pub use permissions::{Action, Actor, PermissionDecision, PermissionPolicy};
pub use world::{InvalidLevel, Level, RegulatoryState};
