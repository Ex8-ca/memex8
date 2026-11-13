//! Username/password login for the web UI.
//!
//! The REST API keeps its Bearer key (agents, MCP bridge, plugins). Browsers
//! can instead sign in with a username + password and receive a signed,
//! HttpOnly session cookie, so the API key never has to be pasted into (or
//! stored by) the browser.
//!
//! Configuration (environment, same place as `MEMEX8_API_KEY`):
//! - `MEMEX8_WEB_USER`           login name
//! - `MEMEX8_WEB_PASSWORD_HASH`  argon2 PHC string from `memex8 hash-password`
//!
//! Login is enabled only when both are set. The session cookie is stateless:
//! `<expiry-unix>.<hmac-sha256>` keyed from the API key + password hash, so
//! changing either the key or the password invalidates every session.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const COOKIE_NAME: &str = "memex8_session";
const SESSION_TTL_SECS: u64 = 30 * 24 * 3600;
const MAX_FAILS: usize = 5;
const FAIL_WINDOW: Duration = Duration::from_secs(60);

type HmacSha256 = Hmac<Sha256>;

static FAILS: Mutex<Vec<Instant>> = Mutex::new(Vec::new());

// ─── primitives ──────────────────────────────────────────────

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::encode_b64(&rand::random::<[u8; 16]>())
        .map_err(|e| anyhow::anyhow!("salt: {e}"))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("hash: {e}"))
}

pub fn verify_password(password: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn signing_key(api_key: &str, phc: &str) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(b"memex8-session-v1\0");
    h.update(api_key.as_bytes());
    h.update(b"\0");
    h.update(phc.as_bytes());
    h.finalize().to_vec()
}

fn mac(key: &[u8], user: &str, exp: u64) -> String {
    let mut m = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    m.update(format!("v1|{user}|{exp}").as_bytes());
    hex(&m.finalize().into_bytes())
}

pub fn issue_token(key: &[u8], user: &str, now: u64) -> String {
    let exp = now + SESSION_TTL_SECS;
    format!("{exp}.{}", mac(key, user, exp))
}

pub fn check_token(key: &[u8], user: &str, token: &str, now: u64) -> bool {
    use subtle::ConstantTimeEq;
    let Some((exp_s, sig)) = token.split_once('.') else {
        return false;
    };
    let Ok(exp) = exp_s.parse::<u64>() else {
        return false;
    };
    if exp <= now {
        return false;
    }
    bool::from(mac(key, user, exp).as_bytes().ct_eq(sig.as_bytes()))
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ─── configuration ───────────────────────────────────────────

pub struct WebLogin {
    pub user: String,
    pub hash: String,
}

/// `Some` only when both env vars are set and non-empty.
pub fn web_login() -> Option<WebLogin> {
    let user = std::env::var("MEMEX8_WEB_USER").ok()?;
    let hash = std::env::var("MEMEX8_WEB_PASSWORD_HASH").ok()?;
    if user.is_empty() || hash.is_empty() {
        return None;
    }
    Some(WebLogin { user, hash })
}

fn api_key() -> Option<String> {
    std::env::var("MEMEX8_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
}

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    raw.split(';').find_map(|part| {
        let (k, v) = part.trim().split_once('=')?;
        (k == COOKIE_NAME).then(|| v.to_string())
    })
}

/// True when the request carries a valid session cookie. Used by the auth
/// middleware as an alternative to the Bearer key.
pub fn request_has_session(headers: &HeaderMap) -> bool {
    let (Some(login), Some(key), Some(tok)) = (web_login(), api_key(), cookie_value(headers))
    else {
        return false;
    };
    check_token(
        &signing_key(&key, &login.hash),
        &login.user,
        &tok,
        now_secs(),
    )
}

// ─── rate limiting (global: a single-user service) ───────────

fn locked_out() -> bool {
    let mut f = FAILS.lock().unwrap_or_else(|e| e.into_inner());
    f.retain(|t| t.elapsed() < FAIL_WINDOW);
    f.len() >= MAX_FAILS
}

fn record_fail() {
    FAILS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Instant::now());
}

fn clear_fails() {
    FAILS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

// ─── handlers (mounted OUTSIDE the auth middleware) ──────────

#[derive(Deserialize)]
pub struct LoginBody {
    pub username: String,
    pub password: String,
}

fn set_cookie(value: &str, max_age: u64, headers: &HeaderMap) -> HeaderValue {
    let secure = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("https"));
    let mut c =
        format!("{COOKIE_NAME}={value}; HttpOnly; SameSite=Strict; Path=/; Max-Age={max_age}");
    if secure {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).expect("cookie is ascii")
}

pub async fn login(headers: HeaderMap, Json(body): Json<LoginBody>) -> Response {
    let (Some(login), Some(key)) = (web_login(), api_key()) else {
        return (StatusCode::NOT_FOUND, "password login is not enabled").into_response();
    };
    if locked_out() {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "too many failed attempts, wait a minute",
        )
            .into_response();
    }

    // Always run the (slow) hash check, so a wrong username takes as long as
    // a wrong password.
    let hash = login.hash.clone();
    let pw = body.password.clone();
    let pw_ok = tokio::task::spawn_blocking(move || verify_password(&pw, &hash))
        .await
        .unwrap_or(false);
    use subtle::ConstantTimeEq;
    let user_ok = bool::from(body.username.as_bytes().ct_eq(login.user.as_bytes()));

    if !(pw_ok && user_ok) {
        record_fail();
        tracing::warn!("failed web login attempt");
        return (StatusCode::UNAUTHORIZED, "invalid username or password").into_response();
    }

    clear_fails();
    let token = issue_token(&signing_key(&key, &login.hash), &login.user, now_secs());
    let mut resp = Json(serde_json::json!({ "ok": true, "user": login.user })).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        set_cookie(&token, SESSION_TTL_SECS, &headers),
    );
    resp
}

pub async fn logout(headers: HeaderMap) -> Response {
    let mut resp = Json(serde_json::json!({ "ok": true })).into_response();
    resp.headers_mut()
        .insert(header::SET_COOKIE, set_cookie("", 0, &headers));
    resp
}

/// Tells the UI whether password login exists and whether this browser is in.
pub async fn me(headers: HeaderMap) -> Response {
    let enabled = web_login().is_some() && api_key().is_some();
    Json(serde_json::json!({
        "login_enabled": enabled,
        "authenticated": enabled && request_has_session(&headers),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_roundtrip() {
        let h = hash_password("correct horse").unwrap();
        assert!(h.starts_with("$argon2"));
        assert!(verify_password("correct horse", &h));
        assert!(!verify_password("wrong", &h));
        assert!(!verify_password("x", "not-a-phc-string"));
    }

    #[test]
    fn token_roundtrip_expiry_and_tamper() {
        let key = signing_key("api-key", "hash");
        let t = issue_token(&key, "marc", 1_000);
        assert!(check_token(&key, "marc", &t, 1_001));
        // expired
        assert!(!check_token(&key, "marc", &t, 1_000 + SESSION_TTL_SECS + 1));
        // wrong user
        assert!(!check_token(&key, "eve", &t, 1_001));
        // different api key / password hash => different signing key
        assert!(!check_token(
            &signing_key("other", "hash"),
            "marc",
            &t,
            1_001
        ));
        assert!(!check_token(
            &signing_key("api-key", "h2"),
            "marc",
            &t,
            1_001
        ));
        // forged expiry
        let (_, sig) = t.split_once('.').unwrap();
        assert!(!check_token(
            &key,
            "marc",
            &format!("99999999999.{sig}"),
            1_001
        ));
        assert!(!check_token(&key, "marc", "garbage", 1_001));
        assert!(!check_token(&key, "marc", "", 1_001));
    }

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; memex8_session=abc.def; b=2"),
        );
        assert_eq!(cookie_value(&h).as_deref(), Some("abc.def"));
        assert_eq!(cookie_value(&HeaderMap::new()), None);
    }

    #[test]
    fn cookie_flags() {
        let h = HeaderMap::new();
        let c = set_cookie("v", 10, &h);
        let s = c.to_str().unwrap();
        assert!(s.contains("HttpOnly") && s.contains("SameSite=Strict"));
        assert!(!s.contains("Secure"));
        let mut h2 = HeaderMap::new();
        h2.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(set_cookie("v", 10, &h2)
            .to_str()
            .unwrap()
            .contains("Secure"));
    }
}
