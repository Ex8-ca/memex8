pub mod embedded;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::IntoResponse;

type WebResult = Result<(http::HeaderMap, Vec<u8>), StatusCode>;

/// Serve the root path (index.html).
pub async fn serve_root() -> impl IntoResponse {
    match serve_file("index.html").await {
        Ok((headers, data)) => (headers, data).into_response(),
        Err(status) => status.into_response(),
    }
}

/// Serve static files by path.
pub async fn serve_static(Path(path): Path<String>) -> impl IntoResponse {
    // Security: prevent path traversal
    if path.contains("..") || path.starts_with('/') {
        return StatusCode::FORBIDDEN.into_response();
    }

    match serve_file(&path).await {
        Ok((headers, data)) => (headers, data).into_response(),
        Err(_) => {
            // SPA fallback: serve index.html for non-file routes
            match serve_file("index.html").await {
                Ok((headers, data)) => (headers, data).into_response(),
                Err(status) => status.into_response(),
            }
        }
    }
}

/// Serve a file from embedded or filesystem assets.
async fn serve_file(path: &str) -> WebResult {
    match embedded::get_file(path).await {
        Some((data, mime)) => {
            let mut headers = http::HeaderMap::new();
            headers.insert("content-type", mime.parse().unwrap());
            headers.insert("cache-control", "public, max-age=3600".parse().unwrap());
            Ok((headers, data))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}
