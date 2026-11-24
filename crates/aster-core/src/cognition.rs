//! The cognition contract. Engines are self-developed (no external models) and
//! interchangeable behind this trait. Owner parameters bound the engine; the
//! engine self-regulates its adaptive values inside those bounds.

use std::{collections::BTreeMap, time::Duration};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    outbound::OutboundDraft,
    regulation::RegulationEvent,
    trace::{CandidateSource, ThoughtTrace},
    world::RegulatoryState,
};

/// Owner-configurable cognition parameters (`[cognition]` in aster.toml).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CognitionParams {
    /// Engine name: "statistical", or "neural" (statistical plus a self-trained
    /// byte-level network; needs a build with the `neural` feature).
    pub engine: String,
    pub ngram_order: usize,
    pub max_reply_tokens: usize,
    pub recall_top_k: usize,
    pub learning_rate: f64,
    /// Bounds for the self-regulated exploration rate.
    pub exploration_min: f64,
    pub exploration_max: f64,
    /// Bounds for the self-regulated confidence threshold to reply.
    pub answer_confidence_min: f64,
    pub answer_confidence_max: f64,
    /// Save a model snapshot after this many learning updates.
    pub snapshot_every_updates: u32,
    /// Fixed seed for reproducible choices; random when absent.
    pub rng_seed: Option<u64>,
    /// Minimum curiosity level before Aster takes initiative to ask.
    pub initiative_curiosity_min: f64,
    /// Energy level below which Aster requests rest.
    pub rest_energy_below: f64,
    /// Load the built-in starter pack (everyday meanings, small-talk replies,
    /// synonyms) into new minds, and once into existing ones. Never overrides
    /// what the owner taught.
    pub starter_pack: bool,
    /// The neural engine (`[cognition.neural]`); ignored by "statistical".
    pub neural: NeuralParams,
}

/// Engines this contract knows; whether one is compiled in is the builder's call.
pub const ENGINES: [&str; 2] = ["statistical", "neural"];

/// Owner bounds for the self-trained neural engine. Structural fields
/// (`context`, `embed_dim`, `hidden`) only take effect for a new network:
/// changing them starts the network over.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NeuralParams {
    /// Bytes of history the network sees per prediction.
    pub context: usize,
    pub embed_dim: usize,
    pub hidden: usize,
    pub learning_rate: f64,
    pub batch: usize,
    /// Training minibatches per runtime tick (0 pauses training).
    pub train_steps_per_tick: u32,
    /// Hard time cap for one tick's training.
    pub max_train_ms_per_tick: u64,
    /// Train only once the corpus is at least this large.
    pub min_corpus_bytes: usize,
    /// Scales the neural candidate's score against the other candidates.
    pub weight: f64,
    pub temperature: f64,
    /// Sample among the `top_k` likeliest bytes.
    pub top_k: usize,
    pub max_reply_bytes: usize,
    /// The network stays quiet until it has trained this many steps...
    pub min_steps_to_speak: u64,
    /// ...and its recent mean loss (nats per byte) is at most this.
    pub max_loss_to_speak: f64,
    /// Share of a candidate's words Aster must already know (0 disables).
    pub min_known_words: f64,
    /// Continuations sampled per turn; the surest one is kept.
    pub samples: usize,
}

impl Default for NeuralParams {
    fn default() -> Self {
        Self {
            context: 32,
            embed_dim: 24,
            hidden: 256,
            learning_rate: 0.003,
            batch: 32,
            train_steps_per_tick: 8,
            max_train_ms_per_tick: 200,
            min_corpus_bytes: 512,
            weight: 0.6,
            temperature: 0.6,
            top_k: 5,
            max_reply_bytes: 160,
            min_steps_to_speak: 2000,
            max_loss_to_speak: 1.3,
            min_known_words: 0.8,
            samples: 3,
        }
    }
}

impl NeuralParams {
    fn validate(&self) -> Result<(), InvalidParams> {
        let fail = |message: &str| Err(InvalidParams(format!("neural.{message}")));
        if !(4..=128).contains(&self.context) {
            return fail("context must be within 4..=128");
        }
        if !(4..=256).contains(&self.embed_dim) || !(16..=2048).contains(&self.hidden) {
            return fail("embed_dim must be within 4..=256 and hidden within 16..=2048");
        }
        if !(self.learning_rate.is_finite()
            && self.learning_rate > 0.0
            && self.learning_rate <= 0.1)
        {
            return fail("learning_rate must be within (0, 0.1]");
        }
        if !(1..=512).contains(&self.batch) || self.train_steps_per_tick > 1000 {
            return fail("batch must be within 1..=512 and train_steps_per_tick at most 1000");
        }
        if !(1..=10_000).contains(&self.max_train_ms_per_tick) {
            return fail("max_train_ms_per_tick must be within 1..=10000");
        }
        if !(self.weight.is_finite() && (0.0..=1.0).contains(&self.weight)) {
            return fail("weight must be within 0..=1");
        }
        if !(self.temperature.is_finite() && self.temperature > 0.0 && self.temperature <= 3.0) {
            return fail("temperature must be within (0, 3]");
        }
        if !(1..=257).contains(&self.top_k) || !(1..=2000).contains(&self.max_reply_bytes) {
            return fail("top_k must be within 1..=257 and max_reply_bytes within 1..=2000");
        }
        if !(self.max_loss_to_speak.is_finite() && self.max_loss_to_speak > 0.0) {
            return fail("max_loss_to_speak must be positive");
        }
        if !(self.min_known_words.is_finite() && (0.0..=1.0).contains(&self.min_known_words)) {
            return fail("min_known_words must be within 0..=1");
        }
        if !(1..=16).contains(&self.samples) {
            return fail("samples must be within 1..=16");
        }
        Ok(())
    }
}

impl Default for CognitionParams {
    fn default() -> Self {
        Self {
            engine: "statistical".into(),
            ngram_order: 3,
            max_reply_tokens: 40,
            recall_top_k: 5,
            learning_rate: 0.25,
            exploration_min: 0.02,
            exploration_max: 0.4,
            answer_confidence_min: 0.15,
            answer_confidence_max: 0.6,
            snapshot_every_updates: 20,
            rng_seed: None,
            initiative_curiosity_min: 55.0,
            rest_energy_below: 25.0,
            starter_pack: true,
            neural: NeuralParams::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Error)]
#[error("invalid cognition parameters: {0}")]
pub struct InvalidParams(pub String);

impl CognitionParams {
    pub fn validate(&self) -> Result<(), InvalidParams> {
        let fail = |message: &str| Err(InvalidParams(message.into()));
        if !ENGINES.contains(&self.engine.as_str()) {
            return fail("engine must be \"statistical\" or \"neural\"");
        }
        self.neural.validate()?;
        if !(1..=6).contains(&self.ngram_order) {
            return fail("ngram_order must be within 1..=6");
        }
        if !(1..=400).contains(&self.max_reply_tokens) {
            return fail("max_reply_tokens must be within 1..=400");
        }
        if !(1..=50).contains(&self.recall_top_k) {
            return fail("recall_top_k must be within 1..=50");
        }
        let unit = |value: f64| value.is_finite() && (0.0..=1.0).contains(&value);
        if !unit(self.learning_rate) || self.learning_rate == 0.0 {
            return fail("learning_rate must be within (0, 1]");
        }
        if !unit(self.exploration_min)
            || !unit(self.exploration_max)
            || self.exploration_min > self.exploration_max
        {
            return fail("exploration bounds must satisfy 0 <= min <= max <= 1");
        }
        if !unit(self.answer_confidence_min)
            || !unit(self.answer_confidence_max)
            || self.answer_confidence_min > self.answer_confidence_max
        {
            return fail("answer_confidence bounds must satisfy 0 <= min <= max <= 1");
        }
        if self.snapshot_every_updates == 0 {
            return fail("snapshot_every_updates must be at least 1");
        }
        let level = |value: f64| value.is_finite() && (0.0..=100.0).contains(&value);
        if !level(self.initiative_curiosity_min) || !level(self.rest_energy_below) {
            return fail("initiative_curiosity_min and rest_energy_below must be within 0..=100");
        }
        Ok(())
    }
}

pub struct TurnContext<'a> {
    pub turn_id: Uuid,
    pub text: &'a str,
    pub levels: &'a RegulatoryState,
    pub params: &'a CognitionParams,
    /// Reconsidering an earlier reply: explore at the owner's upper bound,
    /// pick something other than this text, and don't relearn the prompt.
    pub avoid: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub text: String,
    pub trace: ThoughtTrace,
    /// Set when the reply is a question the owner should answer.
    pub question: Option<OutboundDraft>,
    pub events: Vec<RegulationEvent>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LearningSignal {
    Teach {
        prompt: String,
        reply: String,
    },
    Feedback {
        turn_id: Uuid,
        rating: i8,
        correction: Option<String>,
    },
    Answer {
        subject: Option<String>,
        prompt: Option<String>,
        answer: String,
    },
    Memory {
        id: String,
        content: String,
    },
    Forget {
        id: String,
    },
    Ingest {
        name: String,
        text: String,
    },
    /// The owner picked another of a turn's candidates as the best reply.
    /// `text`/`source` come from the stored trace, never from the client.
    Prefer {
        turn_id: Uuid,
        prompt: String,
        text: String,
        source: CandidateSource,
    },
    /// A rating on one candidate (chosen or not).
    RateCandidate {
        prompt: String,
        text: String,
        source: CandidateSource,
        rating: i8,
    },
    /// A recalled document did not belong with this prompt's terms.
    Irrelevant {
        prompt: String,
        document: String,
    },
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct LearnOutcome {
    pub events: Vec<RegulationEvent>,
    pub note: String,
    /// Whether model state changed (counts toward snapshots).
    pub updated: bool,
}

#[derive(Debug, Error)]
#[error("cognition snapshot could not be encoded or decoded")]
pub struct SnapshotError;

pub trait Cognition: Send {
    fn engine(&self) -> &'static str;
    fn respond(&mut self, context: &TurnContext<'_>) -> Response;
    fn learn(&mut self, signal: LearningSignal, params: &CognitionParams) -> LearnOutcome;
    /// Optionally start something without a prompt (bounded by the runtime).
    fn initiative(
        &mut self,
        levels: &RegulatoryState,
        params: &CognitionParams,
    ) -> Option<OutboundDraft>;
    fn snapshot(&self) -> Result<Vec<u8>, SnapshotError>;
    fn stats(&self) -> BTreeMap<String, f64>;
    /// Everything learned from, as plain text (training corpus export).
    fn corpus(&self) -> String;
    /// Background training within `budget` (called from the runtime tick, off
    /// the turn path). Engines without a trainable model return `None`.
    fn train(&mut self, _params: &CognitionParams, _budget: Duration) -> Option<TrainReport> {
        None
    }
    /// Recent training history, oldest first (empty for untrained engines).
    fn training_history(&self) -> Vec<TrainPoint> {
        Vec::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrainPoint {
    pub step: u64,
    pub loss: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TrainReport {
    pub steps: u32,
    pub total_steps: u64,
    pub loss: f32,
    pub corpus_bytes: usize,
    pub note: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_params_are_valid_and_bounds_checked() {
        CognitionParams::default().validate().unwrap();
        let params = CognitionParams {
            exploration_min: 0.5,
            exploration_max: 0.1,
            ..Default::default()
        };
        assert!(params.validate().is_err());
        let neural = CognitionParams {
            engine: "neural".into(),
            ..Default::default()
        };
        neural.validate().unwrap();
        let unknown = CognitionParams {
            engine: "oracle".into(),
            ..Default::default()
        };
        assert!(unknown.validate().is_err());
        let mut bad = CognitionParams::default();
        bad.neural.context = 1;
        assert!(bad.validate().unwrap_err().0.starts_with("neural.context"));
    }
}
