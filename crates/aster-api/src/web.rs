//! The web dashboard: static assets embedded in the binary, and sign-in /
//! sign-out for browser sessions. Served on the API's own origin, so the
//! browser never needs CORS and never holds the owner token.

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State, rejection::JsonRejection},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;

use crate::auth::Auth;

/// (path, content type, bytes). Kept small and dependency-free: no build step.
const ASSETS: &[(&str, &str, &[u8])] = &[
    (
        "app.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/app.js"),
    ),
    (
        "api.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/api.js"),
    ),
    (
        "views.js",
        "text/javascript; charset=utf-8",
        include_bytes!("../web/views.js"),
    ),
    (
        "style.css",
        "text/css; charset=utf-8",
        include_bytes!("../web/style.css"),
    ),
    (
        "icon.svg",
        "image/svg+xml",
        include_bytes!("../web/icon.svg"),
    ),
];
const INDEX: &[u8] = include_bytes!("../web/index.html");

/// Strict defaults for every response: same-origin everything, no framing,
/// no inline script, no referrer leaks.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
connect-src 'self'; font-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; \
frame-ancestors 'none'";

pub fn router() -> Router<Auth> {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/assets/{name}", get(asset))
        .route("/v1/session", get(session).post(sign_in))
        .route("/v1/session/logout", post(sign_out))
        .layer(DefaultBodyLimit::max(4096))
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "error": "not_found" })),
    )
        .into_response()
}

async fn index(State(auth): State<Auth>) -> Response {
    if !auth.sessions.settings().enabled {
        return not_found();
    }
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX).into_response()
}

async fn asset(State(auth): State<Auth>, Path(name): Path<String>) -> Response {
    if !auth.sessions.settings().enabled {
        return not_found();
    }
    match ASSETS.iter().find(|(path, _, _)| *path == name) {
        Some((_, kind, bytes)) => ([(header::CONTENT_TYPE, *kind)], *bytes).into_response(),
        None => not_found(),
    }
}

/// Whether this browser is signed in, and its CSRF token if so.
async fn session(State(auth): State<Auth>, headers: HeaderMap) -> Response {
    if !auth.sessions.settings().enabled {
        return not_found();
    }
    match auth.sessions.csrf(&headers) {
        Some(csrf) => Json(serde_json::json!({ "authenticated": true, "csrf": csrf })),
        None => Json(serde_json::json!({ "authenticated": false })),
    }
    .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignIn {
    token: String,
}

async fn sign_in(State(auth): State<Auth>, input: Result<Json<SignIn>, JsonRejection>) -> Response {
    let sessions = &auth.sessions;
    if !sessions.settings().enabled {
        return not_found();
    }
    if let Some(wait) = sessions.locked_for() {
        let seconds = wait.as_secs().max(1);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, seconds.to_string())],
            Json(serde_json::json!({ "error": "locked", "retry_after": seconds })),
        )
            .into_response();
    }
    let token = input.map(|Json(input)| input.token).unwrap_or_default();
    if !auth.token.matches(token.trim()) {
        sessions.failed();
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "unauthorized" })),
        )
            .into_response();
    }
    let issued = sessions.issue();
    let mut response = Json(serde_json::json!({
        "authenticated": true,
        "csrf": issued.csrf,
        "expires_in": issued.max_age.as_secs(),
    }))
    .into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, sessions.set_cookie(&issued));
    response
}

async fn sign_out(State(auth): State<Auth>, headers: HeaderMap) -> Response {
    auth.sessions.revoke(&headers);
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, auth.sessions.clear_cookie());
    response
}

/// `Cache-Control: no-store` plus the browser security headers, everywhere.
pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        (header::CACHE_CONTROL, "no-store"),
        (header::CONTENT_SECURITY_POLICY, CSP),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "no-referrer"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), interest-cohort=()"),
    );
    headers.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    response
}

#[cfg(test)]
mod tests {
    use axum::{body::Body, http::Method, middleware, routing::patch};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::{
        auth::{OwnerToken, require_owner},
        session::{CSRF_HEADER, Sessions},
    };

    const TOKEN: &str = "a84276d93670f15ce499bd5680ef2c317690e803a1d59ba7f986ca451d2b036e";

    fn app(enabled: bool) -> Router {
        let auth = Auth {
            token: OwnerToken::new(TOKEN).unwrap(),
            sessions: Sessions::new(move || aster_runtime::DashboardSection {
                enabled,
                login_max_failures: 2,
                ..Default::default()
            }),
        };
        let protected = Router::new()
            .route("/v1/private", get(|| async { "secret" }))
            .route("/v1/write", patch(|| async { "written" }))
            .route_layer(middleware::from_fn_with_state(auth.clone(), require_owner));
        protected
            .merge(router().with_state(auth))
            .layer(middleware::from_fn(security_headers))
    }

    fn request(method: Method, uri: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "aster.example.com")
    }

    async fn sign_in(app: &Router, token: &str) -> Response {
        app.clone()
            .oneshot(
                request(Method::POST, "/v1/session")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({ "token": token }).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn json(response: Response) -> serde_json::Value {
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    #[tokio::test]
    async fn shell_and_assets_are_served_with_strict_headers() {
        let app = app(true);
        let page = app
            .clone()
            .oneshot(request(Method::GET, "/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let headers = page.headers();
        assert!(
            headers[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        let script = app
            .clone()
            .oneshot(
                request(Method::GET, "/assets/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(script.status(), StatusCode::OK);
        assert!(
            script.headers()[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/javascript")
        );
        for path in ["/assets/../Cargo.toml", "/assets/missing.js"] {
            let response = app
                .clone()
                .oneshot(request(Method::GET, path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn disabled_dashboard_serves_nothing() {
        let app = app(false);
        for path in ["/", "/assets/app.js", "/v1/session"] {
            let response = app
                .clone()
                .oneshot(request(Method::GET, path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
        assert_eq!(sign_in(&app, TOKEN).await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn session_cookie_reads_and_writes_only_with_csrf() {
        let app = app(true);
        let response = sign_in(&app, TOKEN).await;
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let csrf = json(response).await["csrf"].as_str().unwrap().to_string();
        assert!(!cookie.contains(TOKEN) && !csrf.contains(TOKEN));

        let read = app
            .clone()
            .oneshot(
                request(Method::GET, "/v1/private")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(read.status(), StatusCode::OK);

        let write = |csrf: Option<&str>, origin: &str| {
            let mut builder = request(Method::PATCH, "/v1/write")
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, origin);
            if let Some(csrf) = csrf {
                builder = builder.header(CSRF_HEADER, csrf);
            }
            app.clone().oneshot(builder.body(Body::empty()).unwrap())
        };
        let same = "https://aster.example.com";
        assert_eq!(
            write(None, same).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            write(Some("wrong"), same).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            write(Some(&csrf), "https://evil.example")
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            write(Some(&csrf), same).await.unwrap().status(),
            StatusCode::OK
        );

        // A wrong bearer header is not rescued by a good cookie.
        let mixed = app
            .clone()
            .oneshot(
                request(Method::GET, "/v1/private")
                    .header(header::COOKIE, &cookie)
                    .header(header::AUTHORIZATION, "Bearer nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mixed.status(), StatusCode::UNAUTHORIZED);

        // Signing out ends the session.
        let out = app
            .clone()
            .oneshot(
                request(Method::POST, "/v1/session/logout")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(
            out.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        let after = app
            .clone()
            .oneshot(
                request(Method::GET, "/v1/private")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn wrong_tokens_lock_sign_in() {
        let app = app(true);
        let wrong = "b".repeat(63) + "c";
        assert_eq!(
            sign_in(&app, &wrong).await.status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            sign_in(&app, &wrong).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let locked = sign_in(&app, TOKEN).await;
        assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(locked.headers().contains_key(header::RETRY_AFTER));
        let status = app
            .clone()
            .oneshot(
                request(Method::GET, "/v1/session")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(json(status).await["authenticated"], false);
    }
}
