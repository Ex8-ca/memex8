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

/// Serve the root path (index.html). The API key is never embedded.
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
/// index.html passes through `inject_api_key`, which strips the placeholder only.
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

/// Scrub the legacy `__MEMEX8_API_KEY__` placeholder from served HTML.
///
/// SECURITY: the server's API key is never written into a page. Anyone who
/// could fetch `/` (LAN, tunnel, reverse proxy) would otherwise read the key
/// that guards every write endpoint. The `api_key` argument is accepted only
/// so call sites stay unchanged; it is deliberately ignored. Clients enter
/// their key in the UI's Settings (stored in the browser's localStorage).
fn inject_api_key(html: &[u8], _api_key: Option<&str>) -> Vec<u8> {
    String::from_utf8_lossy(html)
        .replace("__MEMEX8_API_KEY__", "")
        .into_bytes()
}

type WebResult = Result<(http::HeaderMap, Vec<u8>), StatusCode>;

#[cfg(test)]
mod tests {
    use super::inject_api_key;

    #[test]
    fn served_html_never_contains_the_api_key() {
        let html = b"const K = `__MEMEX8_API_KEY__`;";
        let out = String::from_utf8(inject_api_key(html, Some("super-secret-key"))).unwrap();
        assert!(!out.contains("super-secret-key"));
        assert!(!out.contains("__MEMEX8_API_KEY__"));
    }
}
