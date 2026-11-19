//! Bounded regulatory dynamics. The owner configures baselines, decay rates,
//! event deltas, and self-regulation bounds; Aster's levels then move only
//! through these pure functions. Levels are internal regulation, not feelings.

use std::{collections::BTreeMap, time::Duration};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::world::{Level, LevelName, RegulatoryState};

/// Something that happened and may shift levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegulationEvent {
    MessageReceived,
    ReplySent,
    PositiveFeedback,
    NegativeFeedback,
    UnknownConcept,
    Taught,
    QuestionAnswered,
    Ingested,
    Rest,
}

impl RegulationEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            RegulationEvent::MessageReceived => "message_received",
            RegulationEvent::ReplySent => "reply_sent",
            RegulationEvent::PositiveFeedback => "positive_feedback",
            RegulationEvent::NegativeFeedback => "negative_feedback",
            RegulationEvent::UnknownConcept => "unknown_concept",
            RegulationEvent::Taught => "taught",
            RegulationEvent::QuestionAnswered => "question_answered",
            RegulationEvent::Ingested => "ingested",
            RegulationEvent::Rest => "rest",
        }
    }
}

/// Owner-configured rule for one level.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LevelRule {
    /// Resting value the level drifts back toward.
    pub baseline: f64,
    /// Fraction of the distance to baseline recovered per minute (0..=1).
    pub decay_per_min: f64,
    /// Lowest value self-regulation may reach.
    pub min: f64,
    /// Highest value self-regulation may reach.
    pub max: f64,
}

impl LevelRule {
    const fn new(baseline: f64, decay_per_min: f64) -> Self {
        Self {
            baseline,
            decay_per_min,
            min: 0.0,
            max: 100.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Error)]
#[error("invalid regulation rules: {0}")]
pub struct InvalidRules(pub String);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RegulationRules {
    pub levels: BTreeMap<LevelName, LevelRule>,
    /// Per-event level deltas, applied then clamped to each level's bounds.
    pub events: BTreeMap<RegulationEvent, BTreeMap<LevelName, f64>>,
}

impl Default for RegulationRules {
    fn default() -> Self {
        use LevelName::*;
        use RegulationEvent::*;
        let levels = BTreeMap::from([
            (Energy, LevelRule::new(75.0, 0.02)),
            (Focus, LevelRule::new(70.0, 0.05)),
            (CognitiveLoad, LevelRule::new(10.0, 0.10)),
            (Curiosity, LevelRule::new(60.0, 0.03)),
            (Stimulation, LevelRule::new(40.0, 0.05)),
            (Stress, LevelRule::new(10.0, 0.08)),
            (Confidence, LevelRule::new(50.0, 0.01)),
        ]);
        let events = BTreeMap::from([
            (
                MessageReceived,
                BTreeMap::from([(CognitiveLoad, 4.0), (Stimulation, 5.0), (Energy, -0.5)]),
            ),
            (ReplySent, BTreeMap::from([(Energy, -0.5), (Focus, 1.0)])),
            (
                PositiveFeedback,
                BTreeMap::from([(Confidence, 3.0), (Stress, -2.0)]),
            ),
            (
                NegativeFeedback,
                BTreeMap::from([(Confidence, -3.0), (Stress, 3.0), (Curiosity, 1.0)]),
            ),
            (
                UnknownConcept,
                BTreeMap::from([(Curiosity, 4.0), (CognitiveLoad, 2.0)]),
            ),
            (
                Taught,
                BTreeMap::from([(Confidence, 1.5), (CognitiveLoad, 2.0)]),
            ),
            (
                QuestionAnswered,
                BTreeMap::from([(Curiosity, -3.0), (Confidence, 2.0)]),
            ),
            (
                Ingested,
                BTreeMap::from([(CognitiveLoad, 8.0), (Energy, -3.0), (Curiosity, 2.0)]),
            ),
            (
                Rest,
                BTreeMap::from([(Energy, 10.0), (CognitiveLoad, -10.0), (Stress, -5.0)]),
            ),
        ]);
        Self { levels, events }
    }
}

impl RegulationRules {
    pub fn validate(&self) -> Result<(), InvalidRules> {
        for name in LevelName::ALL {
            let rule = self.rule(name);
            let in_range = |value: f64| value.is_finite() && (0.0..=100.0).contains(&value);
            if !in_range(rule.min) || !in_range(rule.max) || rule.min > rule.max {
                return Err(InvalidRules(format!(
                    "{}: min/max must satisfy 0 <= min <= max <= 100",
                    name.as_str()
                )));
            }
            if !(rule.min..=rule.max).contains(&rule.baseline) {
                return Err(InvalidRules(format!(
                    "{}: baseline must be within min..=max",
                    name.as_str()
                )));
            }
            if !rule.decay_per_min.is_finite() || !(0.0..=1.0).contains(&rule.decay_per_min) {
                return Err(InvalidRules(format!(
                    "{}: decay_per_min must be within 0..=1",
                    name.as_str()
                )));
            }
        }
        for (event, deltas) in &self.events {
            for (name, delta) in deltas {
                if !delta.is_finite() || delta.abs() > 100.0 {
                    return Err(InvalidRules(format!(
                        "events.{}.{}: delta must be finite and within -100..=100",
                        event.as_str(),
                        name.as_str()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Missing levels fall back to the defaults so partial configs stay valid.
    pub fn rule(&self, name: LevelName) -> LevelRule {
        self.levels.get(&name).copied().unwrap_or_else(|| {
            RegulationRules::default()
                .levels
                .get(&name)
                .copied()
                .unwrap_or(LevelRule::new(50.0, 0.0))
        })
    }

    fn clamp(&self, name: LevelName, value: f64) -> Level {
        let rule = self.rule(name);
        let value = value.clamp(rule.min, rule.max).clamp(0.0, 100.0);
        // Valid rules guarantee finiteness; fall back to the baseline otherwise.
        Level::new(value).unwrap_or_else(|_| Level::new(rule.baseline.clamp(0.0, 100.0)).unwrap())
    }

    /// Drift every level toward its baseline over `elapsed`.
    pub fn decay(&self, state: &RegulatoryState, elapsed: Duration) -> RegulatoryState {
        let minutes = elapsed.as_secs_f64() / 60.0;
        let mut next = state.clone();
        for name in LevelName::ALL {
            let rule = self.rule(name);
            let current = state.get(name).get();
            let retained = (1.0 - rule.decay_per_min).powf(minutes);
            let value = rule.baseline + (current - rule.baseline) * retained;
            next.set(name, self.clamp(name, value));
        }
        next
    }

    /// Apply one event's configured deltas.
    pub fn apply(&self, state: &RegulatoryState, event: RegulationEvent) -> RegulatoryState {
        let mut next = state.clone();
        if let Some(deltas) = self.events.get(&event) {
            for (name, delta) in deltas {
                let value = state.get(*name).get() + delta;
                next.set(*name, self.clamp(*name, value));
            }
        }
        next
    }

    /// Clamp an arbitrary state into the configured bounds (after a config change).
    pub fn bound(&self, state: &RegulatoryState) -> RegulatoryState {
        let mut next = state.clone();
        for name in LevelName::ALL {
            next.set(name, self.clamp(name, state.get(name).get()));
        }
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_match_initial_state() {
        let rules = RegulationRules::default();
        rules.validate().unwrap();
        let state = RegulatoryState::default();
        for name in LevelName::ALL {
            assert_eq!(rules.rule(name).baseline, state.get(name).get());
        }
    }

    #[test]
    fn decay_converges_toward_baseline() {
        let rules = RegulationRules::default();
        let state = RegulatoryState {
            stress: Level::new(90.0).unwrap(),
            ..Default::default()
        };
        let later = rules.decay(&state, Duration::from_secs(60 * 60));
        assert!(later.stress.get() < 20.0);
        assert!(later.stress.get() >= 10.0);
        let unchanged = rules.decay(&state, Duration::ZERO);
        assert_eq!(unchanged, state);
    }

    #[test]
    fn events_apply_and_clamp_to_bounds() {
        let mut rules = RegulationRules::default();
        rules.levels.insert(
            LevelName::Confidence,
            LevelRule {
                baseline: 50.0,
                decay_per_min: 0.0,
                min: 20.0,
                max: 55.0,
            },
        );
        let mut state = RegulatoryState::default();
        for _ in 0..10 {
            state = rules.apply(&state, RegulationEvent::PositiveFeedback);
        }
        assert_eq!(state.confidence.get(), 55.0);
        for _ in 0..30 {
            state = rules.apply(&state, RegulationEvent::NegativeFeedback);
        }
        assert_eq!(state.confidence.get(), 20.0);
        assert!(state.stress.get() <= 100.0);
    }

    #[test]
    fn invalid_rules_are_rejected() {
        let mut rules = RegulationRules::default();
        rules.levels.insert(
            LevelName::Energy,
            LevelRule {
                baseline: 90.0,
                decay_per_min: 0.1,
                min: 0.0,
                max: 80.0,
            },
        );
        assert!(rules.validate().is_err());
        let mut rules = RegulationRules::default();
        rules.levels.insert(
            LevelName::Energy,
            LevelRule {
                baseline: 50.0,
                decay_per_min: 2.0,
                min: 0.0,
                max: 100.0,
            },
        );
        assert!(rules.validate().is_err());
        let mut rules = RegulationRules::default();
        rules.events.insert(
            RegulationEvent::Rest,
            BTreeMap::from([(LevelName::Energy, f64::NAN)]),
        );
        assert!(rules.validate().is_err());
    }

    #[test]
    fn partial_rules_fall_back_to_defaults() {
        let rules: RegulationRules = serde_json::from_str(r#"{"levels":{}}"#).unwrap();
        assert_eq!(rules.rule(LevelName::Energy).baseline, 75.0);
        assert!(rules.events.contains_key(&RegulationEvent::Rest));
    }
}
