//! Private operational interface for Aster's Phase 1 foundation.

pub mod auth;
pub mod config;
pub mod routes;
pub mod server;

pub use routes::router;
