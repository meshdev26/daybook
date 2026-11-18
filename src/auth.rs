//! Shared web authentication helpers (sessions, login throttling, cookies).
//!
//! Extracted from `web.rs` so the HTTP API (`api.rs`) and the web UI share
//! one implementation. Re-exported from `crate::web` for backwards
//! compatibility (`crate::web::session_from_headers` still works).

use crate::web::{Session, WebState};
use axum::http::HeaderMap;
use std::time::{Duration, Instant};

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
    Some(Session {
        account_id,
        role,
        seller_id,
        selected_bot_id,
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
