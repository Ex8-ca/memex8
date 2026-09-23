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
/// Returns 404 (not 500) when the file doesn't exist so the SPA fallback can
/// serve index.html for unknown routes. The router chain matches specific
/// paths first (`/`, `/health`, `/mcp`, `/api/v1/...`); only paths that fall
/// through to here go through `serve_static`.
pub async fn serve_static(Path(path): Path<String>) -> impl IntoResponse {
    // Security: prevent path traversal
    if path.contains("..") || path.starts_with('/') {
        return StatusCode::FORBIDDEN.into_response();
    }

    // Empty path (fallback hit) and paths to files we don't bundle (like
    // favicon.ico) should serve the SPA shell rather than 500. Detect the
    // latter by trying the embedded lookup first; only fall through to
    // index.html when we know we'd 404 anyway.
    let key = WEB_CONFIG.read().unwrap().clone();
    match serve_file(&path, &key).await {
        Ok((headers, data)) => (headers, data).into_response(),
        Err(StatusCode::NOT_FOUND) => {
            // SPA fallback for missing assets (e.g. /favicon.ico when we
            // don't ship one). The browser will then request index.html's
            // resources normally. Returning 404 here would break the SPA.
            match serve_file("index.html", &key).await {
                Ok((headers, data)) => (headers, data).into_response(),
                Err(status) => status.into_response(),
            }
        }
        Err(status) => status.into_response(),
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
/// We inject as a bare token (no quotes) — the HTML must wrap the placeholder
/// itself in valid JS quoting (e.g. backticks or double quotes). Doing it
/// this way avoids double-injection bugs when the HTML template already has
/// quote characters around the placeholder.
fn inject_api_key(html: &[u8], api_key: Option<&str>) -> Vec<u8> {
    let placeholder = "__MEMEX8_API_KEY__";
    match api_key {
        Some(key) => {
            let html_str = String::from_utf8_lossy(html);
            // Backslash-escape any backticks in the key so it stays valid
            // inside a JS template literal. The HTML's wrapping quotes are
            // the HTML's responsibility; we just emit the raw token.
            html_str.replace(placeholder, &key.replace('`', "\\`")).into_bytes()
        }
        None => {
            // No key configured — leave the placeholder so JS prompts the user
            html.to_vec()
        }
    }
}

type WebResult = Result<(http::HeaderMap, Vec<u8>), StatusCode>;
