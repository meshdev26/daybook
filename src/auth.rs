//! Shared web authentication helpers (sessions, login throttling, cookies).
//!
//! Extracted from `web.rs` so the HTTP API (`api.rs`) and the web UI share
//! one implementation. Re-exported from `crate::web` for backwards
//! compatibility (`crate::web::session_from_headers` still works).

use crate::web::{Session, WebState};
use axum::http::HeaderMap;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Process-wide secret for CSRF token derivation. Filled once from the OS
/// CSPRNG (the same `getrandom::fill` pattern login/MFA already use). The
/// secret is never persisted or logged: tokens are derived from it and the
/// session credential, so each session gets a distinct, unpredictable value
/// without a database schema change (the DB is owned by another phase).
static CSRF_SECRET: OnceLock<[u8; 32]> = OnceLock::new();

fn csrf_secret() -> &'static [u8; 32] {
    CSRF_SECRET.get_or_init(|| {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).expect("OS CSPRNG failed while initializing CSRF secret");
        key
    })
}

/// HMAC-SHA256 over a fixed-size block, built directly on `sha2` (already a
/// direct dependency) so no new crate is introduced. Used only for CSRF
/// derivation.
fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::digest([ipad.as_slice(), data].concat());
    let outer = Sha256::digest([opad.as_slice(), inner.as_slice()].concat());
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer);
    out
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The per-session CSRF token: HMAC(secret, `session=<raw cookie token>`).
/// Stable for the lifetime of the session credential, different for every
/// session, and unguessable without the server secret.
pub fn csrf_token_for_session(session_token: &str) -> String {
    hex(&hmac_sha256(
        csrf_secret(),
        format!("session={session_token}").as_bytes(),
    ))
}

/// Token for browser forms rendered before a session exists (login, claim,
/// setup, MFA challenge/enroll). No session credential is available there, so
/// the value is derived from the process secret alone and is still an
/// unpredictable secret to any cross-origin attacker. `SameSite=Strict`
/// remains the primary defense on these doors.
pub fn anonymous_csrf_token() -> String {
    hex(&hmac_sha256(csrf_secret(), b"anonymous"))
}

/// Canonical key for the shared login failure bucket.
///
/// The web login (`web::auth_pages`) and the REST login (`api::auth_login`)
/// both key `login_attempts` with this shape so one username gets a single
/// 5-attempts-per-15-minutes counter across both surfaces. The `login:`
/// namespace also prevents a raw username from colliding with the `mfa:` and
/// `identity-unlock:` buckets held in the same map. Keep byte-identical to
/// the web key format when changing this.
pub fn login_throttle_key(username: &str) -> String {
    format!("login:{}", username)
}

pub fn login_throttle(
    map: &std::sync::Mutex<std::collections::HashMap<String, (u32, std::time::Instant)>>,
    key: &str,
    max: u32,
    window_secs: u64,
) -> bool {
    let mut map = map.lock().unwrap();
    let now = Instant::now();
    if map.len() > 1024 {
        map.retain(|_, at| now.duration_since(at.1) < Duration::from_secs(3600));
    }
    while map.len() > 1024 {
        let Some(oldest) = map
            .iter()
            .min_by_key(|(_, at)| at.1)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        map.remove(&oldest);
    }
    let entry = map.entry(key.to_string()).or_insert((0, now));
    if now.duration_since(entry.1) >= Duration::from_secs(window_secs) {
        *entry = (0, now);
    }
    entry.0 = entry.0.saturating_add(1);
    entry.0 >= max
}

pub fn login_ok(
    map: &std::sync::Mutex<std::collections::HashMap<String, (u32, std::time::Instant)>>,
    key: &str,
) {
    map.lock().unwrap().remove(key);
}

pub fn secure_cookies() -> bool {
    std::env::var("WEB_URL")
        .map(|u| u.starts_with("https"))
        .unwrap_or(false)
        || std::env::var("WEB_SECURE")
            .map(|v| v == "1")
            .unwrap_or(false)
}

pub fn get_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';').find_map(|p| {
                let p = p.trim();
                p.strip_prefix("session=").map(|v| v.to_string())
            })
        })
}

pub fn session_from_headers(state: &WebState, headers: &HeaderMap) -> Option<Session> {
    if let Some(token) = get_cookie(headers)
        && let Some(s) = try_session(state, &token)
    {
        return Some(s);
    }
    if let Some(auth) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(tok) = auth.strip_prefix("Bearer ")
        && let Some(s) = try_session(state, tok)
    {
        return Some(s);
    }
    None
}

fn try_session(state: &WebState, token: &str) -> Option<Session> {
    let (account_id, role, seller_id, selected_bot_id) = state.db.get_web_session(token)?;
    // Load-side half of the creation boundary (see `create_web_account`): a
    // session whose role is seller-tier but which carries no seller binding has
    // no tenant, and used to resolve to the platform view wherever a handler read
    // `seller_id` as `None`/"unscoped". Such rows can pre-date the fix, so they
    // are refused here as well -- the credential simply does not resolve.
    if !crate::roles::session_is_usable(&role, seller_id) {
        return None;
    }
    Some(Session {
        account_id,
        role,
        seller_id,
        selected_bot_id,
        csrf_token: csrf_token_for_session(token),
    })
}

pub fn pending_cookie(state: &WebState, token: &str) -> String {
    if secure_cookies() {
        format!(
            "mfa_pending={}; Path={}; HttpOnly; SameSite=Strict; Secure; Max-Age=300",
            token,
            cookie_path(state)
        )
    } else {
        format!(
            "mfa_pending={}; Path={}; HttpOnly; SameSite=Strict; Max-Age=300",
            token,
            cookie_path(state)
        )
    }
}

pub fn clear_pending_cookie(state: &WebState) -> String {
    if secure_cookies() {
        format!(
            "mfa_pending=; Path={}; HttpOnly; SameSite=Strict; Secure; Max-Age=0",
            cookie_path(state)
        )
    } else {
        format!(
            "mfa_pending=; Path={}; HttpOnly; SameSite=Strict; Max-Age=0",
            cookie_path(state)
        )
    }
}

pub fn get_pending_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';').find_map(|p| {
                let p = p.trim();
                p.strip_prefix("mfa_pending=").map(|v| v.to_string())
            })
        })
}

pub fn cookie_path(state: &WebState) -> String {
    if state.base_path.is_empty() {
        "/".to_string()
    } else {
        state.base_path.clone()
    }
}
