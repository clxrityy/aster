//! Phase 1 regulatory snapshots only: no elapsed-time or action-based dynamics.
//! Levels model internal regulatory state, not subjective feelings or authority.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Error)]
#[error("regulatory level must be finite and within 0..=100; received {0}")]
pub struct InvalidLevel(pub f64);

/// A finite regulatory level in the inclusive range 0..=100.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Level(f64);

impl Level {
    pub fn new(value: f64) -> Result<Self, InvalidLevel> {
        if value.is_finite() && (0.0..=100.0).contains(&value) {
            Ok(Self(value))
        } else {
            Err(InvalidLevel(value))
        }
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Level {
    type Error = InvalidLevel;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Level> for f64 {
    fn from(level: Level) -> Self {
        level.get()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegulatoryState {
    pub energy: Level,
    pub focus: Level,
    pub cognitive_load: Level,
    pub curiosity: Level,
    pub stimulation: Level,
    pub stress: Level,
    pub confidence: Level,
}

impl Default for RegulatoryState {
    fn default() -> Self {
        Self {
            energy: Level(75.0),
            focus: Level(70.0),
            cognitive_load: Level(10.0),
            curiosity: Level(60.0),
            stimulation: Level(40.0),
            stress: Level(10.0),
            confidence: Level(50.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::value::{Error as ValueError, F64Deserializer};

    #[test]
    fn constructor_accepts_boundaries_and_fractional_levels() {
        for value in [0.0, -0.0, 0.001, 50.5, 99.999, 100.0] {
            assert_eq!(Level::new(value).unwrap().get(), value);
        }
    }

    #[test]
    fn constructor_rejects_out_of_range_levels() {
        for value in [-f64::EPSILON, -1.0, 100.000_001, f64::MAX] {
            assert_eq!(Level::new(value), Err(InvalidLevel(value)));
        }
    }

    #[test]
    fn constructor_rejects_nan_and_infinities() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(Level::new(value).is_err());
        }
    }

    #[test]
    fn numeric_serde_round_trip_includes_boundaries() {
        for value in [0.0, 42.5, 100.0] {
            let level = Level::new(value).unwrap();
            let json = serde_json::to_value(level).unwrap();
            assert_eq!(json, serde_json::json!(value));
            assert_eq!(serde_json::from_value::<Level>(json).unwrap(), level);
        }
    }

    #[test]
    fn serde_rejects_invalid_numeric_levels() {
        for json in ["-0.001", "-1", "100.001", "101", "1e999"] {
            assert!(serde_json::from_str::<Level>(json).is_err());
        }
        // JSON cannot represent nonfinite numbers; exercise Serde directly.
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let deserializer = F64Deserializer::<ValueError>::new(value);
            assert!(Level::deserialize(deserializer).is_err());
        }
    }

    #[test]
    fn serde_rejects_nonnumeric_values() {
        for json in ["null", "true", "\"50\"", "[]", "{}"] {
            assert!(serde_json::from_str::<Level>(json).is_err());
        }
    }

    #[test]
    fn conversions_preserve_validation() {
        assert_eq!(f64::from(Level::try_from(12.5).unwrap()), 12.5);
        assert!(Level::try_from(101.0).is_err());
    }

    #[test]
    fn default_state_has_exact_initial_levels() {
        let state = RegulatoryState::default();
        assert_eq!(state.energy.get(), 75.0);
        assert_eq!(state.focus.get(), 70.0);
        assert_eq!(state.cognitive_load.get(), 10.0);
        assert_eq!(state.curiosity.get(), 60.0);
        assert_eq!(state.stimulation.get(), 40.0);
        assert_eq!(state.stress.get(), 10.0);
        assert_eq!(state.confidence.get(), 50.0);
    }

    #[test]
    fn state_round_trip_and_every_field_validation() {
        let state = RegulatoryState::default();
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(
            serde_json::from_value::<RegulatoryState>(value.clone()).unwrap(),
            state
        );
        for field in [
            "energy",
            "focus",
            "cognitive_load",
            "curiosity",
            "stimulation",
            "stress",
            "confidence",
        ] {
            for invalid in [-1.0, 101.0] {
                let mut malformed = value.clone();
                malformed[field] = serde_json::json!(invalid);
                assert!(serde_json::from_value::<RegulatoryState>(malformed).is_err());
            }
        }
    }
}
