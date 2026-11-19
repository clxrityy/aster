//! Aster's self-developed cognition. No external models: everything here is
//! learned from the owner's conversations, teaching, memories, and ingested text.

pub mod concepts;
pub mod corpus;
pub mod mind;
pub mod ngram;
pub mod pack;
pub mod recall;
pub mod rng;
pub mod text;
pub mod understand;

#[cfg(feature = "neural")]
pub mod neural;

pub use mind::StatisticalMind;

use aster_core::{Cognition, CognitionParams, SnapshotError};

/// Build the configured engine, restoring a snapshot when one is supplied.
pub fn build(
    params: &CognitionParams,
    snapshot: Option<&[u8]>,
) -> Result<Box<dyn Cognition>, SnapshotError> {
    let mut mind = match snapshot {
        Some(bytes) => StatisticalMind::restore(bytes, params)?,
        None => StatisticalMind::new(params),
    };
    if params.starter_pack && !mind.has_pack(pack::STARTER_ID) {
        mind.load_pack(pack::STARTER_ID, pack::STARTER, params);
    }
    Ok(Box::new(mind))
}
