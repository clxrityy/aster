//! Questions and requests Aster sends to its owner. A request never authorizes
//! anything by itself; denied actions are never even proposed.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::permissions::{Action, Actor, PermissionDecision, PermissionPolicy};

pub const MAX_OUTBOUND_TEXT_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundKind {
    Question,
    Request,
}

impl OutboundKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OutboundKind::Question => "question",
            OutboundKind::Request => "request",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundStatus {
    Pending,
    Answered,
    Approved,
    Denied,
    Expired,
}

impl OutboundStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            OutboundStatus::Pending => "pending",
            OutboundStatus::Answered => "answered",
            OutboundStatus::Approved => "approved",
            OutboundStatus::Denied => "denied",
            OutboundStatus::Expired => "expired",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "pending" => OutboundStatus::Pending,
            "answered" => OutboundStatus::Answered,
            "approved" => OutboundStatus::Approved,
            "denied" => OutboundStatus::Denied,
            "expired" => OutboundStatus::Expired,
            _ => return None,
        })
    }
}

/// What a request asks the runtime to do if approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestEffect {
    /// Pause initiative and apply the rest event.
    Rest,
    /// Read the page at the draft's `source` URL.
    Fetch,
}

/// Longest URL a request may carry.
pub const MAX_SOURCE_BYTES: usize = 2048;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutboundDraft {
    pub kind: OutboundKind,
    pub body: String,
    pub rationale: String,
    /// A term Aster asks about; the answer teaches it.
    pub subject: Option<String>,
    /// The owner prompt Aster could not answer; the answer becomes a reply.
    pub prompt: Option<String>,
    pub action: Option<Action>,
    pub effect: Option<RequestEffect>,
    /// The page a `Fetch` request asks to read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Error)]
pub enum OutboundRejected {
    #[error("outbound text must be nonblank and at most 4096 bytes")]
    Text,
    #[error("the autonomous actor is denied this action")]
    Denied,
    #[error("a fetch request needs one http(s) URL of at most 2048 bytes")]
    Source,
}

impl OutboundDraft {
    pub fn question(body: impl Into<String>, rationale: impl Into<String>) -> Self {
        Self {
            kind: OutboundKind::Question,
            body: body.into(),
            rationale: rationale.into(),
            subject: None,
            prompt: None,
            action: Some(Action::AskOwner),
            effect: None,
            source: None,
        }
    }

    /// Ask to read a page. Always a request: nothing is fetched until the
    /// owner approves, and the runtime checks the URL again then.
    pub fn fetch(
        url: impl Into<String>,
        body: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            kind: OutboundKind::Request,
            body: body.into(),
            rationale: rationale.into(),
            subject: None,
            prompt: None,
            action: Some(Action::BrowseSource),
            effect: Some(RequestEffect::Fetch),
            source: Some(url.into()),
        }
    }

    /// Check text bounds and that the policy does not deny the action.
    pub fn admit(self) -> Result<Self, OutboundRejected> {
        for text in [&self.body, &self.rationale] {
            if text.trim().is_empty() || text.len() > MAX_OUTBOUND_TEXT_BYTES || text.contains('\0')
            {
                return Err(OutboundRejected::Text);
            }
        }
        if let Some(action) = self.action
            && PermissionPolicy.decide(Actor::Autonomous, action) == PermissionDecision::Denied
        {
            return Err(OutboundRejected::Denied);
        }
        let fetch = self.effect == Some(RequestEffect::Fetch);
        let source_ok = self.source.as_deref().is_some_and(|url| {
            (url.starts_with("https://") || url.starts_with("http://"))
                && url.len() <= MAX_SOURCE_BYTES
                && !url.contains(char::is_whitespace)
        });
        if fetch != self.source.is_some()
            || (fetch && (!source_ok || self.action != Some(Action::BrowseSource)))
        {
            return Err(OutboundRejected::Source);
        }
        Ok(self)
    }

    /// Whether the owner must approve (rather than simply answer) this item.
    pub fn needs_approval(&self) -> bool {
        self.kind == OutboundKind::Request
            || self.action.is_some_and(|action| {
                PermissionPolicy.decide(Actor::Autonomous, action)
                    == PermissionDecision::RequiresOwnerApproval
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_actions_are_never_admitted() {
        for action in [Action::EditProtectedIdentity, Action::ChangePermissions] {
            let mut draft = OutboundDraft::question("May I?", "testing");
            draft.kind = OutboundKind::Request;
            draft.action = Some(action);
            assert_eq!(draft.admit(), Err(OutboundRejected::Denied));
        }
    }

    #[test]
    fn approval_actions_become_requests_needing_approval() {
        let mut draft = OutboundDraft::question("May I browse?", "curious");
        draft.action = Some(Action::BrowseSource);
        let admitted = draft.admit().unwrap();
        assert!(admitted.needs_approval());
        assert!(
            !OutboundDraft::question("What is X?", "unknown")
                .admit()
                .unwrap()
                .needs_approval()
        );
    }

    #[test]
    fn fetch_requests_need_approval_and_a_web_url() {
        let draft = OutboundDraft::fetch("https://example.org/a", "May I read it?", "curious")
            .admit()
            .unwrap();
        assert!(draft.needs_approval());
        for bad in ["file:///etc/passwd", "https://a b", "javascript:alert(1)"] {
            assert_eq!(
                OutboundDraft::fetch(bad, "May I?", "r").admit(),
                Err(OutboundRejected::Source)
            );
        }
        let mut sneaky = OutboundDraft::fetch("https://example.org", "May I?", "r");
        sneaky.action = Some(Action::AskOwner);
        assert!(sneaky.admit().is_err());
        let mut stray = OutboundDraft::question("What is X?", "r");
        stray.source = Some("https://example.org".into());
        assert!(stray.admit().is_err());
    }

    #[test]
    fn blank_or_oversized_text_is_rejected() {
        assert!(OutboundDraft::question(" ", "r").admit().is_err());
        assert!(
            OutboundDraft::question("x".repeat(5000), "r")
                .admit()
                .is_err()
        );
    }
}
