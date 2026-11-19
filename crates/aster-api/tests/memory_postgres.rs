//! Authenticated memory API tests, opt-in against SQLx's isolated test databases.

use aster_api::{auth::OwnerToken, router};
use aster_storage::Store;
use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
    response::Response,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;

const TOKEN: &str = "a84276d93670f15ce499bd5680ef2c317690e803a1d59ba7f986ca451d2b036e";
const ID: &str = "11111111-1111-4111-8111-111111111111";

async fn request(app: &Router, method: &str, path: &str, body: &str) -> Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn json_response(response: Response, expected: StatusCode) -> Value {
    assert_eq!(response.status(), expected);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn owner_can_create_page_and_delete_private_memory(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let identity = store.initialize().await.unwrap().identity;
    let app = router(store.clone(), OwnerToken::new(TOKEN).unwrap());
    let empty = json_response(
        request(&app, "GET", "/v1/memories", "").await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(empty, json!({"memories":[], "next_cursor":null}));
    let input = json!({"content":"Private project context", "provenance":"Owner's project note"})
        .to_string();
    let first = json_response(
        request(&app, "POST", "/v1/memories", &input).await,
        StatusCode::CREATED,
    )
    .await;
    let second = json_response(
        request(&app, "POST", "/v1/memories", &input).await,
        StatusCode::CREATED,
    )
    .await;
    assert_ne!(first["id"], second["id"]);
    assert_eq!(first["content"], "Private project context");
    assert_eq!(first["provenance"], "Owner's project note");
    assert!(first["created_at"].is_string());
    assert_eq!(first.as_object().unwrap().len(), 4);
    let page = json_response(
        request(&app, "GET", "/v1/memories?limit=1", "").await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(page["memories"], json!([second]));
    let cursor = page["next_cursor"].as_str().unwrap();
    let last = json_response(
        request(
            &app,
            "GET",
            &format!("/v1/memories?limit=1&before={cursor}"),
            "",
        )
        .await,
        StatusCode::OK,
    )
    .await;
    assert_eq!(last["memories"], json!([first]));
    assert!(last["next_cursor"].is_null());
    let path = format!("/v1/memories/{}", first["id"].as_str().unwrap());
    let deleted = request(&app, "DELETE", &path, "").await;
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert_eq!(deleted.headers()[header::CACHE_CONTROL], "no-store");
    assert!(
        deleted
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .is_empty()
    );
    let missing = json_response(
        request(&app, "DELETE", &path, "").await,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(missing, json!({"error":"memory_not_found"}));
    assert_eq!(store.identity().await.unwrap(), Some(identity));
    let status = json_response(request(&app, "GET", "/v1/status", "").await, StatusCode::OK).await;
    assert_eq!(status["phase"], 3);
    assert_eq!(status["memory_enabled"], true);
    for flag in ["runtime_enabled", "external_ai_enabled", "browsing_enabled"] {
        assert_eq!(status[flag], false);
    }
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM aster_audit_events WHERE kind = 'memory_deleted'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn uninitialized_memory_api_never_creates_foundation(pool: PgPool) {
    let store = Store::from_pool(pool);
    let app = router(store.clone(), OwnerToken::new(TOKEN).unwrap());
    for (method, path, body) in [
        ("GET", "/v1/memories".to_string(), ""),
        (
            "POST",
            "/v1/memories".to_string(),
            r#"{"content":"private", "provenance":"note"}"#,
        ),
        ("DELETE", format!("/v1/memories/{ID}"), ""),
    ] {
        let result = json_response(
            request(&app, method, &path, body).await,
            StatusCode::SERVICE_UNAVAILABLE,
        )
        .await;
        assert_eq!(result, json!({"error":"not_initialized"}));
    }
    assert_eq!(store.identity().await.unwrap(), None);
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn caller_claims_do_not_create_or_delete_records(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    let identity = store.initialize().await.unwrap().identity;
    let app = router(store, OwnerToken::new(TOKEN).unwrap());
    for claim in ["actor", "identity_id", "id", "created_at"] {
        let mut input = json!({"content":"Private note", "provenance":"Owner note"});
        input[claim] = json!(identity.id.to_string());
        let result = json_response(
            request(&app, "POST", "/v1/memories", &input.to_string()).await,
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(result, json!({"error":"invalid_memory"}));
    }
    for (method, path) in [
        ("POST", "/v1/memories".to_string()),
        ("DELETE", format!("/v1/memories/{ID}")),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .body(Body::from(r#"{"actor":"owner"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            json_response(response, StatusCode::UNAUTHORIZED).await,
            json!({"error":"unauthorized"})
        );
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM aster_memories")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM aster_audit_events")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(audits, 1);
}

#[sqlx::test(migrations = "../../migrations")]
#[ignore = "requires disposable PostgreSQL; run with --include-ignored and DATABASE_URL"]
async fn corrupt_memory_and_closed_storage_fail_with_redacted_errors(pool: PgPool) {
    let store = Store::from_pool(pool.clone());
    store.initialize().await.unwrap();
    let app = router(store.clone(), OwnerToken::new(TOKEN).unwrap());
    request(
        &app,
        "POST",
        "/v1/memories",
        r#"{"content":"private-content", "provenance":"private-source"}"#,
    )
    .await;
    sqlx::query("UPDATE aster_memories SET content = $1")
        .bind("\u{2003}")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        json_response(
            request(&app, "GET", "/v1/memories", "").await,
            StatusCode::SERVICE_UNAVAILABLE
        )
        .await,
        json!({"error":"storage_unavailable"})
    );
    assert_eq!(
        json_response(
            request(&app, "GET", "/health/ready", "").await,
            StatusCode::OK
        )
        .await,
        json!({"status":"ready"})
    );
    store.close().await;
    for (method, path, body) in [
        ("GET", "/v1/memories".to_string(), ""),
        (
            "POST",
            "/v1/memories".to_string(),
            r#"{"content":"private-content", "provenance":"private-source"}"#,
        ),
        ("DELETE", format!("/v1/memories/{ID}"), ""),
    ] {
        assert_eq!(
            json_response(
                request(&app, method, &path, body).await,
                StatusCode::SERVICE_UNAVAILABLE
            )
            .await,
            json!({"error":"storage_unavailable"})
        );
    }
    assert_eq!(
        request(&app, "GET", "/health/live", "").await.status(),
        StatusCode::OK
    );
}
