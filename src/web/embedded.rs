//! Embedded web assets for the memex8 web UI.
//!
//! At runtime, files are served from:
//! 1. `./web-dist/` directory (if it exists, for development)
//! 2. Compiled-in assets (for production binary)

use std::path::Path;

pub async fn get_file(path: &str) -> Option<(Vec<u8>, &'static str)> {
    // Try filesystem first (development mode)
    let local_path = Path::new("web-dist").join(path);
    if local_path.exists() {
        if let Ok(data) = tokio::fs::read(&local_path).await {
            let mime = mime_from_ext(path);
            return Some((data, mime));
        }
    }

    // Try compiled-in assets (production)
    get_compiled_file(path)
}

fn get_compiled_file(path: &str) -> Option<(Vec<u8>, &'static str)> {
    match path {
        "index.html" | "" | "/" => Some((INDEX_HTML.to_vec(), "text/html; charset=utf-8")),
        _ => None,
    }
}

fn mime_from_ext(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}

// Compiled-in default web UI
const INDEX_HTML: &[u8] = include_bytes!("../../web-dist/index.html");
