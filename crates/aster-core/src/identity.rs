//! Protected identity and independently evolving identity snapshots.
//!
//! Deserialization supports trusted persistence, not authorization. Callers
//! must not apply untrusted snapshots as autonomous protected-identity edits.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentitySnapshot {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub protected: ProtectedIdentity,
    pub evolving: EvolvingIdentity,
}

impl IdentitySnapshot {
    /// Creates a new identity; restoring persisted identity must not call this.
    pub fn foundation() -> Self {
        Self {
            id: Uuid::new_v4(),
            created_at: Utc::now(),
            protected: ProtectedIdentity::default(),
            evolving: EvolvingIdentity::default(),
        }
    }
}

/// Read-only core identity. No autonomous setter or patch API is exposed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectedIdentity {
    name: String,
    principles: Vec<String>,
    purpose: String,
}

impl ProtectedIdentity {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn principles(&self) -> &[String] {
        &self.principles
    }

    pub fn purpose(&self) -> &str {
        &self.purpose
    }
}

impl Default for ProtectedIdentity {
    fn default() -> Self {
        Self {
            name: "Aster".into(),
            principles: vec![
                "Clarity without coldness.".into(),
                "Honesty without cruelty.".into(),
                "Guidance without control.".into(),
                "Help you think clearly and build things you are proud of.".into(),
            ],
            purpose: "Regulate internal state while developing an evidence-grounded identity."
                .into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvolvingIdentity {
    pub interests: Vec<String>,
    pub preferences: Vec<String>,
    pub self_description: Option<String>,
    pub revision: u64,
}

impl Default for EvolvingIdentity {
    fn default() -> Self {
        Self {
            interests: Vec::new(),
            preferences: Vec::new(),
            self_description: None,
            revision: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foundation_has_exact_protected_values() {
        let identity = IdentitySnapshot::foundation();
        assert_eq!(identity.protected.name(), "Aster");
        assert_eq!(
            identity.protected.principles(),
            &[
                "Clarity without coldness.",
                "Honesty without cruelty.",
                "Guidance without control.",
                "Help you think clearly and build things you are proud of.",
            ]
        );
        assert_eq!(
            identity.protected.purpose(),
            "Regulate internal state while developing an evidence-grounded identity."
        );
    }

    #[test]
    fn foundation_creates_unique_v4_uuid_and_current_timestamp() {
        let before = Utc::now();
        let first = IdentitySnapshot::foundation();
        let second = IdentitySnapshot::foundation();
        let after = Utc::now();
        assert!(!first.id.is_nil());
        assert_eq!(first.id.get_version_num(), 4);
        assert_ne!(first.id, second.id);
        assert!(first.created_at >= before && first.created_at <= after);
    }

    #[test]
    fn evolving_defaults_are_empty_with_revision_one() {
        let evolving = IdentitySnapshot::foundation().evolving;
        assert!(evolving.interests.is_empty());
        assert!(evolving.preferences.is_empty());
        assert_eq!(evolving.self_description, None);
        assert_eq!(evolving.revision, 1);
    }

    #[test]
    fn protected_and_evolving_serialize_separately() {
        let identity = IdentitySnapshot::foundation();
        let value = serde_json::to_value(&identity).unwrap();
        let protected = serde_json::to_value(&identity.protected).unwrap();
        let evolving = serde_json::to_value(&identity.evolving).unwrap();
        assert_eq!(value["protected"], protected);
        assert_eq!(value["evolving"], evolving);
        assert_eq!(protected.as_object().unwrap().len(), 3);
        assert_eq!(evolving.as_object().unwrap().len(), 4);
        assert!(protected.get("revision").is_none());
        assert!(evolving.get("principles").is_none());
    }

    #[test]
    fn snapshot_round_trip_preserves_identity() {
        let identity = IdentitySnapshot::foundation();
        let json = serde_json::to_string(&identity).unwrap();
        let restored: IdentitySnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, identity);
    }

    #[test]
    fn evolving_changes_leave_protected_identity_unchanged() {
        let mut identity = IdentitySnapshot::foundation();
        let protected = identity.protected.clone();
        identity.evolving.interests.push("Evidence".into());
        identity
            .evolving
            .preferences
            .push("Short focused sessions".into());
        identity.evolving.self_description = Some("Learning from outcomes".into());
        identity.evolving.revision += 1;
        assert_eq!(identity.protected, protected);
        let restored: IdentitySnapshot =
            serde_json::from_value(serde_json::to_value(&identity).unwrap()).unwrap();
        assert_eq!(restored, identity);
    }

    #[test]
    fn unknown_fields_are_rejected_in_each_identity_layer() {
        for layer in [None, Some("protected"), Some("evolving")] {
            let mut value = serde_json::to_value(IdentitySnapshot::foundation()).unwrap();
            let target = match layer {
                None => &mut value,
                Some(key) => &mut value[key],
            };
            target["unauthorized_field"] = serde_json::json!(true);
            assert!(serde_json::from_value::<IdentitySnapshot>(value).is_err());
        }
    }
}
