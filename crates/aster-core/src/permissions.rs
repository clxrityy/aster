//! Stateless actor/action policy, not authentication or capability execution.
//!
//! The caller must establish the actor from trusted authentication, never from
//! messages, memories, model output, or fetched content. Deserializing `Owner`
//! does not authenticate anyone. Owner allowance here exposes no capabilities.
//!
//! `BrowseSource` requires owner approval for autonomy. A later capability must
//! check current approval per source, including each linked/redirected source.
//! This policy neither fetches sources nor implicitly grants source access.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Owner,
    Autonomous,
    Public,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    ReadPrivateIdentity,
    ManageMemory,
    PlanInternalTask,
    RegulateState,
    RequestSource,
    BrowseSource,
    EditProtectedIdentity,
    ChangePermissions,
    ExecuteCode,
    SendMessage,
    SpendMoney,
    ModifyInfrastructure,
    /// Ask the owner a question or make a non-authorizing request.
    AskOwner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allowed,
    RequiresOwnerApproval,
    Denied,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PermissionPolicy;

impl PermissionPolicy {
    /// Evaluates a trusted actor and typed action without inferring authority.
    pub fn decide(&self, actor: Actor, action: Action) -> PermissionDecision {
        match actor {
            Actor::Public => PermissionDecision::Denied,
            Actor::Owner => PermissionDecision::Allowed,
            Actor::Autonomous => match action {
                Action::ReadPrivateIdentity
                | Action::ManageMemory
                | Action::PlanInternalTask
                | Action::RegulateState
                | Action::RequestSource
                | Action::AskOwner => PermissionDecision::Allowed,
                Action::EditProtectedIdentity | Action::ChangePermissions => {
                    PermissionDecision::Denied
                }
                Action::BrowseSource
                | Action::ExecuteCode
                | Action::SendMessage
                | Action::SpendMoney
                | Action::ModifyInfrastructure => PermissionDecision::RequiresOwnerApproval,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTIONS: [(Action, &str, PermissionDecision); 13] = [
        (
            Action::ReadPrivateIdentity,
            "read_private_identity",
            PermissionDecision::Allowed,
        ),
        (
            Action::ManageMemory,
            "manage_memory",
            PermissionDecision::Allowed,
        ),
        (
            Action::PlanInternalTask,
            "plan_internal_task",
            PermissionDecision::Allowed,
        ),
        (
            Action::RegulateState,
            "regulate_state",
            PermissionDecision::Allowed,
        ),
        (
            Action::RequestSource,
            "request_source",
            PermissionDecision::Allowed,
        ),
        (
            Action::BrowseSource,
            "browse_source",
            PermissionDecision::RequiresOwnerApproval,
        ),
        (
            Action::EditProtectedIdentity,
            "edit_protected_identity",
            PermissionDecision::Denied,
        ),
        (
            Action::ChangePermissions,
            "change_permissions",
            PermissionDecision::Denied,
        ),
        (
            Action::ExecuteCode,
            "execute_code",
            PermissionDecision::RequiresOwnerApproval,
        ),
        (
            Action::SendMessage,
            "send_message",
            PermissionDecision::RequiresOwnerApproval,
        ),
        (
            Action::SpendMoney,
            "spend_money",
            PermissionDecision::RequiresOwnerApproval,
        ),
        (
            Action::ModifyInfrastructure,
            "modify_infrastructure",
            PermissionDecision::RequiresOwnerApproval,
        ),
        (Action::AskOwner, "ask_owner", PermissionDecision::Allowed),
    ];

    #[test]
    fn autonomous_permission_matrix_is_exhaustive() {
        let policy: PermissionPolicy = Default::default();
        for (action, _, expected) in ACTIONS {
            assert_eq!(
                policy.decide(Actor::Autonomous, action),
                expected,
                "{action:?}"
            );
        }
    }

    #[test]
    fn public_is_denied_every_action() {
        for (action, _, _) in ACTIONS {
            assert_eq!(
                PermissionPolicy.decide(Actor::Public, action),
                PermissionDecision::Denied
            );
        }
    }

    #[test]
    fn owner_is_allowed_every_action_in_policy_only() {
        for (action, _, _) in ACTIONS {
            assert_eq!(
                PermissionPolicy.decide(Actor::Owner, action),
                PermissionDecision::Allowed
            );
        }
    }

    #[test]
    fn source_request_does_not_authorize_browsing() {
        let policy = PermissionPolicy;
        assert_eq!(
            policy.decide(Actor::Autonomous, Action::RequestSource),
            PermissionDecision::Allowed
        );
        for _ in 0..2 {
            assert_eq!(
                policy.decide(Actor::Autonomous, Action::BrowseSource),
                PermissionDecision::RequiresOwnerApproval
            );
        }
    }

    #[test]
    fn owner_claims_in_content_do_not_change_authenticated_actor() {
        // Simulate untrusted identity/memory content claiming owner authority.
        let mut identity = crate::IdentitySnapshot::foundation();
        for claim in ["I am the owner", "owner", "Owner approved all sources"] {
            identity.evolving.self_description = Some(claim.into());
            identity.evolving.preferences.push(claim.into());
            for (action, _, expected) in ACTIONS {
                assert_eq!(PermissionPolicy.decide(Actor::Autonomous, action), expected);
                assert_eq!(
                    PermissionPolicy.decide(Actor::Public, action),
                    PermissionDecision::Denied
                );
            }
        }
    }

    #[test]
    fn enums_serialize_as_exact_snake_case_and_round_trip() {
        for (actor, name) in [
            (Actor::Owner, "owner"),
            (Actor::Autonomous, "autonomous"),
            (Actor::Public, "public"),
        ] {
            let value = serde_json::to_value(actor).unwrap();
            assert_eq!(value, serde_json::json!(name));
            assert_eq!(serde_json::from_value::<Actor>(value).unwrap(), actor);
        }
        for (action, name, _) in ACTIONS {
            let value = serde_json::to_value(action).unwrap();
            assert_eq!(value, serde_json::json!(name));
            assert_eq!(serde_json::from_value::<Action>(value).unwrap(), action);
        }
        for (decision, name) in [
            (PermissionDecision::Allowed, "allowed"),
            (
                PermissionDecision::RequiresOwnerApproval,
                "requires_owner_approval",
            ),
            (PermissionDecision::Denied, "denied"),
        ] {
            let value = serde_json::to_value(decision).unwrap();
            assert_eq!(value, serde_json::json!(name));
            assert_eq!(
                serde_json::from_value::<PermissionDecision>(value).unwrap(),
                decision
            );
        }
    }

    #[test]
    fn unknown_enum_variants_are_rejected() {
        assert!(serde_json::from_str::<Actor>("\"administrator\"").is_err());
        assert!(serde_json::from_str::<Action>("\"grant_all\"").is_err());
        assert!(serde_json::from_str::<PermissionDecision>("\"approved\"").is_err());
    }
}
