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
#[cfg(feature = "neural")]
pub mod neural_mind;

pub use mind::StatisticalMind;

use aster_core::{Cognition, CognitionParams, SnapshotError};

/// Engines compiled into this build.
pub fn available_engines() -> &'static [&'static str] {
    if cfg!(feature = "neural") {
        &["statistical", "neural"]
    } else {
        &["statistical"]
    }
}

/// Neural snapshots are an envelope around a statistical one.
const NEURAL_MAGIC: &[u8] = b"ASTE1";

/// Build the configured engine, restoring a snapshot when one is supplied.
/// Snapshots move between engines: a statistical snapshot restored as
/// "neural" gets a new network, and a neural snapshot restored as
/// "statistical" keeps everything but the network.
pub fn build(
    params: &CognitionParams,
    snapshot: Option<&[u8]>,
) -> Result<Box<dyn Cognition>, SnapshotError> {
    if !available_engines().contains(&params.engine.as_str()) {
        return Err(SnapshotError);
    }
    let is_neural = snapshot.is_some_and(|bytes| bytes.starts_with(NEURAL_MAGIC));
    #[cfg(feature = "neural")]
    if params.engine == neural_mind::ENGINE {
        let mut mind = match snapshot {
            Some(bytes) if is_neural => neural_mind::NeuralMind::restore(bytes, params)?,
            Some(bytes) => {
                neural_mind::NeuralMind::wrap(StatisticalMind::restore(bytes, params)?, params)?
            }
            None => neural_mind::NeuralMind::wrap(StatisticalMind::new(params), params)?,
        };
        starter(mind.statistical_mut(), params);
        return Ok(Box::new(mind));
    }
    let mut mind = match snapshot {
        #[cfg(feature = "neural")]
        Some(bytes) if is_neural => {
            StatisticalMind::restore(neural_mind::NeuralMind::statistical_bytes(bytes)?, params)?
        }
        #[cfg(not(feature = "neural"))]
        Some(_) if is_neural => return Err(SnapshotError),
        Some(bytes) => StatisticalMind::restore(bytes, params)?,
        None => StatisticalMind::new(params),
    };
    starter(&mut mind, params);
    Ok(Box::new(mind))
}

fn starter(mind: &mut StatisticalMind, params: &CognitionParams) {
    if params.starter_pack && !mind.has_pack(pack::STARTER_ID) {
        mind.load_pack(pack::STARTER_ID, pack::STARTER, params);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_or_missing_engines_are_refused() {
        let oracle = CognitionParams {
            engine: "oracle".into(),
            ..Default::default()
        };
        assert!(build(&oracle, None).is_err());
        let neural = CognitionParams {
            engine: "neural".into(),
            ..Default::default()
        };
        assert_eq!(build(&neural, None).is_ok(), cfg!(feature = "neural"));
        assert_eq!(
            available_engines().contains(&"neural"),
            cfg!(feature = "neural")
        );
    }
}
