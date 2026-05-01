use crate::api::server::AppState;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

/// Bearer token auth middleware.
/// Validates `Authorization: Bearer <key>` against the configured API key.
/// Exempts `/health` endpoint.
pub async fn auth_middleware(
    state: axum::extract::State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, (StatusCode, &'static str)> {
    let path = req.uri().path();

    // Exempt health check
    if path == "/health" {
        return Ok(next.run(req).await);
    }

    let api_key = state.config.api_key();
    let Some(expected) = api_key else {
        // No API key configured — deny all requests (fail closed)
        tracing::warn!("API key not configured, rejecting request to {}", path);
        return Err((StatusCode::UNAUTHORIZED, "API key not configured"));
    };

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
