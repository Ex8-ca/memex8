use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::middleware::Next;
use axum::http::Request;

/// Bearer token auth middleware.
pub async fn auth_middleware(
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // TODO: read API key from config and compare with Authorization header
    Ok(next.run(req).await)
}
