use crate::api::server::AppState;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

/// Paths that are intentionally reachable without credentials.
///
/// Health is public *by design*: `docker-compose.yml` probes
/// `/api/v1/health` with plain `curl` and no `Authorization` header, so the
/// container is marked unhealthy by its own healthcheck if health requires a
/// key. `/health` is the legacy alias used by the test fixtures and by
/// reverse proxies. Both routes are mounted on the auth-guarded router, so
/// this list — not route placement — is what keeps them open.
///
/// Nothing else belongs here. Every other path must fail closed.
pub fn is_public_path(path: &str) -> bool {
    matches!(path.trim_end_matches('/'), "/health" | "/api/v1/health")
}

/// Bearer token auth middleware.
/// Validates `Authorization: Bearer *** against the configured API key.
/// Exempts only the health endpoints listed in [`is_public_path`].
pub async fn auth_middleware(
    state: axum::extract::State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, (StatusCode, &'static str)> {
    let path = req.uri().path();

    // Public-by-design health probes; see `is_public_path`.
    if is_public_path(path) {
        return Ok(next.run(req).await);
    }

    let api_key = state.config.api_key();
    let Some(expected) = api_key else {
        // No API key configured — deny all requests (fail closed)
        tracing::warn!("API key not configured, rejecting request to {}", path);
        return Err((StatusCode::UNAUTHORIZED, "API key not configured"));
    };

    // A valid password-login session cookie is accepted in place of the key.
    if crate::api::session::request_has_session(req.headers()) {
        return Ok(next.run(req).await);
    }

    // Extract Bearer token
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let Some(auth_value) = auth_header else {
        return Err((StatusCode::UNAUTHORIZED, "Missing Authorization header"));
    };

    if !auth_value.starts_with("Bearer ") {
        return Err((
            StatusCode::UNAUTHORIZED,
            "Invalid Authorization format, expected 'Bearer <key>'",
        ));
    }

    let token = &auth_value["Bearer ".len()..];

    use subtle::ConstantTimeEq;
    if !bool::from(token.as_bytes().ct_eq(expected.as_bytes())) {
        tracing::warn!("Invalid API key for request to {}", path);
        return Err((StatusCode::UNAUTHORIZED, "Invalid API key"));
    }

    Ok(next.run(req).await)
}
