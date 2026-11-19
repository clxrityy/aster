//! Explicitly opt into these isolated PostgreSQL tests with --include-ignored.

use aster_api::{auth::OwnerToken, router};
use aster_storage::Store;
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use sqlx::PgPool;
use tower::ServiceExt;

const TOKEN: &str = "a84276d93670f15ce499bd5680ef2c317690e803a1d59ba7f986ca451d2b036e";

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; use DATABASE_URL and --include-ignored"]
async fn api_reads_persisted_identity_and_initial_state(pool: PgPool) {
    let store = Store::from_pool(pool);
    let initial = store.initialize().await.unwrap();
    let app = router(store.clone(), OwnerToken::new(TOKEN).unwrap());
    for path in ["/health/ready", "/v1/identity", "/v1/state", "/v1/status"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let value: serde_json::Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        match path {
            "/v1/identity" => {
                assert_eq!(value["id"], initial.identity.id.to_string());
                assert_eq!(value["protected"]["name"], "Aster");
            }
            "/v1/state" => assert_eq!(value["state"]["energy"], 75.0),
            "/v1/status" => {
                assert_eq!(value["runtime_enabled"], false);
                assert_eq!(value["external_ai_enabled"], false);
                assert_eq!(value["browsing_enabled"], false);
            }
            _ => assert_eq!(value["status"], "ready"),
        }
    }
    assert_eq!(store.identity().await.unwrap().unwrap(), initial.identity);
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires PostgreSQL; use DATABASE_URL and --include-ignored"]
async fn uninitialized_api_is_unavailable_and_never_creates_identity(pool: PgPool) {
    let store = Store::from_pool(pool);
    let app = router(store.clone(), OwnerToken::new(TOKEN).unwrap());
    for path in ["/health/ready", "/v1/identity", "/v1/state", "/v1/status"] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("authorization", format!("Bearer {TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    assert!(store.identity().await.unwrap().is_none());
}
