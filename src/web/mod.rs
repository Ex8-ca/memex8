pub mod embedded;

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use once_cell::sync::Lazy;
use std::sync::RwLock;

/// API key injected at startup from the server's config.
pub static WEB_CONFIG: Lazy<RwLock<Option<String>>> = Lazy::new(|| RwLock::new(None));

/// Initialize the web config with the API key from the server.
/// Called once at startup from the API server.
pub fn init(api_key: Option<String>) {
    let mut cfg = WEB_CONFIG.write().unwrap();
    *cfg = api_key;
}

/// Serve the root path (index.html) with the API key injected.
pub async fn serve_root() -> impl IntoResponse {
    let key = WEB_CONFIG.read().unwrap().clone();
    match serve_file("index.html", &key).await {
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

    let key = WEB_CONFIG.read().unwrap().clone();
    match serve_file(&path, &key).await {
        Ok((headers, data)) => (headers, data).into_response(),
        Err(_) => {
            // SPA fallback: serve index.html for non-file routes
            match serve_file("index.html", &key).await {
                Ok((headers, data)) => (headers, data).into_response(),
                Err(status) => status.into_response(),
            }
        }
    }
}

/// Serve a file from embedded or filesystem assets.
/// For index.html, inject the API key from config.
async fn serve_file(path: &str, api_key: &Option<String>) -> WebResult {
    match embedded::get_file(path).await {
        Some((data, mime)) => {
            let data = if path == "index.html" {
                inject_api_key(&data, api_key.as_deref())
            } else {
                data
            };

            let mut headers = http::HeaderMap::new();
            headers.insert("content-type", mime.parse().unwrap());
            headers.insert("cache-control", "public, max-age=3600".parse().unwrap());
            Ok((headers, data))
        }
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// Replace __MEMEX8_API_KEY__ placeholder in the HTML with the actual key.
fn inject_api_key(html: &[u8], api_key: Option<&str>) -> Vec<u8> {
    let placeholder = "__MEMEX8_API_KEY__";
    match api_key {
        Some(key) => {
            let html_str = String::from_utf8_lossy(html);
            html_str.replace(placeholder, &format!("'{}'", key.replace('\'', "\\'"))).into_bytes()
        }
        None => {
            // No key configured — leave the placeholder so JS prompts the user
            html.to_vec()
        }
    }
}

type WebResult = Result<(http::HeaderMap, Vec<u8>), StatusCode>;
