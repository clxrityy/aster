//! The cognition contract. Engines are self-developed (no external models) and
//! interchangeable behind this trait. Owner parameters bound the engine; the
//! engine self-regulates its adaptive values inside those bounds.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    outbound::OutboundDraft, regulation::RegulationEvent, trace::ThoughtTrace,
    world::RegulatoryState,
};

/// Owner-configurable cognition parameters (`[cognition]` in aster.toml).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct CognitionParams {
    /// Engine name; only "statistical" is available in this version.
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Error)]
#[error("invalid cognition parameters: {0}")]
pub struct InvalidParams(pub String);

impl CognitionParams {
    pub fn validate(&self) -> Result<(), InvalidParams> {
        let fail = |message: &str| Err(InvalidParams(message.into()));
        if self.engine != "statistical" {
            return fail("engine must be \"statistical\" (neural is not selectable yet)");
        }
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
        let params = CognitionParams {
            engine: "neural".into(),
            ..Default::default()
        };
        assert!(params.validate().is_err());
    }
}
