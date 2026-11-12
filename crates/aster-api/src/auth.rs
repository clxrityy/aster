//! The owner actor is established by authentication, never by request content.

use axum::{
    Json,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use thiserror::Error;

/// Retain only a fixed-size digest, not a printable credential.
#[derive(Clone)]
pub struct OwnerToken {
    digest: [u8; 32],
}

#[derive(Debug, Error)]
#[error("ASTER_OWNER_TOKEN must be 64 random hexadecimal characters, not a placeholder")]
pub struct InvalidOwnerToken;

impl OwnerToken {
    pub fn new(value: &str) -> Result<Self, InvalidOwnerToken> {
        if value.len() != 64
            || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
            || value.bytes().all(|byte| byte == value.as_bytes()[0])
            || value.to_ascii_lowercase() == "0123456789abcdef".repeat(4)
        {
            return Err(InvalidOwnerToken);
        }
        Ok(Self {
            digest: Sha256::digest(value.as_bytes()).into(),
        })
    }

    fn matches(&self, value: &str) -> bool {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return false;
        }
        let candidate: [u8; 32] = Sha256::digest(value.as_bytes()).into();
        bool::from(self.digest.ct_eq(&candidate))
    }
}

pub async fn require_owner(
    State(token): State<OwnerToken>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers().get_all(header::AUTHORIZATION);
    let mut values = headers.iter();
    let authorized = values
        .next()
        .filter(|_| values.next().is_none())
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .is_some_and(|(scheme, value)| {
            scheme.eq_ignore_ascii_case("bearer") && token.matches(value)
        });

    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only fixtures, never generated or installed as deployment secrets.
    pub const TOKEN: &str = "a84276d93670f15ce499bd5680ef2c317690e803a1d59ba7f986ca451d2b036e";

    #[test]
    fn valid_token_matches_only_exact_credential() {
        let token = OwnerToken::new(TOKEN).unwrap();
        assert!(token.matches(TOKEN));
        assert!(!token.matches(&TOKEN.to_uppercase()));
        assert!(!token.matches(&"e".repeat(64)));
        assert!(!token.matches("owner"));
    }

    #[test]
    fn token_validation_rejects_blank_short_nonhex_and_placeholders() {
        for value in [
            "".into(),
            "change-me".into(),
            "a".repeat(63),
            "0".repeat(64),
            "f".repeat(64),
            "z".repeat(64),
            "0123456789abcdef".repeat(4),
            "0123456789ABCDEF".repeat(4),
            format!(" {TOKEN}"),
        ] {
            assert!(OwnerToken::new(&value).is_err());
        }
    }

    #[test]
    fn validation_error_never_contains_supplied_secret() {
        let supplied = "secret-invalid-token";
        let error = OwnerToken::new(supplied).err().unwrap().to_string();
        assert!(!error.contains(supplied));
    }
}
