//! Owner-only memory endpoints. Bodies and provenance never establish actors,
//! rewrite identity, or authorize fetching. Extractor errors are redacted.

use aster_core::{MemoryCursor, MemoryDraft, MemoryId, MemoryPageSize};
use aster_storage::{StorageError, Store};
use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        rejection::{JsonRejection, PathRejection, QueryRejection},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get},
};
use serde::Deserialize;

use super::unavailable;

// JSON escapes may take six wire bytes per content byte. Bound raw bodies too,
// while still accepting maximally escaped input within the domain text limits.
const MAX_BODY_BYTES: usize = 128 * 1024;

pub(super) fn router() -> Router<Store> {
    Router::new()
        .route("/v1/memories", get(list).post(create))
        .route("/v1/memories/{id}", delete(remove))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

async fn create(
    State(store): State<Store>,
    input: Result<Json<MemoryDraft>, JsonRejection>,
) -> Response {
    let draft = match input {
        Ok(Json(draft)) => draft,
        Err(error) => {
            return match error.status() {
                StatusCode::PAYLOAD_TOO_LARGE => {
                    failure(StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
                }
                StatusCode::UNSUPPORTED_MEDIA_TYPE => {
                    failure(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type")
                }
                _ => failure(StatusCode::BAD_REQUEST, "invalid_memory"),
            };
        }
    };
    match store.create_memory(draft).await {
        Ok(memory) => (StatusCode::CREATED, Json(memory)).into_response(),
        Err(error) => storage_failure(error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    limit: Option<u16>,
    before: Option<String>,
}

async fn list(
    State(store): State<Store>,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return failure(StatusCode::BAD_REQUEST, "invalid_memory_query");
    };
    let limit = match query.limit {
        Some(size) => match MemoryPageSize::new(size) {
            Ok(limit) => limit,
            Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid_memory_query"),
        },
        None => MemoryPageSize::default(),
    };
    let before = match query
        .before
        .map(|value| value.parse::<MemoryCursor>())
        .transpose()
    {
        Ok(before) => before,
        Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid_memory_query"),
    };
    match store.list_memories(limit, before).await {
        Ok(page) => Json(page).into_response(),
        Err(error) => storage_failure(error),
    }
}

async fn remove(State(store): State<Store>, path: Result<Path<String>, PathRejection>) -> Response {
    let id = match path {
        Ok(Path(value)) => match value.parse::<MemoryId>() {
            Ok(id) => id,
            Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid_memory_id"),
        },
        Err(_) => return failure(StatusCode::BAD_REQUEST, "invalid_memory_id"),
    };
    match store.delete_memory(id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => failure(StatusCode::NOT_FOUND, "memory_not_found"),
        Err(error) => storage_failure(error),
    }
}

fn storage_failure(error: StorageError) -> Response {
    match error {
        StorageError::NotInitialized => unavailable("not_initialized"),
        _ => unavailable("storage_unavailable"),
    }
}

fn failure(status: StatusCode, code: &'static str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aster_core::memory::{MAX_MEMORY_CONTENT_BYTES, MAX_MEMORY_PROVENANCE_BYTES};
    use axum::{
        body::Body,
        http::{Request, header},
    };
    use http_body_util::BodyExt;
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    use crate::{auth::OwnerToken, router};

    use super::*;

    const TOKEN: &str = "a84276d93670f15ce499bd5680ef2c317690e803a1d59ba7f986ca451d2b036e";
    const ID: &str = "11111111-1111-4111-8111-111111111111";

    fn app() -> Router {
        let pool = PgPoolOptions::new()
            .acquire_timeout(Duration::from_millis(25))
            .connect_lazy("postgres://aster@127.0.0.1:1/aster")
            .unwrap();
        router(Store::from_pool(pool), OwnerToken::new(TOKEN).unwrap())
    }

    async fn request(method: &str, path: &str, body: String) -> Response {
        app()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn check_error(response: Response, status: StatusCode, code: &str) {
        assert_eq!(response.status(), status);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(
            response
                .headers()
                .get("access-control-allow-origin")
                .is_none()
        );
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body, serde_json::json!({ "error": code }));
    }

    #[tokio::test]
    async fn all_memory_methods_require_exactly_one_owner_header_before_parsing() {
        for (method, path) in [
            ("POST", "/v1/memories"),
            ("GET", "/v1/memories"),
            ("DELETE", "/v1/memories/invalid-id"),
        ] {
            for headers in [
                vec![],
                vec!["Bearer owner".to_string()],
                vec![format!("Bearer {TOKEN}"), format!("Bearer {TOKEN}")],
            ] {
                let mut builder = Request::builder()
                    .method(method)
                    .uri(format!("{path}?actor=owner&token={TOKEN}"));
                for value in headers {
                    builder = builder.header(header::AUTHORIZATION, value);
                }
                let response = app()
                    .oneshot(builder.body(Body::from("malformed private body")).unwrap())
                    .await
                    .unwrap();
                assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
                check_error(response, StatusCode::UNAUTHORIZED, "unauthorized").await;
            }
        }
    }

    #[tokio::test]
    async fn malformed_and_authority_bearing_memory_bodies_are_redacted() {
        for body in [
            "malformed-private-content".to_string(),
            "[]".to_string(),
            r#"["private-content","private-source"]"#.to_string(),
            r#"{"content":"a","content":"b","provenance":"private-source"}"#.to_string(),
            serde_json::json!({"content":"private-content"}).to_string(),
            serde_json::json!({"content":"private-content", "provenance":42}).to_string(),
            serde_json::json!({"content":"private-content", "provenance":"private-source", "actor":"owner"}).to_string(),
            serde_json::json!({"content":"private-content", "provenance":"private-source", "identity_id":ID}).to_string(),
            serde_json::json!({"content":"private-content", "provenance":"private-source", "id":ID}).to_string(),
        ] {
            check_error(request("POST", "/v1/memories", body).await, StatusCode::BAD_REQUEST, "invalid_memory").await;
        }
    }

    #[tokio::test]
    async fn memory_field_limits_apply_before_database_access() {
        for body in [
            serde_json::json!({"content":" \n", "provenance":"note"}),
            serde_json::json!({"content":"text", "provenance":"\u{2003}"}),
            serde_json::json!({"content":"text\0", "provenance":"note"}),
            serde_json::json!({"content":"x".repeat(MAX_MEMORY_CONTENT_BYTES + 1), "provenance":"note"}),
            serde_json::json!({"content":"text", "provenance":"x".repeat(MAX_MEMORY_PROVENANCE_BYTES + 1)}),
        ] {
            check_error(
                request("POST", "/v1/memories", body.to_string()).await,
                StatusCode::BAD_REQUEST,
                "invalid_memory",
            )
            .await;
        }
        check_error(
            request("POST", "/v1/memories", "x".repeat(MAX_BODY_BYTES + 1)).await,
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
        )
        .await;
    }

    #[tokio::test]
    async fn missing_content_type_is_redacted() {
        let response = app()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/memories")
                    .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
                    .body(Body::from("private-content"))
                    .unwrap(),
            )
            .await
            .unwrap();
        check_error(
            response,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unsupported_media_type",
        )
        .await;
    }

    #[tokio::test]
    async fn invalid_limits_cursors_and_unknown_query_fields_are_redacted() {
        for query in [
            "limit=0",
            "limit=101",
            "limit=-1",
            "limit=65536",
            "limit=secret",
            "limit=1&limit=2",
            "before=private-source",
            "before=-1000000000000000000:11111111-1111-4111-8111-111111111111",
            "actor=owner",
        ] {
            check_error(
                request("GET", &format!("/v1/memories?{query}"), String::new()).await,
                StatusCode::BAD_REQUEST,
                "invalid_memory_query",
            )
            .await;
        }
    }

    #[tokio::test]
    async fn invalid_memory_ids_are_redacted() {
        for id in ["private-content", "00000000-0000-0000-0000-000000000000"] {
            check_error(
                request("DELETE", &format!("/v1/memories/{id}"), String::new()).await,
                StatusCode::BAD_REQUEST,
                "invalid_memory_id",
            )
            .await;
        }
    }

    #[tokio::test]
    async fn valid_memory_requests_with_failed_storage_return_redacted_503() {
        for (method, path, body) in [
            (
                "POST",
                "/v1/memories".to_string(),
                serde_json::json!({"content":"private-content", "provenance":"private-source"})
                    .to_string(),
            ),
            ("GET", "/v1/memories".to_string(), String::new()),
            ("DELETE", format!("/v1/memories/{ID}"), String::new()),
        ] {
            check_error(
                request(method, &path, body).await,
                StatusCode::SERVICE_UNAVAILABLE,
                "storage_unavailable",
            )
            .await;
        }
    }

    #[tokio::test]
    async fn fully_escaped_maximum_input_is_accepted_by_wire_limit() {
        let body = format!(
            r#"{{"content":"a{}","provenance":"a{}"}}"#,
            "\\u0001".repeat(MAX_MEMORY_CONTENT_BYTES - 1),
            "\\u0001".repeat(MAX_MEMORY_PROVENANCE_BYTES - 1)
        );
        check_error(
            request("POST", "/v1/memories", body).await,
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_unavailable",
        )
        .await;
    }

    #[tokio::test]
    async fn update_methods_remain_unimplemented() {
        for method in ["PUT", "PATCH"] {
            let response = request(method, &format!("/v1/memories/{ID}"), String::new()).await;
            assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
    }
}
