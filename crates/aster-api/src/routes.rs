//! Read-only Phase 1 routes. Unsupported future capabilities are not exposed.

use aster_core::{Action, Actor, PermissionDecision, PermissionPolicy};
use aster_storage::Store;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;

use crate::auth::{OwnerToken, require_owner};

pub fn router(store: Store, token: OwnerToken) -> Router {
    let private = Router::new()
        .route("/health/ready", get(ready))
        .route("/v1/identity", get(identity))
        .route("/v1/state", get(regulatory_state))
        .route("/v1/status", get(status))
        .route("/v1/policy", get(policy))
        .route_layer(middleware::from_fn_with_state(token, require_owner));

    Router::new()
        .route("/health/live", get(live))
        .merge(private)
        .with_state(store)
        .layer(middleware::from_fn(no_store))
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn live() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "status": "alive" }))
}

async fn ready(State(store): State<Store>) -> Response {
    match store.is_ready().await {
        Ok(true) => Json(serde_json::json!({ "status": "ready" })).into_response(),
        Ok(false) | Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "status": "not_ready" })),
        )
            .into_response(),
    }
}

async fn identity(State(store): State<Store>) -> Response {
    match store.identity().await {
        Ok(Some(identity)) => Json(identity).into_response(),
        Ok(None) => unavailable("not_initialized"),
        Err(_) => unavailable("storage_unavailable"),
    }
}

async fn regulatory_state(State(store): State<Store>) -> Response {
    match store.regulatory_state().await {
        Ok(Some(state)) => Json(state).into_response(),
        Ok(None) => unavailable("not_initialized"),
        Err(_) => unavailable("storage_unavailable"),
    }
}

async fn status(State(store): State<Store>) -> Response {
    match store.is_ready().await {
        Ok(true) => Json(serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "phase": 1,
            "ready": true,
            "runtime_enabled": false,
            "ai_enabled": false,
            "browsing_enabled": false,
        }))
        .into_response(),
        Ok(false) => unavailable("not_initialized"),
        Err(_) => unavailable("storage_unavailable"),
    }
}

fn unavailable(code: &'static str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": code })),
    )
        .into_response()
}

#[derive(Serialize)]
struct PolicyEntry {
    action: Action,
    decision: PermissionDecision,
}

async fn policy() -> Json<serde_json::Value> {
    let actions = [
        Action::ReadPrivateIdentity,
        Action::ManageMemory,
        Action::PlanInternalTask,
        Action::RegulateState,
        Action::RequestSource,
        Action::BrowseSource,
        Action::EditProtectedIdentity,
        Action::ChangePermissions,
        Action::ExecuteCode,
        Action::SendMessage,
        Action::SpendMoney,
        Action::ModifyInfrastructure,
    ];
    let entries: Vec<_> = actions
        .into_iter()
        .map(|action| PolicyEntry {
            action,
            decision: PermissionPolicy.decide(Actor::Autonomous, action),
        })
        .collect();
    Json(serde_json::json!({ "actor": "autonomous", "actions": entries }))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    use super::*;

    const TOKEN: &str = "a84276d93670f15ce499bd5680ef2c317690e803a1d59ba7f986ca451d2b036e";

    fn app() -> Router {
        let pool = PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(25))
            .connect_lazy("postgres://aster@127.0.0.1:1/aster")
            .unwrap();
        router(Store::from_pool(pool), OwnerToken::new(TOKEN).unwrap())
    }

    #[tokio::test]
    async fn liveness_does_not_need_authentication_or_database() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let value: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value, serde_json::json!({ "status": "alive" }));
    }

    #[tokio::test]
    async fn every_private_route_rejects_missing_or_wrong_credentials() {
        for path in [
            "/health/ready",
            "/v1/identity",
            "/v1/state",
            "/v1/status",
            "/v1/policy",
        ] {
            for authorization in [
                None,
                Some("Bearer owner"),
                Some("Basic owner"),
                Some("Bearer "),
            ] {
                let mut request = Request::builder().uri(path);
                if let Some(value) = authorization {
                    request = request.header(header::AUTHORIZATION, value);
                }
                let response = app()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
                assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
                assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            }
        }
    }

    #[tokio::test]
    async fn query_and_body_claims_do_not_authenticate_owner() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/policy?token={TOKEN}&actor=owner"))
                    .body(Body::from(format!(
                        r#"{{"actor":"owner","token":"{TOKEN}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn duplicate_authorization_is_rejected() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/v1/policy")
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn authenticated_policy_is_read_only_and_denies_core_changes() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/v1/policy")
                    .header(header::AUTHORIZATION, format!("bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
        let value: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(value["actor"], "autonomous");
        let actions = value["actions"].as_array().unwrap();
        assert_eq!(actions.len(), 12);
        for name in ["edit_protected_identity", "change_permissions"] {
            assert_eq!(
                actions
                    .iter()
                    .find(|entry| entry["action"] == name)
                    .unwrap()["decision"],
                "denied"
            );
        }
        assert_eq!(
            actions
                .iter()
                .find(|entry| entry["action"] == "browse_source")
                .unwrap()["decision"],
            "requires_owner_approval"
        );
    }

    #[tokio::test]
    async fn database_failure_returns_redacted_unavailable_response() {
        for path in ["/health/ready", "/v1/identity", "/v1/state", "/v1/status"] {
            let response = app()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            let body = response.into_body().collect().await.unwrap().to_bytes();
            let text = std::str::from_utf8(&body).unwrap();
            assert!(!text.contains(TOKEN));
            assert!(!text.contains("postgres://"));
            assert!(!text.contains("127.0.0.1"));
        }
    }

    #[tokio::test]
    async fn identity_has_no_mutation_route_even_for_authenticated_owner() {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let response = app()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/v1/identity")
                        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                        .body(Body::from(r#"{"protected":{"name":"other"}}"#))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        }
    }
}
