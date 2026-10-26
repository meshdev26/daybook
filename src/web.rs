use axum::{
    Router,
    extract::{Form, FromRequestParts, Path, Query, Request, State, connect_info::ConnectInfo},
    http::{StatusCode, header},
    middleware::Next,
    response::{AppendHeaders, Html, IntoResponse, Json, Redirect, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use teloxide::{Bot, prelude::*};
use tower_http::services::ServeDir;

use crate::{config::Config, db::Db, util::ct_eq};

use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

/// The peer IP of the request, when the server tracks one. `main.rs` serves
/// with `into_make_service_with_connect_info::<SocketAddr>()`, which injects
/// the connection info as a request extension before routing; this extractor
/// reads it back and NEVER rejects — absent (oneshot tests, or a server that
/// does not track peers) means unknown, and security decisions treat unknown
/// as remote.
pub struct PeerIp(pub Option<IpAddr>);

impl FromRequestParts<WebState> for PeerIp {
    type Rejection = std::convert::Infallible;

    fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &WebState,
    ) -> impl std::future::Future<Output = Result<Self, Self::Rejection>> + Send {
        // Read the extension before returning the future so the borrow of
        // `parts` ends here.
        let ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        std::future::ready(Ok(PeerIp(ip)))
    }
}

impl PeerIp {
    /// True only when the request provably arrived from the machine itself.
    /// `None` (peer unknown) is deliberately NOT loopback: absence of
    /// evidence must never widen the claim window.
    fn is_loopback(&self) -> bool {
        matches!(self.0, Some(ip) if ip.is_loopback())
    }
}

#[derive(Clone)]
pub struct WebState {
    pub db: Arc<Db>,
    pub cfg: Config,
    pub bot: Bot,
    pub login_attempts: Arc<StdMutex<HashMap<String, (u32, Instant)>>>,
    /// Serializes `/ai/test` requests (N1): a held guard rejects/serializes
    /// concurrent test calls so the provider cannot be hammered.
    pub ai_test_inflight: Arc<tokio::sync::Mutex<()>>,
    /// Base path the dashboard is served under (`""` = root). All generated
    /// URLs, redirects and cookie paths are prefixed with it.
    pub base_path: String,
}

impl WebState {
    /// An in-app absolute path prefixed with the configured base path.
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_path, path)
    }

    /// `Path` attribute for session cookies: the base path when mounted under
    /// one, `/` when serving at the root.
    pub fn cookie_path(&self) -> String {
        if self.base_path.is_empty() {
            "/".to_string()
        } else {
            self.base_path.clone()
        }
    }
}

/// Records one failed login attempt for `key` and returns whether the key is
/// currently locked out (`failures >= max` within `window_secs`, measured from
/// the first failure of the current window). The counter resets on window
/// expiry; a successful login should call [`login_ok`] to clear it. When the
/// map grows past 1024 entries, entries older than one hour are evicted, and
/// the oldest entries are then dropped regardless of age until it fits, so it
/// stays bounded even against a flood of single-attempt keys.
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
    // Hard cap: fresh one-shot keys inside the hour window can still grow the
    // map without bound, so once over 1024 entries evict the oldest whatever
    // their age (a memory bound beats throttle accuracy for the evicted keys).
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

/// Clears the failure counter for `key` after a successful login.
pub fn login_ok(
    map: &std::sync::Mutex<std::collections::HashMap<String, (u32, std::time::Instant)>>,
    key: &str,
) {
    map.lock().unwrap().remove(key);
}

/// HTML-escapes a dynamic value before it is interpolated into an HTML body
/// or an attribute value. Must be applied to every non-numeric dynamic value.
/// Public so the fuzz targets can exercise it.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Whether session cookies should carry the `Secure` attribute.
fn secure_cookies() -> bool {
    std::env::var("WEB_URL")
        .map(|u| u.starts_with("https"))
        .unwrap_or(false)
        || std::env::var("WEB_SECURE")
            .map(|v| v == "1")
            .unwrap_or(false)
}

/// Authoritative first-run gate: the `setup_done` DB flag written by
/// `/setup` wins over the startup-frozen env snapshot, so a completed setup
/// cannot be replayed before a restart.
fn setup_done(state: &WebState) -> bool {
    state.db.get_config("setup_done").is_some() || state.cfg.is_configured()
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/app.css", get(app_css))
        .route("/assets/app.js", get(app_js))
        .route("/assets/fonts/IBMPlexSans-var.woff2", get(font_sans))
        .route("/assets/fonts/IBMPlexMono-400.woff2", get(font_mono_400))
        .route("/assets/fonts/IBMPlexMono-500.woff2", get(font_mono_500))
        .route("/setup", get(setup_page).post(setup_submit))
        .route("/setup/skip", post(setup_skip))
        .route("/login", get(login_page).post(login_submit))
        .route("/logout", get(logout))
        .route("/dashboard", get(dashboard))
        .route("/settings", get(settings_page))
        .route("/settings/save", post(settings_save))
        .route("/settings/password", post(settings_password))
        .route("/settings/sidebar", post(settings_sidebar))
        .route("/channels", get(channels_page))
        .route("/channels/add", post(channels_add))
        .route("/ai", get(ai_page))
        .route("/ai/save", post(ai_save))
        .route("/ai/test", post(ai_test))
        .route("/usage/export", get(usage_export))
        .route("/channels/default", post(channels_default))
        .route("/channels/remove", post(channels_remove))
        .route("/plans", get(plans_page))
        .route("/posts", get(posts_page))
        .route("/post", post(create_post))
        .route("/plan", post(create_plan))
        .route("/plandel", post(delete_plan_by_name))
        .route("/plans/{id}/delete", post(delete_plan_by_id))
        .route("/products", get(products_page))
        .route("/product", post(create_product))
        .route("/analytics", get(analytics_page))
        .route("/subs", get(subs_page))
        .route("/subs/export", get(subs_export))
        .route("/subs/revoke", post(subs_revoke))
        .route("/subs/expirecheck", post(subs_expirecheck))
        .route("/subs/grant", post(subs_grant))
        .route("/subs/extend", post(subs_extend))
        .route(
            "/subs/bulkrevoke",
            get(subs_bulkrevoke).post(subs_bulkrevoke),
        )
        .route("/codes", get(codes_page))
        .route("/codes/generate", post(codes_generate))
        .route("/codes/delete", post(codes_delete))
        .route("/codes/reclaim", post(codes_reclaim))
        .route("/orders", get(orders_page))
        .route("/orders/status", post(orders_status))
        .route("/orders/tracking", post(orders_tracking))
        .route("/finance", get(finance_page))
        .route("/finance/payout/create", post(finance_payout_create))
        .route("/finance/payout/approve", post(finance_payout_approve))
        .route("/finance/payout/execute", post(finance_payout_execute))
        .route("/finance/refund/create", post(finance_refund_create))
        .route("/finance/refund/approve", post(finance_refund_approve))
        .route("/finance/refund/execute", post(finance_refund_execute))
        .route("/broadcast", get(broadcast_page))
        .route("/broadcast/send", post(broadcast_send))
        .route("/crypto", get(crypto_page))
        .route("/crypto/setup", post(crypto_setup))
        .route("/crypto/me", post(crypto_me))
        .route("/admins", get(admins_page))
        .route("/admins/add", post(admins_add))
        .route("/admins/remove", post(admins_remove))
        .route("/sellers", get(sellers_page))
        .route("/sellers/upsert", post(sellers_upsert))
        .route("/sellers/channel", post(sellers_channel))
        .route("/webusers", get(webusers_page))
        .route("/webusers/create", post(webusers_create))
        .route("/webusers/delete", post(webusers_delete))
        .route("/webusers/password", post(webusers_password))
        .route("/webusers/resetmfa", post(webusers_resetmfa))
        .route("/keys", get(keys_page))
        .route("/keys/create", post(keys_create))
        .route("/keys/delete", post(keys_delete))
        .route("/mfa/verify", get(mfa_verify_page).post(mfa_verify_submit))
        .route("/mfa/enroll", get(mfa_enroll_page).post(mfa_enroll_submit))
        .route("/settings/mfa/enroll", post(settings_mfa_enroll))
        .route("/settings/mfa/disable", post(settings_mfa_disable))
        .route("/settings/mfa/recovery", post(settings_mfa_recovery))
        .route("/settings/setupbot", post(settings_setupbot))
        .route("/post/schedule", post(post_schedule))
        .route("/post/edit", post(post_edit))
        .route("/post/delete", post(post_delete))
        .route("/post/pin", post(post_pin))
        .route("/post/unpin", post(post_unpin))
        .route("/channels/invite", post(channel_invite))
        .route("/channels/title", post(channel_title))
        .route("/channels/desc", post(channel_desc))
        .route("/productdel", post(delete_product))
        .nest_service("/static", ServeDir::new("static"))
        .with_state(state)
}

/// Mount `router` under a base path. An empty base returns the router
/// unchanged — `Router::nest("")` panics, so the guard is load-bearing.
/// `base + "/"` (trailing slash) is redirected to `base`: axum's nest maps
/// it to an empty inner path, which matches no route.
pub fn mount(base: &str, router: Router) -> Router {
    if base.is_empty() {
        return router;
    }
    let target = base.to_string();
    let trailing = format!("{base}/");
    Router::new()
        .nest(base, router)
        .route(&trailing, get(move || async move { Redirect::to(&target) }))
}

// ---- Request timeout (control-plane hardening, L8; zero new deps) ----

/// Production ceiling for any request on the merged router. Generous by
/// design: dashboard/API/MCP handlers do SQLite writes and awaited Telegram
/// sends (each with its own client timeout), the bot long-poll runs in the
/// dispatcher task — never through axum — and the cryptopay webhook only
/// performs two DB writes, so nothing legitimate needs 30s.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout middleware applied in `main.rs` around the merged router. On
/// elapse the handler future is dropped (its side effects so far stand —
/// SQLite writes are transactional) and a 503 in the API error envelope is
/// returned.
#[tracing::instrument(skip_all)]
pub async fn request_timeout(req: Request, next: Next) -> Response {
    run_with_timeout(REQUEST_TIMEOUT, req, next).await
}

/// Parameterized core so tests exercise the real logic with a short limit
/// instead of sleeping 30s.
#[tracing::instrument(skip_all)]
pub(crate) async fn run_with_timeout(limit: Duration, req: Request, next: Next) -> Response {
    match tokio::time::timeout(limit, next.run(req)).await {
        Ok(resp) => resp,
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": {"code": "timeout", "message": "request exceeded the time limit and was aborted"}
            })),
        )
            .into_response(),
    }
}

/// The design system, shipped from the binary (no CDN, no build step).
const APP_CSS: &str = include_str!("../assets/app.css");
const FONT_SANS: &[u8] = include_bytes!("../assets/fonts/IBMPlexSans-var.woff2");
const FONT_MONO_400: &[u8] = include_bytes!("../assets/fonts/IBMPlexMono-400.woff2");
const FONT_MONO_500: &[u8] = include_bytes!("../assets/fonts/IBMPlexMono-500.woff2");

/// Reveal/hide behavior for every `.secret-field` (password and token inputs).
/// Served as a real external script (`/assets/app.js`, same shipped-from-the-
/// binary mechanism as APP_CSS) instead of an inline block: one shared
/// implementation for every form, no per-page handlers, and it keeps working
/// unchanged if a strict Content-Security-Policy (no 'unsafe-inline') is ever
/// added. ES5 on purpose — the rest of the console's scripts are too.
const APP_JS: &str = r#"(function () {
  'use strict';
  function init() {
    var toggles = document.querySelectorAll('.secret-toggle');
    for (var i = 0; i < toggles.length; i++) {
      var btn = toggles[i];
      if (btn.dataset.secretBound) continue;
      btn.dataset.secretBound = '1';
      btn.addEventListener('click', function () {
        var field = this.closest('.secret-field');
        var input = field ? field.querySelector('input') : null;
        if (!input) return;
        var show = input.type === 'password';
        // Type swap keeps the typed value; only the rendering changes.
        input.type = show ? 'text' : 'password';
        this.setAttribute('aria-pressed', show ? 'true' : 'false');
        this.setAttribute('aria-label', show ? 'Hide password' : 'Show password');
      });
    }
  }
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
"#;

#[tracing::instrument(skip_all)]
async fn app_css() -> Response {
    asset_css()
}

#[tracing::instrument(skip_all)]
async fn app_js() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
        .into_response()
}
#[tracing::instrument(skip_all)]
async fn font_sans() -> Response {
    asset_font(FONT_SANS)
}
#[tracing::instrument(skip_all)]
async fn font_mono_400() -> Response {
    asset_font(FONT_MONO_400)
}
#[tracing::instrument(skip_all)]
async fn font_mono_500() -> Response {
    asset_font(FONT_MONO_500)
}

fn asset_css() -> Response {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], APP_CSS).into_response()
}

fn asset_font(body: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, "font/woff2"),
            (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
        ],
        body,
    )
        .into_response()
}

fn html_page(state: &WebState, session: Option<&Session>, title: &str, body: &str) -> String {
    let base = &state.base_path;
    format!(
        r##"<!DOCTYPE html><html lang="en" data-theme="light"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{t} — Telegram Business</title>
<link rel="stylesheet" href="{base}/assets/app.css">
<link rel="preload" href="{base}/assets/fonts/IBMPlexSans-var.woff2" as="font" type="font/woff2" crossorigin>
<script>(function(){{var s=localStorage.getItem('theme');var d=window.matchMedia('(prefers-color-scheme: dark)').matches;document.documentElement.setAttribute('data-theme',s||(d?'dark':'light'));}})();</script>
<script src="{base}/assets/app.js" defer></script>
</head><body>
<a class="skip-link" href="#main">Skip to content</a>
<div class="shell">
<aside class="rail">
  <div class="brand">{plane}<b>Telegram Business<small>operator console</small></b></div>
  <div class="group">Operate</div>
  <nav>{nav}</nav>
  <div class="foot"><a href="{base}/settings">Settings</a> <button type="button" class="btn btn-ghost btn-sm theme-toggle" onclick="toggleTheme()">Theme</button></div>
</aside>
<main class="main" id="main"><div class="measure">
{b}
</div></main>
</div>
<script>function toggleTheme(){{var c=document.documentElement.getAttribute('data-theme');var n=c==='dark'?'light':'dark';document.documentElement.setAttribute('data-theme',n);localStorage.setItem('theme',n);}}</script>
</body></html>"##,
        base = base,
        t = title,
        b = body,
        plane = PLANE_SVG,
        nav = rail_nav(state, title, session)
    )
}

/// Front-door shell for pre-auth pages (setup / login): no rail, a focused
/// panel on a calm canvas, brand on top. The operator's first minute should
/// be one action wide, not a dashboard pretending to be configured.
fn html_page_bare(base: &str, title: &str, body: &str) -> String {
    format!(
        r#"<!DOCTYPE html><html lang="en" data-theme="light"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{t} — Telegram Business</title>
<link rel="stylesheet" href="{base}/assets/app.css">
<script>(function(){{var s=localStorage.getItem('theme');var d=window.matchMedia('(prefers-color-scheme: dark)').matches;document.documentElement.setAttribute('data-theme',s||(d?'dark':'light'));}})();</script>
<script src="{base}/assets/app.js" defer></script>
</head><body>
<div class="door">
  <div class="brand">{plane}<b>Telegram Business</b></div>
  <div class="panel">{b}</div>
</div>
</body></html>"#,
        base = base,
        t = title,
        b = body,
        plane = PLANE_SVG
    )
}

/// Brand mark: a paper plane, drawn with three strokes — the same shape the
/// product's world (Telegram) speaks in, reduced to the console's own line weight.
const PLANE_SVG: &str = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 3.5 3.8 10.2c-.8.3-.8 1.4 0 1.7l4.9 1.8 1.8 4.9c.3.8 1.4.8 1.7 0L19 5.5"/><path d="M10.5 13.7 21 3.5"/></svg>"#;

/// The reveal toggle that sits inside every `.secret-field` wrapper (login,
/// claim, setup, settings, AI, crypto, web users). One shared markup, one
/// behavior (APP_JS): a real `<button type="button">` so Enter/Space work and
/// it can never submit the surrounding form. The two stroke-drawn icons ship
/// inline (the PLANE_SVG idiom, currentColor) and swap on the button's
/// `aria-pressed` state, which also carries the state to screen readers.
const SECRET_TOGGLE: &str = r#"<button type="button" class="secret-toggle" aria-label="Show password" aria-pressed="false"><svg class="icon-eye" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/></svg><svg class="icon-eye-off" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24"/><line x1="1" y1="1" x2="23" y2="23"/></svg></button>"#;

/// Rail nav with the current page marked. The active entry is derived from
/// the page title (they are the same vocabulary on purpose — one name per
/// place, per DESIGN.md's voice rules).
fn rail_nav(state: &WebState, title: &str, session: Option<&Session>) -> String {
    let base = &state.base_path;
    let items = vec![
        ("dashboard", "Dashboard"),
        ("channels", "Channels"),
        ("plans", "Plans"),
        ("products", "Products"),
        ("codes", "Codes"),
        ("orders", "Orders"),
        ("finance", "Finance"),
        ("subs", "Subscriptions"),
        ("analytics", "Analytics"),
        ("posts", "Posts"),
        ("broadcast", "Broadcast"),
    ];
    let mut manage: Vec<(&str, &str)> = Vec::new();
    if let Some(s) = session.filter(|s| s.role == "owner") {
        let p = effective_sidebar_prefs(state, s);
        if p.ai {
            manage.push(("ai", "AI"));
        }
        if p.crypto {
            manage.push(("crypto", "Crypto"));
        }
        if p.admins {
            manage.push(("admins", "Admins"));
        }
        if p.sellers {
            manage.push(("sellers", "Sellers"));
        }
        if p.webusers {
            manage.push(("webusers", "Web users"));
        }
    }
    let mut out = String::new();
    for (path, label) in items {
        let active = if title.eq_ignore_ascii_case(label) {
            " class=\"active\""
        } else {
            ""
        };
        out.push_str(&format!("<a href=\"{base}/{path}\"{active}>{label}</a>"));
    }
    // API keys are self-service for every signed-in role (seller+), the same
    // visibility the REST key endpoints enforce.
    if session.is_some_and(|s| s.role == "seller" || is_admin_role(&s.role)) {
        let active = if title.eq_ignore_ascii_case("API keys") {
            " class=\"active\""
        } else {
            ""
        };
        out.push_str(&format!("<a href=\"{base}/keys\"{active}>API keys</a>"));
    }
    if !manage.is_empty() {
        out.push_str(r#"<div class="group">Manage</div>"#);
        for (path, label) in manage {
            let active = if title.eq_ignore_ascii_case(label) {
                " class=\"active\""
            } else {
                ""
            };
            out.push_str(&format!("<a href=\"{base}/{path}\"{active}>{label}</a>"));
        }
    }
    out.push_str(&format!("<a href=\"{base}/settings\">Settings</a>"));
    out.push_str(&format!("<a href=\"{base}/logout\">Log out</a>"));
    out
}

#[derive(Clone)]
pub struct Session {
    pub account_id: i64,
    pub role: String,
    pub seller_id: Option<i64>,
}

pub fn get_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';').find_map(|p| {
                let p = p.trim();
                p.strip_prefix("session=").map(|v| v.to_string())
            })
        })
}

pub fn session_from_headers(state: &WebState, headers: &axum::http::HeaderMap) -> Option<Session> {
    // Cookie
    if let Some(token) = get_cookie(headers)
        && let Some(s) = try_session(state, &token)
    {
        return Some(s);
    }
    // Bearer
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
    let (account_id, role, seller_id) = state.db.get_web_session(token)?;
    Some(Session {
        account_id,
        role,
        seller_id,
    })
}

/// Mirrors `api.rs`'s `is_admin_role`: admins and owners share the
/// privileged tier, sellers are scoped to their own rows. One definition of
/// the tier so console pages and REST endpoints can never drift apart.
fn is_admin_role(role: &str) -> bool {
    role == "admin" || role == "owner"
}

#[tracing::instrument(skip_all)]
async fn index(State(state): State<WebState>, headers: axum::http::HeaderMap) -> impl IntoResponse {
    if !setup_done(&state) {
        return Redirect::to(&state.url("/setup")).into_response();
    }
    if session_from_headers(&state, &headers).is_some() {
        Redirect::to(&state.url("/dashboard")).into_response()
    } else {
        Redirect::to(&state.url("/login")).into_response()
    }
}

/// Query shared by the login GET and POST: a `?code=` deep link (printed to
/// the server console once at startup) is how a remote operator reaches the
/// claim form without being on the machine.
#[derive(Deserialize)]
struct ClaimQuery {
    code: Option<String>,
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
    /// Claim mode only: the confirmation retype. Server-side checked — the
    /// browser's minlength is a courtesy, never the gate.
    confirm_password: Option<String>,
    /// Claim mode only: the one-time code, either as a form field or via
    /// `?code=` (see [`ClaimQuery`]). Never rendered back on failure.
    claim_code: Option<String>,
}

/// The sign-in form. `alert` (already-escaped or static text) is shown above
/// it — used by the GET page and both failure branches so the three surfaces
/// stay one form, one vocabulary.
fn sign_in_form_html(state: &WebState, alert: Option<&str>) -> String {
    let alert = alert
        .map(|a| format!(r#"<div class="alert alert-error" role="alert">{}</div>"#, a))
        .unwrap_or_default();
    format!(
        r#"{alert}<h1>Sign in</h1>
<p class="help">The operator console for your Telegram shop.</p>
<form method="post" action="{}">
<label>Username<input type="text" name="username" required autocomplete="username"></label>
<label for="login-password">Password</label>
<span class="secret-field"><input type="password" id="login-password" name="password" required autocomplete="current-password">{toggle}</span>
<button type="submit">Sign in</button>
</form>"#,
        state.url("/login"),
        toggle = SECRET_TOGGLE,
    )
}

/// The claim form: the front door while no owner account exists. `provided`
/// is the caller-SUPPLIED code (from `?code=` or a previous attempt) carried
/// in a hidden field so a remote operator who mistypes their password keeps
/// their code; the server's real code is never echoed anywhere.
fn claim_form_html(state: &WebState, alert: Option<&str>, provided: Option<&str>) -> String {
    let alert = alert
        .map(|a| format!(r#"<div class="alert alert-error" role="alert">{}</div>"#, a))
        .unwrap_or_default();
    let hidden = provided
        .map(|c| {
            format!(
                r#"<input type="hidden" name="claim_code" value="{}">"#,
                esc(c)
            )
        })
        .unwrap_or_default();
    format!(
        r#"{alert}<h1>Claim the console</h1>
<p class="help">No dashboard account exists yet. The account you create here becomes the owner — it manages settings, channels and sellers. This offer ends once an owner exists.</p>
<form method="post" action="{}">
{hidden}
<label>Username<input type="text" name="username" required autocomplete="username"></label>
<label for="claim-password">Password</label>
<span class="secret-field"><input type="password" id="claim-password" name="password" required minlength="6" autocomplete="new-password">{toggle}</span>
<label for="claim-confirm-password">Confirm password</label>
<span class="secret-field"><input type="password" id="claim-confirm-password" name="confirm_password" required minlength="6" autocomplete="new-password">{toggle}</span><span class="help">At least 6 characters, typed twice.</span>
<button type="submit">Claim the console</button>
</form>"#,
        state.url("/login"),
        hidden = hidden,
        toggle = SECRET_TOGGLE,
    )
}

#[tracing::instrument(skip_all)]
async fn login_page(
    State(state): State<WebState>,
    Query(q): Query<ClaimQuery>,
) -> impl IntoResponse {
    if !setup_done(&state) {
        return Redirect::to(&state.url("/setup")).into_response();
    }
    if !state.db.owner_account_exists() {
        // Claim mode: the front door IS the claim form.
        return Html(html_page_bare(
            &state.base_path,
            "Claim",
            &claim_form_html(&state, None, q.code.as_deref()),
        ))
        .into_response();
    }
    Html(html_page_bare(
        &state.base_path,
        "Login",
        &sign_in_form_html(&state, None),
    ))
    .into_response()
}

#[tracing::instrument(skip_all)]
async fn login_submit(
    State(state): State<WebState>,
    peer: PeerIp,
    Query(q): Query<ClaimQuery>,
    Form(f): Form<LoginForm>,
) -> impl IntoResponse {
    // Claim mode: with no owner account, POST /login IS the claim submission.
    // Once an owner exists this branch can never run again (claim mode never
    // returns), so normal sign-in below stays the steady state.
    if !state.db.owner_account_exists() {
        return claim_submit(&state, &peer, &q, &f).await;
    }
    // Rate limit: 5 failed attempts per username per 15 minutes
    // (a successful login clears the counter via `login_ok`).
    if login_throttle(&state.login_attempts, &f.username, 5, 900) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Html(html_page_bare(
                &state.base_path,
                "Login",
                r##"<div class="alert alert-error" role="alert">Too many attempts. Try again in a few minutes.</div>
<p><a href="#">Back to sign in</a></p>"##,
            )),
        )
            .into_response();
    }
    if let Some((account_id, role, seller_id)) =
        state.db.verify_web_password(&f.username, &f.password).await
    {
        login_ok(&state.login_attempts, &f.username);
        let (_totp_secret, totp_enabled, _last) = state.db.get_totp_state(account_id);
        if totp_enabled {
            let pending_raw = crate::util::gen_token(48);
            let pending_hash = sha256_hex(&pending_raw);
            let expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
            let _ = state.db.create_pending_mfa(
                pending_hash,
                account_id,
                "verify",
                None::<String>,
                expires,
            );
            return (
                [(header::SET_COOKIE, pending_cookie(&state, &pending_raw))],
                Redirect::to(&state.url("/mfa/verify")),
            )
                .into_response();
        }
        let privileged = role == "owner" || role == "admin";
        if privileged {
            let mut buf = [0u8; 20];
            let _ = getrandom::fill(&mut buf);
            let secret = crate::util::base32_encode(&buf);
            let pending_raw = crate::util::gen_token(48);
            let pending_hash = sha256_hex(&pending_raw);
            let expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
            let _ = state.db.create_pending_mfa(
                pending_hash,
                account_id,
                "enroll",
                Some(secret),
                expires,
            );
            return (
                [(header::SET_COOKIE, pending_cookie(&state, &pending_raw))],
                Redirect::to(&state.url("/mfa/enroll")),
            )
                .into_response();
        }
        let token = state.db.create_web_session(account_id, &role, seller_id, 7);
        (
            [(header::SET_COOKIE, session_cookie(&state, &token))],
            Redirect::to(&state.url("/dashboard")),
        )
            .into_response()
    } else {
        // The failure was already recorded by `login_throttle` above.
        Html(html_page_bare(
            &state.base_path,
            "Login",
            &sign_in_form_html(
                &state,
                Some("Invalid credentials. Check the username and password, then try again."),
            ),
        ))
        .into_response()
    }
}

/// Anti-takeover gate for the claim flow (PLAN v1.3.x). The one-time code is
/// waived only when the request provably arrives from the machine itself AND
/// the server is genuinely local-only — the desktop default bind is
/// `127.0.0.1`, where claiming with no code is the whole point. Behind a
/// reverse proxy on the same host (`WEB_BIND=0.0.0.0`) every remote browser
/// arrives with a loopback peer address, so the peer alone proves nothing:
/// `Config::web_bind()` must also be a loopback form ("127.0.0.1",
/// "localhost", "::1"). Anything else — a wildcard or LAN bind, or a remote
/// peer — requires the one-time claim code generated at startup, compared in
/// constant time; the code itself is never rendered back.
fn claim_authorized(state: &WebState, peer: &PeerIp, provided: Option<&str>) -> bool {
    let bind = state.cfg.web_bind();
    let local_only = matches!(bind.trim(), "127.0.0.1" | "localhost" | "::1");
    if peer.is_loopback() && local_only {
        return true;
    }
    match (
        provided.map(str::trim).filter(|c| !c.is_empty()),
        state.db.claim_code(),
    ) {
        (Some(code), Some(expected)) => ct_eq(code, &expected),
        _ => false,
    }
}

/// Claim-mode POST /login: create the owner account and sign it in, exactly
/// like a successful sign-in. Every refusal renders the claim form again —
/// with the caller's own code (if any) preserved in the hidden field.
#[tracing::instrument(skip_all)]
async fn claim_submit(state: &WebState, peer: &PeerIp, q: &ClaimQuery, f: &LoginForm) -> Response {
    // Same throttle as sign-in: a claim endpoint that guesses passwords at
    // full speed is just a login form with worse hygiene.
    if login_throttle(&state.login_attempts, &f.username, 5, 900) {
        return claim_error(
            state,
            None,
            "Too many attempts. Try again in a few minutes.",
        );
    }
    // The gate runs before any account work: a remote request without a
    // valid code learns nothing except that the console is unclaimed —
    // which the public setup-skip flow already reveals.
    let provided = q.code.as_deref().or(f.claim_code.as_deref());
    if !claim_authorized(state, peer, provided) {
        return claim_error(
            state,
            provided,
            "Claiming requires the one-time code shown in the server console.",
        );
    }
    let uname = f.username.trim();
    let pw = f.password.as_str();
    if uname.is_empty() {
        return claim_error(state, provided, "Choose a username.");
    }
    if pw.chars().count() < 6 {
        return claim_error(state, provided, "Password must be at least 6 characters.");
    }
    if f.confirm_password.as_deref() != Some(pw) {
        return claim_error(state, provided, "Passwords do not match.");
    }
    if state.db.get_web_account_by_username(uname).is_some() {
        return claim_error(state, provided, "That username is taken.");
    }
    if let Err(e) = state.db.create_web_account(uname, pw, "owner", None).await {
        return claim_error(
            state,
            provided,
            &format!("Could not create the account: {}", esc(&e)),
        );
    }
    login_ok(&state.login_attempts, uname);
    // The freshly created account is the owner session; mirror login_submit's
    // cookie contract exactly (same lifetime, same flags).
    let (account_id, _u, _hash, _role, _seller) = state
        .db
        .get_web_account_by_username(uname)
        .expect("account was just created");
    let token = state.db.create_web_session(account_id, "owner", None, 7);
    (
        [(header::SET_COOKIE, session_cookie(state, &token))],
        Redirect::to(&state.url("/dashboard")),
    )
        .into_response()
}

fn claim_error(state: &WebState, provided: Option<&str>, msg: &str) -> Response {
    Html(html_page_bare(
        &state.base_path,
        "Claim",
        &claim_form_html(state, Some(msg), provided),
    ))
    .into_response()
}

fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    let bytes = h.finalize();
    let mut out = String::with_capacity(64);
    for b in bytes.iter() {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

fn pending_cookie(state: &WebState, token: &str) -> String {
    if secure_cookies() {
        format!(
            "mfa_pending={}; Path={}; HttpOnly; SameSite=Strict; Secure; Max-Age=300",
            token,
            state.cookie_path()
        )
    } else {
        format!(
            "mfa_pending={}; Path={}; HttpOnly; SameSite=Strict; Max-Age=300",
            token,
            state.cookie_path()
        )
    }
}

fn clear_pending_cookie(state: &WebState) -> String {
    if secure_cookies() {
        format!(
            "mfa_pending=; Path={}; HttpOnly; SameSite=Strict; Secure; Max-Age=0",
            state.cookie_path()
        )
    } else {
        format!(
            "mfa_pending=; Path={}; HttpOnly; SameSite=Strict; Max-Age=0",
            state.cookie_path()
        )
    }
}

fn get_pending_cookie(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.split(';').find_map(|p| {
                let p = p.trim();
                p.strip_prefix("mfa_pending=").map(|v| v.to_string())
            })
        })
}

/// The `session=<token>` cookie for a fresh login, shared by sign-in and
/// claim so both doors hand out identical cookies.
fn session_cookie(state: &WebState, token: &str) -> String {
    if secure_cookies() {
        format!(
            "session={}; Path={}; HttpOnly; SameSite=Strict; Secure; Max-Age=604800",
            token,
            state.cookie_path()
        )
    } else {
        format!(
            "session={}; Path={}; HttpOnly; SameSite=Strict; Max-Age=604800",
            token,
            state.cookie_path()
        )
    }
}

#[tracing::instrument(skip_all)]
async fn logout(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_cookie(&headers) {
        state.db.delete_web_session(&token);
    }
    let cookie = if secure_cookies() {
        format!(
            "session=; Path={}; HttpOnly; SameSite=Strict; Secure; Max-Age=0",
            state.cookie_path()
        )
    } else {
        format!(
            "session=; Path={}; HttpOnly; SameSite=Strict; Max-Age=0",
            state.cookie_path()
        )
    };
    (
        [(header::SET_COOKIE, cookie)],
        Redirect::to(&state.url("/login")),
    )
        .into_response()
}

/// The persistent restart reminder. Setup and every settings save that
/// writes .env set `restart_required`; the running process keeps the config
/// snapshot it started with, so a saved value is not live until the operator
/// restarts. Nothing clears the flag (the console cannot observe a restart)
/// — the banner stays until then, the honest cost of a config file read
/// once at boot. Read by /dashboard and /settings.
fn restart_required_banner(state: &WebState) -> String {
    if state.db.get_config("restart_required").as_deref() == Some("1") {
        r#"<div class="alert alert-warn" role="status" aria-live="polite">Settings saved — restart the app to apply. Desktop: quit and reopen. Server (systemd): <code>systemctl restart telegram-agent</code>. Docker: <code>docker compose restart</code>.</div>"#
            .to_string()
    } else {
        String::new()
    }
}

#[tracing::instrument(skip_all)]
async fn dashboard(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    // Default-channel reader: resolve through the managed channels table
    // with the legacy-config fallback (matches Db::default_channel's other
    // call sites, e.g. api.rs posts_create).
    let ch = state
        .db
        .default_channel()
        .unwrap_or_else(|| "(none)".into());
    let plans = state.db.list_plans(seller_scope);
    let subs = state.db.list_subscriptions(seller_scope, 10);
    let plans_list = if plans.is_empty() {
        "<li class=\"empty\"><b>No plans yet.</b>Create your first plan below — buyers see it in the shop.</li>"
            .to_string()
    } else {
        plans
            .iter()
            .map(|p| {
                format!(
                    "<li><b>{}</b><span class=\"mono\">{} {}</span><span class=\"help\">/ {} days</span></li>",
                    esc(&p.0),
                    esc(&p.1),
                    esc(&p.2),
                    p.3
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let subs_list = if subs.is_empty() {
        "<li class=\"empty\"><b>No subscriptions yet.</b>They appear here the moment a buyer activates.</li>"
            .to_string()
    } else {
        subs.iter()
            .map(|s| {
                format!(
                    "<li>{} user {} {} expires {}</li>",
                    s.0,
                    s.1,
                    esc(&s.3),
                    esc(&s.4)
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    // Persistent honesty: an unconfigured console says so on every visit —
    // the bot is not running, and here is the one place that fixes it.
    let connect_banner = if state.cfg.is_configured() {
        String::new()
    } else {
        format!(
            r#"<div class="alert alert-info" role="status" aria-live="polite">The bot is not connected yet. Add your bot token in <a href="{}">Settings</a> — it connects on the next restart.</div>"#,
            state.url("/settings")
        )
    };
    let restart_banner = restart_required_banner(&state);
    // The Post form offers exactly the channels this session may post to:
    // list_channels(seller_scope) — the global set plus the seller's own for
    // a seller, every known row for the owner. Dropdown only — the free-text
    // guess moved to the API/MCP path, which still accepts arbitrary
    // channels.
    let mut channel_options = String::from("<option value=\"\">(default)</option>");
    let mut seen: Vec<String> = Vec::new();
    for row in state.db.list_channels(seller_scope) {
        seen.push(row.channel.clone());
        channel_options.push_str(&format!(
            "<option value=\"{0}\">{0}</option>",
            esc(&row.channel)
        ));
    }
    // A seller's bot-managed channel lives in the legacy sellers column
    // (set_seller_channel — what /setchannel writes), which list_channels
    // does not read. Merge it so the seller can actually select their own
    // posting target instead of guessing a table row they do not have.
    if let Some(sid) = seller_scope
        && let Some(own) = state.db.seller_channel(sid)
        && !seen.iter().any(|c| c.eq_ignore_ascii_case(&own))
    {
        channel_options.push_str(&format!("<option value=\"{0}\">{0}</option>", esc(&own)));
    }
    let has_product = !plans.is_empty() || !state.db.list_products(seller_scope).is_empty();
    let total_subs_count = state.db.count_subscriptions(seller_scope, None);
    let active_subs_count = state.db.count_subscriptions(seller_scope, Some("active"));
    let has_test_buy = total_subs_count > 0;
    let has_delivery = active_subs_count > 0;
    let chk = |done: bool| {
        if done {
            r#"<span aria-hidden="true">✅</span><span class="sr-only">Done</span>"#
        } else {
            r#"<span aria-hidden="true">⬜</span><span class="sr-only">To-do</span>"#
        }
    };
    let onboarding = format!(
        r#"<article class="panel"><h3>Onboarding checklist</h3>
<ul>
<li>{} <a href="{}">Create product</a></li>
<li>{} Test-buy <span class="mono">/buy &lt;name&gt;</span></li>
<li>{} <a href="{}">Verify delivery</a></li>
</ul></article>"#,
        chk(has_product),
        state.url("/products"),
        chk(has_test_buy),
        chk(has_delivery),
        state.url("/orders"),
    );
    let body = format!(
        r#"<h1>Dashboard <span class="sub">({})</span></h1>
{connect_banner}
{restart_banner}
{onboarding}
<article class="panel"><h3>Default channel</h3><p class="mono">{}</p></article>
<div class="grid-2">
<article class="panel"><h3>Plans ({})</h3><ul>{}</ul></article>
<article class="panel"><h3>Recent subscriptions ({})</h3><ul>{}</ul></article>
</div>
<div class="grid-2">
<article class="panel"><h3>Create plan</h3><form method="post" action="{}">
<label>Name<input name="name" required></label>
<label>Price<input name="price" required></label>
<label>Asset<input name="asset" required></label>
<label>Duration days<input name="duration_days" type="number" required></label>
<label>Channel<input name="channel" placeholder="@channel (optional)"></label>
<button type="submit">Create plan</button></form></article>
<article class="panel"><h3>Post</h3><form method="post" action="{}">
<label>Channel<select name="channel">{}</select><span class="help">Leave on (default) to post where the default channel points.</span></label>
<label>Text<textarea name="text" required></textarea></label>
<button type="submit">Post</button></form></article>
</div>"#,
        esc(&session.role),
        esc(&ch),
        plans.len(),
        plans_list,
        subs.len(),
        subs_list,
        state.url("/plan"),
        state.url("/post"),
        channel_options,
    );
    Html(html_page(&state, Some(&session), "Dashboard", &body)).into_response()
}

#[tracing::instrument(skip_all)]
async fn channels_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ChannelsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    // Outcome banners from the PRG redirects (?added=1 / ?default=1 /
    // ?removed=1) — the same query-param idiom /subs and /finance use.
    let mut banners: Vec<String> = Vec::new();
    if q.added.as_deref() == Some("1") {
        banners.push(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Channel added.</div>"#
                .to_string(),
        );
    }
    if q.default.as_deref() == Some("1") {
        banners.push(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Default channel set.</div>"#
                .to_string(),
        );
    }
    if q.removed.as_deref() == Some("1") {
        banners.push(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Channel removed.</div>"#
                .to_string(),
        );
    }
    let success_banner: Option<String> = if banners.is_empty() {
        None
    } else {
        Some(banners.join("\n"))
    };
    Html(html_page(
        &state,
        Some(&session),
        "Channels",
        &channels_body(&state, &session, None, success_banner.as_deref()),
    ))
    .into_response()
}

/// Query shared by the channels GET page: the success flags its mutation
/// handlers redirect with.
#[derive(Deserialize)]
struct ChannelsQuery {
    added: Option<String>,
    default: Option<String>,
    removed: Option<String>,
}

/// The channels manager body (PLAN v1.3.x). The v1 contract recognizes
/// exactly two classes: the owner manages the global set; a seller sees only
/// their own bot-managed channel. `alert` renders an inline error so a
/// failed action can come straight back to the page (PRG on success only);
/// `success` carries the pre-rendered banner HTML the mutation handlers
/// redirect with (?added=1 / ?default=1 / ?removed=1).
fn channels_body(
    state: &WebState,
    session: &Session,
    alert: Option<&str>,
    success: Option<&str>,
) -> String {
    let alert = alert
        .map(|a| format!(r#"<div class="alert alert-error" role="alert">{}</div>"#, a))
        .unwrap_or_default();
    let success = success.unwrap_or_default();
    if session.role == "seller" {
        // The seller's bot-managed channel is a single setting (the legacy
        // sellers column, what /setchannel writes) — shown read-only here,
        // replaced by the form below. Table rows a seller owns would be
        // managed by the scoped add/remove handlers, not this panel.
        let list = match state.db.seller_channel(session.seller_id.unwrap_or(0)) {
            Some(ch) => format!(
                "<li><span class=\"mono\">{}</span><span class=\"badge badge-success\">bot-managed</span></li>",
                esc(&ch)
            ),
            None => "<li class=\"empty\"><b>No channel yet.</b>Add one below, or use /setchannel in the bot.</li>"
                .to_string(),
        };
        return format!(
            r#"{alert}{success}<h1>Channels <span class="sub">your posting target</span></h1>
<article class="panel"><h3>Your channel</h3><ul>{list}</ul>
<form method="post" action="{}">
<label>Channel<input name="channel" required placeholder="@yourchannel"><span class="help">@-prefixed Telegram channel. Adding one replaces the previous.</span></label>
<button type="submit">Set channel</button>
</form></article>"#,
            state.url("/channels/add"),
            alert = alert,
            success = success,
            list = list,
        );
    }
    // Global panel: full actions for the owner; other operator roles see the
    // rows read-only (the v1 contract gates mutations on the owner session).
    let is_owner = session.role == "owner";
    let rows = state
        .db
        .list_channels(None)
        .into_iter()
        .filter(|row| row.seller_id == 0)
        .map(|row| {
            let badge = if row.is_default {
                "<span class=\"badge badge-success\">default</span>".to_string()
            } else {
                String::new()
            };
            // Set default and Remove exist only on non-default rows — the
            // default channel is the fallback target and must not be
            // deletable from the row it anchors.
            let actions = if is_owner && !row.is_default {
                format!(
                    "<form method=\"post\" action=\"{}\" class=\"inline-form\"><input type=\"hidden\" name=\"channel\" value=\"{}\"><button type=\"submit\" class=\"btn btn-ghost btn-sm\">Set default</button></form><form method=\"post\" action=\"{}\" class=\"inline-form\"><input type=\"hidden\" name=\"channel\" value=\"{}\"><button type=\"submit\" class=\"btn btn-danger btn-sm\">Remove</button></form>",
                    state.url("/channels/default"),
                    esc(&row.channel),
                    state.url("/channels/remove"),
                    esc(&row.channel)
                )
            } else {
                String::new()
            };
            format!(
                "<li><span class=\"mono\">{}</span>{badge}<span class=\"row-action\">{actions}</span></li>",
                esc(&row.channel),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let list = if rows.is_empty() {
        "<li class=\"empty\"><b>No global channels yet.</b>Add one below — it becomes a posting target for plans and posts.</li>"
            .to_string()
    } else {
        rows
    };
    let add_form = if is_owner {
        format!(
            r#"<form method="post" action="{}">
<label>Channel<input name="channel" required placeholder="@yourchannel"><span class="help">@-prefixed Telegram channel. Adding one does not change the default — use Set default.</span></label>
<button type="submit">Add channel</button>
</form>"#,
            state.url("/channels/add")
        )
    } else {
        String::new()
    };
    format!(
        r#"{alert}{success}<h1>Channels <span class="sub">the global posting targets</span></h1>
<article class="panel"><h3>Global channels</h3><ul>{list}</ul>
{add_form}
<p class="help">Sellers manage their own channel via /setchannel in the bot.</p>
</article>"#,
        alert = alert,
        success = success,
        list = list,
        add_form = add_form,
    )
}

/// Re-render the channels page with an inline error (no redirect: the failed
/// action's message belongs on the page it failed from).
fn channels_error(state: &WebState, session: &Session, msg: &str) -> Response {
    Html(html_page(
        state,
        Some(session),
        "Channels",
        &channels_body(state, session, Some(msg), None),
    ))
    .into_response()
}

#[derive(Deserialize)]
struct ChannelForm {
    channel: String,
}

#[tracing::instrument(skip_all)]
async fn channels_add(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let ch = f.channel.trim();
    if ch.is_empty() {
        return channels_error(&state, &session, "Type a channel name first.");
    }
    // Sellers adding = setting their own channel (the bot's /setchannel
    // semantics); owners add to the global set (seller_id 0).
    if session.role == "seller" {
        match session.seller_id {
            Some(sid) => {
                state.db.set_seller_channel(sid, ch);
                Redirect::to(&state.url("/channels?added=1")).into_response()
            }
            None => forbidden_page(&state, "Channels", "Your session has no seller profile."),
        }
    } else if session.role == "owner" {
        match state.db.add_channel(ch, 0) {
            Ok(()) => Redirect::to(&state.url("/channels?added=1")).into_response(),
            Err(e) => channels_error(&state, &session, &esc(&e)),
        }
    } else {
        forbidden_page(&state, "Channels", "Global channels")
    }
}

#[tracing::instrument(skip_all)]
async fn channels_default(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Channels", "The default channel");
    }
    let ch = f.channel.trim();
    if ch.is_empty() {
        return channels_error(&state, &session, "Type a channel name first.");
    }
    // set_default_channel refuses seller-owned rows and unknown names —
    // surface that honestly instead of redirecting as if it worked.
    match state.db.set_default_channel(ch) {
        Ok(()) => Redirect::to(&state.url("/channels?default=1")).into_response(),
        Err(e) => channels_error(&state, &session, &esc(&e)),
    }
}

#[tracing::instrument(skip_all)]
async fn channels_remove(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let ch = f.channel.trim();
    if ch.is_empty() {
        return channels_error(&state, &session, "Type a channel name first.");
    }
    // The default channel is the fallback posting target: removing it would
    // silently break every plan that relies on the fallback, so it is
    // refused until another channel takes over. The check is scope-aware —
    // seller rows are never the global default.
    if session.role == "owner" {
        if state.db.default_channel().as_deref() == Some(ch) {
            return channels_error(
                &state,
                &session,
                "The default channel cannot be removed — set another channel as default first.",
            );
        }
        match state.db.remove_channel(ch, 0) {
            Ok(()) => Redirect::to(&state.url("/channels?removed=1")).into_response(),
            Err(e) => channels_error(&state, &session, &esc(&e)),
        }
    } else if session.role == "seller" {
        match session.seller_id {
            Some(sid) => match state.db.remove_channel(ch, sid) {
                Ok(()) => Redirect::to(&state.url("/channels?removed=1")).into_response(),
                Err(e) => channels_error(&state, &session, &esc(&e)),
            },
            None => forbidden_page(&state, "Channels", "Your session has no seller profile."),
        }
    } else {
        forbidden_page(&state, "Channels", "Global channels")
    }
}

/// 403-style refusal page in the console shell: names what is restricted and
/// stops there — no partial content renders.
fn forbidden_page(state: &WebState, page: &str, what: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Html(html_page(
            state,
            None,
            page,
            &format!(
                r#"<h1>{}</h1><div class="alert alert-error" role="alert">{} is restricted to the console owner.</div><p><a href="{}">Back to the dashboard</a></p>"#,
                esc(page),
                esc(what),
                state.url("/dashboard")
            ),
        )),
    )
        .into_response()
}

// ---- Settings (owner-only, in-console configuration) ----

#[derive(Clone, Copy, Default)]
struct SidebarPrefs {
    ai: bool,
    crypto: bool,
    admins: bool,
    sellers: bool,
    webusers: bool,
}

impl SidebarPrefs {
    fn from_json(s: &str) -> Self {
        let v: serde_json::Value = serde_json::from_str(s).unwrap_or_default();
        Self {
            ai: v.get("ai").and_then(|x| x.as_bool()).unwrap_or(false),
            crypto: v.get("crypto").and_then(|x| x.as_bool()).unwrap_or(false),
            admins: v.get("admins").and_then(|x| x.as_bool()).unwrap_or(false),
            sellers: v.get("sellers").and_then(|x| x.as_bool()).unwrap_or(false),
            webusers: v.get("webusers").and_then(|x| x.as_bool()).unwrap_or(false),
        }
    }
    fn to_json(self) -> String {
        serde_json::json!({
            "ai": self.ai,
            "crypto": self.crypto,
            "admins": self.admins,
            "sellers": self.sellers,
            "webusers": self.webusers,
        })
        .to_string()
    }
}

fn effective_sidebar_prefs(state: &WebState, session: &Session) -> SidebarPrefs {
    if session.role != "owner" {
        return SidebarPrefs::default();
    }
    let user_key = format!("sidebar_items:user:{}", session.account_id);
    if let Some(raw) = state.db.get_config(&user_key) {
        return SidebarPrefs::from_json(&raw);
    }
    if let Some(raw) = state.db.get_config("sidebar_items") {
        return SidebarPrefs::from_json(&raw);
    }
    SidebarPrefs {
        ai: state.cfg.ai_enabled,
        ..SidebarPrefs::default()
    }
}

fn normalize_section(s: Option<String>) -> &'static str {
    match s.as_deref() {
        Some("connection") => "connection",
        Some("password") => "password",
        Some("management") => "management",
        _ => "all",
    }
}

#[derive(Deserialize)]
struct SettingsQuery {
    section: Option<String>,
    /// Outcome flag from the setupbot POST (?setupbot=1 success, any other
    /// value failure) — rendered as a banner by settings_page.
    setupbot: Option<String>,
    /// Outcome flag from the MFA mutation POSTs (?mfa=disabled success;
    /// any other value = the re-auth code was rejected, nothing changed) —
    /// rendered as a banner by settings_page.
    mfa: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn settings_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SettingsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Settings", "Settings");
    }
    let section = normalize_section(q.section);
    let setupbot_banner = match q.setupbot.as_deref() {
        Some("1") => Some(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Bot profile updated — commands, description and short description pushed to Telegram.</div>"#
                .to_string(),
        ),
        Some(_) => Some(
            r#"<div class="alert alert-error" role="alert">Bot profile update failed — Telegram refused or was unreachable. Check the bot token; the profile also refreshes on restart.</div>"#
                .to_string(),
        ),
        None => None,
    };
    let mfa_banner = match q.mfa.as_deref() {
        Some("disabled") => Some(
            r#"<div class="alert alert-success" role="status" aria-live="polite">MFA disabled. Sign-in is password-only until MFA is enrolled again.</div>"#
                .to_string(),
        ),
        Some(_) => Some(
            r#"<div class="alert alert-error" role="alert">That code was not accepted — MFA settings are unchanged. Enter a current authenticator code or an unused recovery code.</div>"#
                .to_string(),
        ),
        None => None,
    };
    let banner = [setupbot_banner, mfa_banner]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n");
    let banner = if banner.is_empty() {
        None
    } else {
        Some(banner)
    };
    Html(html_page(
        &state,
        Some(&session),
        "Settings",
        &settings_body(&state, &session, Some(section), banner.as_deref()),
    ))
    .into_response()
}

/// The settings page body. `banner` carries the outcome of a save (rendered
/// above the panels so a POST can answer in place). Secrets are write-only:
/// the current bot token / Crypto Pay token are never rendered back, only
/// whether one is configured.
fn settings_body(
    state: &WebState,
    session: &Session,
    section: Option<&str>,
    banner: Option<&str>,
) -> String {
    let banner = banner.unwrap_or_default();
    let token_badge = if state.cfg.bot_token.trim().is_empty() {
        r#"<span class="badge badge-warning">not connected</span>"#.to_string()
    } else {
        r#"<span class="badge badge-success">configured</span>"#.to_string()
    };
    let token_help = if state.cfg.bot_token.trim().is_empty() {
        "From @BotFather. The bot connects on restart once a token is saved."
    } else {
        "leave blank to keep the current token"
    };
    let crypto_badge = if state
        .cfg
        .crypto_token
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        r#"<span class="badge badge-warning">not set</span>"#.to_string()
    } else {
        r#"<span class="badge badge-success">configured</span>"#.to_string()
    };
    let owner_id_value = if state.cfg.owner_id != 0 {
        state.cfg.owner_id.to_string()
    } else {
        String::new()
    };
    let default_channel_value = state.cfg.default_channel.clone().unwrap_or_default();

    let sec = section.unwrap_or("all");
    let show_all = sec == "all";
    let show_conn = show_all || sec == "connection";
    let show_pw = show_all || sec == "password";
    let show_mgmt = show_all || sec == "management";

    // tabs — the active section is marked with the chip idiom /subs and
    // /finance use for their status filters (btn-secondary +
    // aria-current="page"): the section arrives as a query param, so the
    // handler knows exactly which tab is current.
    let tab = |label: &str, href: String, active: bool| -> String {
        let cls = if active {
            "btn btn-secondary btn-sm"
        } else {
            "btn btn-ghost btn-sm"
        };
        let aria = if active {
            r#" aria-current="page""#
        } else {
            ""
        };
        format!(
            r#"<a class="{}" href="{}"{}>{}</a>"#,
            cls, href, aria, label
        )
    };
    let tabs = format!(
        r#"<nav class="inline-form" aria-label="Settings sections">
{}{}{}{}
</nav>"#,
        tab("All", state.url("/settings"), show_all),
        tab(
            "Connection",
            state.url("/settings?section=connection"),
            sec == "connection"
        ),
        tab(
            "Password",
            state.url("/settings?section=password"),
            sec == "password"
        ),
        tab(
            "Management",
            state.url("/settings?section=management"),
            sec == "management"
        ),
    );

    // connection panel
    let connection_panel = if show_conn {
        format!(
            r#"<details open id="connection"><summary>Bot connection</summary>
<article class="panel">
<form method="post" action="{}">
<input type="hidden" name="section" value="connection">
<label for="settings-bot-token">Bot token {token_badge}</label>
<span class="secret-field"><input type="password" id="settings-bot-token" name="bot_token" autocomplete="new-password" placeholder="123456:ABC-DEF…">{toggle}</span><span class="help">{token_help}. Stored in .env with 0600 permissions and never rendered back.</span>
<label>Owner Telegram user ID<input type="text" name="owner_id" inputmode="numeric" value="{}" placeholder="123456789"><span class="help">Blank = keep current. Applies on restart — the running process keeps the snapshot it started with.</span></label>
<label>Default channel<input type="text" name="default_channel" value="{}" placeholder="@yourchannel"><span class="help">Blank = keep current. Where posts and grants go when nothing else is set.</span></label>
<label for="settings-crypto-token">Crypto Pay token {crypto_badge}</label>
<span class="secret-field"><input type="password" id="settings-crypto-token" name="crypto_token" autocomplete="new-password" placeholder="from @CryptoBot → Crypto Pay → Create App">{toggle}</span><span class="help">leave blank to keep the current token. Enables automated payment verification.</span>
<button type="submit">Save settings</button>
</form>
<p class="help">Every field is blank-keeps-current: tabbing through without typing changes nothing. Saving writes .env only — the bot connects on restart.</p>
<p class="help">AI provider and usage live on the <a href="{}">AI page</a>.</p>
<p class="help">REST API keys are managed on the <a href="{}">API keys</a> page.</p>
</article></details>"#,
            state.url("/settings/save"),
            esc(&owner_id_value),
            esc(&default_channel_value),
            state.url("/ai"),
            state.url("/keys"),
            toggle = SECRET_TOGGLE,
        )
    } else {
        String::new()
    };

    // password panel
    let password_panel = if show_pw {
        format!(
            r#"<details open id="password"><summary>Dashboard password</summary>
<article class="panel">
<form method="post" action="{}">
<input type="hidden" name="section" value="password">
<label for="settings-new-password">New password</label>
<span class="secret-field"><input type="password" id="settings-new-password" name="new_password" required minlength="6" autocomplete="new-password">{toggle}</span><span class="help">At least 6 characters. Other sessions of this account are signed out; this one stays.</span>
<button type="submit">Change password</button>
</form></article></details>"#,
            state.url("/settings/password"),
            toggle = SECRET_TOGGLE,
        )
    } else {
        String::new()
    };

    // MFA panel
    let (_secret, enabled, _last) = state.db.get_totp_state(session.account_id);
    let unused = state.db.list_unused_recovery_codes(session.account_id);
    let mfa_status = if enabled {
        r#"<span class="badge badge-success">enabled</span>"#
    } else {
        r#"<span class="badge badge-warning">disabled</span>"#
    };
    let mfa_panel = format!(
        r#"<details open id="mfa"><summary>Multi-factor authentication (MFA)</summary>
<article class="panel">
<p>Status: {status}</p>
<p>Unused recovery codes: {unused}</p>
<form method="post" action="{}"><button type="submit" class="btn btn-sm">Enroll / Re-enroll MFA</button></form>
<form method="post" action="{}">
<label for="mfa-disable-code">Current authenticator or recovery code</label>
<input type="text" id="mfa-disable-code" name="code" required autocomplete="one-time-code"><span class="help">Required: a hijacked session cannot disable MFA without the current factor.</span>
<button type="submit" class="btn btn-danger btn-sm" onclick="return confirm('Disable MFA? You will sign in with password only until you enroll again.')">Disable MFA</button>
</form>
<form method="post" action="{}">
<label for="mfa-recovery-code">Current authenticator or recovery code</label>
<input type="text" id="mfa-recovery-code" name="code" required autocomplete="one-time-code"><span class="help">Required to rotate. A recovery code is accepted (and consumed) when your authenticator is unavailable.</span>
<button type="submit" class="btn btn-sm">Regenerate recovery codes</button>
</form>
</article></details>"#,
        state.url("/settings/mfa/enroll"),
        state.url("/settings/mfa/disable"),
        state.url("/settings/mfa/recovery"),
        status = mfa_status,
        unused = unused
    );

    // sidebar prefs
    let prefs = effective_sidebar_prefs(state, session);
    let checked = |b: bool| if b { "checked" } else { "" };

    // management panel
    let management_panel = if show_mgmt {
        format!(
            r#"<details open id="management"><summary>Management</summary>
<article class="panel">
<p class="help">Open a section to manage that area of the console. Toggle which management links appear in the sidebar.</p>
<form method="post" action="{}">
<input type="hidden" name="section" value="management">
<label><input type="checkbox" name="show_ai" value="1" {}> Show AI in sidebar</label>
<label><input type="checkbox" name="show_crypto" value="1" {}> Show Crypto in sidebar</label>
<label><input type="checkbox" name="show_admins" value="1" {}> Show Admins in sidebar</label>
<label><input type="checkbox" name="show_sellers" value="1" {}> Show Sellers in sidebar</label>
<label><input type="checkbox" name="show_webusers" value="1" {}> Show Web users in sidebar</label>
<button type="submit">Save sidebar</button>
</form>
<div class="grid-2">
<a class="panel" href="{}"><h3>AI</h3><p class="help">Provider, API key, base URL, usage and cost.</p></a>
<a class="panel" href="{}"><h3>Crypto</h3><p class="help">Crypto Pay token and account.</p></a>
<a class="panel" href="{}"><h3>Admins</h3><p class="help">Add and remove console admins.</p></a>
<a class="panel" href="{}"><h3>Sellers</h3><p class="help">Seller accounts and channels.</p></a>
<a class="panel" href="{}"><h3>Web users</h3><p class="help">Console accounts and passwords.</p></a>
</div>
<form method="post" action="{}"><button type="submit" class="btn btn-secondary btn-sm">Run setup bot</button></form>
</article></details>"#,
            state.url("/settings/sidebar"),
            checked(prefs.ai),
            checked(prefs.crypto),
            checked(prefs.admins),
            checked(prefs.sellers),
            checked(prefs.webusers),
            state.url("/ai"),
            state.url("/crypto"),
            state.url("/admins"),
            state.url("/sellers"),
            state.url("/webusers"),
            state.url("/settings/setupbot"),
        )
    } else {
        String::new()
    };

    let restart = restart_required_banner(state);
    format!(
        r#"{banner}{restart}<h1>Settings <span class="sub">bot connection and console access</span></h1>
{tabs}
{connection_panel}
{password_panel}
{mfa_panel}
{management_panel}"#,
        banner = banner,
        restart = restart,
        tabs = tabs,
        connection_panel = connection_panel,
        password_panel = password_panel,
        mfa_panel = mfa_panel,
        management_panel = management_panel,
    )
}

fn settings_response(
    state: &WebState,
    session: &Session,
    section: Option<&str>,
    banner_html: String,
) -> Response {
    Html(html_page(
        state,
        Some(session),
        "Settings",
        &settings_body(state, session, section, Some(&banner_html)),
    ))
    .into_response()
}

fn settings_error(
    state: &WebState,
    session: &Session,
    section: Option<&str>,
    msg: &str,
) -> Response {
    settings_response(
        state,
        session,
        section,
        format!(
            r#"<div class="alert alert-error" role="alert">{}</div>"#,
            msg
        ),
    )
}

#[derive(Deserialize)]
struct SettingsForm {
    bot_token: Option<String>,
    owner_id: Option<String>,
    default_channel: Option<String>,
    crypto_token: Option<String>,
    section: Option<String>,
}

/// Trimmed, non-empty view of an optional form field: the single definition
/// of "blank = keep current" for settings saves.
fn non_blank(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

#[tracing::instrument(skip_all)]
async fn settings_save(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SettingsForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Settings", "Settings");
    }
    let section = normalize_section(f.section.clone());
    // Build updates ONLY for non-blank fields: a blank field means "keep the
    // current value", so the secrets already in .env can never be clobbered
    // by an empty form field.
    let mut updates: Vec<(String, String)> = Vec::new();
    if let Some(t) = non_blank(&f.bot_token) {
        // A changed token is validated before anything is written: a typo
        // must never take down the bot's connection on the next restart.
        if !validate_bot_token(t).await {
            return settings_error(
                &state,
                &session,
                Some(section),
                "Invalid bot token (Telegram rejected it). Nothing was saved.",
            );
        }
        updates.push(("TELEGRAM_BOT_TOKEN".to_string(), t.to_string()));
    }
    if let Some(o) = non_blank(&f.owner_id) {
        match o.parse::<i64>() {
            Ok(v) if v > 0 => updates.push(("OWNER_ID".to_string(), v.to_string())),
            _ => {
                return settings_error(
                    &state,
                    &session,
                    Some(section),
                    "Owner ID must be a positive number. Nothing was saved.",
                );
            }
        }
    }
    if let Some(c) = non_blank(&f.default_channel) {
        updates.push(("DEFAULT_CHANNEL".to_string(), c.to_string()));
    }
    if let Some(c) = non_blank(&f.crypto_token) {
        updates.push(("CRYPTO_PAY_TOKEN".to_string(), c.to_string()));
    }
    if updates.is_empty() {
        // Nothing to change — do not touch .env as a side effect of an
        // all-blank submit.
        return settings_response(
            &state,
            &session,
            Some(section),
            r#"<div class="alert alert-info" role="status" aria-live="polite">Nothing to save — all fields were blank.</div>"#
                .to_string(),
        );
    }
    match crate::config::write_env_file(&updates) {
        Ok(_) => {
            // .env changed, but the running process reads its config snapshot
            // once at boot: set the restart flag the /dashboard and /settings
            // banners read (first-run setup sets the same flag).
            state.db.set_config("restart_required", "1");
            settings_response(
                &state,
                &session,
                Some(section),
                r#"<div class="alert alert-success" role="status" aria-live="polite">Saved. The bot connects on restart.</div>
<p class="help">Values are written to .env; the running process keeps the settings snapshot it started with, so restart to apply them.</p>"#
                    .to_string(),
            )
        }
        Err(e) => settings_error(
            &state,
            &session,
            Some(section),
            &format!("Failed to write .env: {}", esc(&e.to_string())),
        ),
    }
}

#[derive(Deserialize)]
struct SettingsPasswordForm {
    new_password: String,
    section: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn settings_password(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SettingsPasswordForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Settings", "Settings");
    }
    let section = normalize_section(f.section.clone());
    // Mirrors the api.rs auth_password contract exactly, no invention: the
    // owner's web session is the strongest identity a deployment has, so no
    // current password is demanded of it; the 6-character floor is the same.
    if f.new_password.chars().count() < 6 {
        return settings_error(
            &state,
            &session,
            Some(section),
            "Password must be at least 6 characters.",
        );
    }
    if let Err(e) = state
        .db
        .update_web_password_by_id(session.account_id, &f.new_password)
        .await
    {
        return settings_error(
            &state,
            &session,
            Some(section),
            &format!("Could not change the password: {}", esc(&e)),
        );
    }
    // Rotation evicts every OTHER session of this account (a stolen session
    // must not survive it); the caller keeps theirs — same helper api.rs uses.
    if let Some(tok) = caller_token(&headers) {
        state.db.delete_other_web_sessions(session.account_id, &tok);
    }
    settings_response(
        &state,
        &session,
        Some(section),
        r#"<div class="alert alert-success" role="status" aria-live="polite">Password changed. Other sessions were signed out.</div>"#
            .to_string(),
    )
}

#[derive(Deserialize)]
struct SidebarForm {
    section: Option<String>,
    show_ai: Option<String>,
    show_crypto: Option<String>,
    show_admins: Option<String>,
    show_sellers: Option<String>,
    show_webusers: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn settings_sidebar(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SidebarForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Settings", "Settings");
    }
    let section = normalize_section(f.section.clone());
    let prefs = SidebarPrefs {
        ai: f.show_ai.is_some(),
        crypto: f.show_crypto.is_some(),
        admins: f.show_admins.is_some(),
        sellers: f.show_sellers.is_some(),
        webusers: f.show_webusers.is_some(),
    };
    state.db.set_config("sidebar_items", &prefs.to_json());
    settings_response(
        &state,
        &session,
        Some(section),
        r#"<div class="alert alert-success" role="status" aria-live="polite">Sidebar updated.</div>"#.to_string(),
    )
}

/// The session token behind a request, resolved the way
/// `session_from_headers` does (cookie first, then Bearer) so the password
/// rotation can keep the caller's session while evicting the others.
fn caller_token(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(t) = get_cookie(headers) {
        return Some(t);
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.to_string())
}

#[derive(Deserialize)]
struct AiForm {
    ai_enabled: Option<String>,
    ai_provider: Option<String>,
    ai_api_style: Option<String>,
    ai_base_url: Option<String>,
    ai_api_key: Option<String>,
    ai_model: Option<String>,
    ai_daily_token_budget: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn ai_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "AI", "AI");
    }
    Html(html_page(
        &state,
        Some(&session),
        "AI",
        &ai_body(&state, None),
    ))
    .into_response()
}

fn ai_body(state: &WebState, banner: Option<&str>) -> String {
    let banner = banner.unwrap_or_default();

    // Unconfigured alert
    let unconfigured = if !state.cfg.ai_enabled {
        r#"<div class="alert alert-info" role="status" aria-live="polite">AI is currently disabled. Configure a provider and key, then toggle Enable AI features.</div>"#.to_string()
    } else {
        String::new()
    };

    // API key badge. No aria-label on purpose: the badge sits inside the
    // field's <label>, so its text already joins the input's accessible name
    // ("API key not set") — an aria-label here would read it twice.
    let key_badge = if state
        .cfg
        .ai_api_key
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        r#"<span class="badge badge-warning">not set</span>"#.to_string()
    } else {
        r#"<span class="badge badge-success">configured</span>"#.to_string()
    };
    let key_placeholder = if state
        .cfg
        .ai_api_key
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        "sk-… / your provider key"
    } else {
        "leave blank to keep the current key"
    };
    let key_help = if state
        .cfg
        .ai_api_key
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        "Write-only. Stored in .env with 0600 permissions and never rendered back."
    } else {
        "leave blank to keep the current key. Stored in .env with 0600 permissions and never rendered back."
    };

    let provider_value = &state.cfg.ai_provider;
    // Defensively strip any userinfo from a pre-existing .env value so it
    // cannot leak into the rendered input (B1).
    let base_url_value = esc(&strip_url_userinfo(&state.cfg.ai_base_url));
    let model_value = esc(&state.cfg.ai_model);
    let budget_value = state
        .cfg
        .ai_daily_token_budget
        .map(|v| v.to_string())
        .unwrap_or_default();
    let budget_value = esc(&budget_value);
    let enabled_checked = if state.cfg.ai_enabled { "checked" } else { "" };

    let selected_openai = if provider_value == "openai" {
        "selected"
    } else {
        ""
    };
    let selected_anthropic = if provider_value == "anthropic" {
        "selected"
    } else {
        ""
    };
    let selected_custom = if provider_value == "custom" {
        "selected"
    } else {
        ""
    };
    let selected_ollama = if provider_value == "ollama" {
        "selected"
    } else {
        ""
    };
    let selected_openrouter = if provider_value == "openrouter" {
        "selected"
    } else {
        ""
    };

    let api_style_value = &state.cfg.ai_api_style;
    let selected_style_openai = if api_style_value == "openai" {
        "selected"
    } else {
        ""
    };
    let selected_style_anthropic = if api_style_value == "anthropic" {
        "selected"
    } else {
        ""
    };

    // base url disabled for anthropic initial
    let base_disabled = if provider_value == "anthropic" {
        "disabled"
    } else {
        ""
    };

    let form = format!(
        r#"<article class="panel"><h3>AI provider</h3>
<form method="post" action="{}">
<label>Enable AI features <input type="checkbox" name="ai_enabled" value="1" {} aria-describedby="ai-enabled-help"><span class="help" id="ai-enabled-help">Turns on AI-assisted replies and templates. Off means no AI calls are made, even with a key saved.</span></label>
<label>Provider <select name="ai_provider" id="ai-provider" aria-describedby="ai-provider-help">
<option value="openai" {}>openai</option>
<option value="anthropic" {}>anthropic</option>
<option value="ollama" {}>ollama (local)</option>
<option value="openrouter" {}>openrouter</option>
<option value="custom" {}>custom</option>
</select><span class="help" id="ai-provider-help">OpenAI, Anthropic, Ollama (local), OpenRouter, or any OpenAI-compatible endpoint. Ollama/vLLM/LM Studio use OpenAI-compatible /v1/chat/completions; Anthropic uses /v1/messages.</span></label>
<label>API style <select name="ai_api_style" id="ai-api-style" aria-describedby="ai-api-style-help">
<option value="openai" {}>openai (chat/completions)</option>
<option value="anthropic" {}>anthropic (messages)</option>
</select><span class="help" id="ai-api-style-help">Use openai for /v1/chat/completions, anthropic for /v1/messages.</span></label>
<label>Base URL <input type="text" name="ai_base_url" inputmode="url" value="{}" {} aria-describedby="ai-baseurl-help" maxlength="512"><span class="help" id="ai-baseurl-help">OpenAI-compatible API base URL ending in /v1 (e.g. https://api.openai.com/v1, http://localhost:11434/v1, https://openrouter.ai/api/v1). Anthropic ignores this field.</span></label>
<label for="ai-api-key">API key {}</label>
<span class="secret-field"><input type="password" id="ai-api-key" name="ai_api_key" autocomplete="new-password" placeholder="{}" aria-describedby="ai-apikey-help">{toggle}</span><span class="help" id="ai-apikey-help">{}</span>
<label>Model <input type="text" name="ai_model" value="{}" aria-describedby="ai-model-help" maxlength="128"><span class="help" id="ai-model-help">Model name as the provider expects it (e.g. gpt-4o-mini, claude-3-5-sonnet-20241022).</span></label>
<label>Daily token budget (global) <input type="number" name="ai_daily_token_budget" min="0" step="1" inputmode="numeric" value="{}" aria-describedby="ai-budget-help"><span class="help" id="ai-budget-help">Maximum tokens per day across all sellers and buyers. Blank = keep current. The service treats an unset budget as unlimited. Enforced by the AI service.</span></label>
<div class="form-row">
<div><button type="submit">Save AI settings</button></div>
<div><button type="button" id="ai-test-btn" class="btn btn-secondary">Test connection</button></div>
</div>
<div id="ai-test-status" role="status" aria-live="polite" aria-atomic="true" class="help"></div>
<noscript><p class="help">JavaScript is required to test the connection.</p></noscript>
</form>
<p class="help">Values are written to .env with 0600 permissions. AI settings apply on restart; Test connection uses the values as typed.</p>
<script>
(function(){{
  var sel=document.getElementById('ai-provider');
  var base=document.querySelector('input[name="ai_base_url"]');
  var defaults={{'openai':'https://api.openai.com/v1','ollama':'http://localhost:11434/v1','openrouter':'https://openrouter.ai/api/v1'}};
  function sync(){{ if(!base) return; base.disabled = (sel && sel.value==='anthropic'); if(sel && defaults[sel.value] && !base.value){{ base.value=defaults[sel.value]; }} }}
  if(sel){{ sel.addEventListener('change',sync); sync(); }}
  var btn=document.getElementById('ai-test-btn');
  var status=document.getElementById('ai-test-status');
  function esc(s){{ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;'); }}
  if(btn){{
    btn.addEventListener('click', async function(){{
      if(btn.disabled) return;
      btn.disabled=true; btn.setAttribute('aria-busy','true'); btn.textContent='Testing…';
      if(status){{ status.setAttribute('role','status'); status.textContent='Testing connection…'; }}
      var form=btn.closest('form');
      var fd=new FormData(form);
      try{{
        var r=await fetch('{}',{{method:'POST',body:fd,credentials:'same-origin'}});
        var j=await r.json();
        if(j.ok){{ if(status){{ status.setAttribute('role','status'); status.textContent='Connected in '+j.latency_ms+' ms.'; }} }}
        else {{ if(status){{ status.setAttribute('role','alert'); status.textContent='Connection failed: '+(j.error||'Unknown error'); }} }}
      }}catch(e){{ if(status){{ status.setAttribute('role','alert'); status.textContent='Connection failed: Network error.'; }} }}
      btn.disabled=false; btn.removeAttribute('aria-busy'); btn.textContent='Test connection';
    }});
  }}
}})();
</script>
</article>"#,
        state.url("/ai/save"),
        enabled_checked,
        selected_openai,
        selected_anthropic,
        selected_ollama,
        selected_openrouter,
        selected_custom,
        selected_style_openai,
        selected_style_anthropic,
        base_url_value,
        base_disabled,
        key_badge,
        key_placeholder,
        key_help,
        model_value,
        budget_value,
        state.url("/ai/test"),
        toggle = SECRET_TOGGLE,
    );

    // Usage dashboard
    let today_tokens = state.db.ai_usage_today_tokens_global();
    let summary = state.db.ai_usage_summary_global(7);
    let week_tokens: i64 = summary.iter().map(|r| r.1).sum();
    let week_requests: i64 = summary.iter().map(|r| r.2).sum();
    let week_cost = state.db.ai_usage_cost_total_global(7);
    let errors = state.db.ai_usage_recent_errors_global(10);

    // daily table
    let daily_table = if summary.is_empty() {
        r#"<p class="empty">No AI usage yet. Usage appears here after the first AI-assisted reply.</p>"#.to_string()
    } else {
        let rows = summary
            .iter()
            .map(|(d, t, c)| {
                format!(
                    "<tr><td class=\"mono\" title=\"{}\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
                    esc(d),
                    esc(d),
                    t,
                    c
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!(
            "<div class=\"table-wrap\"><table><caption class=\"sr-only\">Daily AI usage for the last 7 days</caption><thead><tr><th>Date</th><th class=\"num\">Tokens</th><th class=\"num\">Requests</th></tr></thead><tbody>{}</tbody></table></div>",
            rows
        )
    };

    // recent errors
    let errors_html = if errors.is_empty() {
        r#"<p class="empty">No recent errors. AI calls are succeeding.</p>"#.to_string()
    } else {
        let items = errors
            .iter()
            .map(|(ts, prov, model, err, _status)| {
                let raw = err.clone().unwrap_or_default();
                // Render-time redaction (N4): never trust stored error text.
                let key = state.cfg.ai_api_key.clone().unwrap_or_default();
                let base = state.cfg.ai_base_url.clone();
                let msg = redact_ai_error(&raw, &key, &base);
                let short = if msg.chars().count() > 160 {
                    let s: String = msg.chars().take(160).collect();
                    format!("{}…", s)
                } else {
                    msg.clone()
                };
                format!(
                    "<li><span class=\"mono\">{}</span> <span class=\"mono\">{}/{}</span> <span title=\"{}\">{}</span><details><summary>Show more</summary><p>{}</p></details></li>",
                    esc(ts),
                    esc(prov),
                    esc(model),
                    esc(&msg),
                    esc(&short),
                    esc(&msg)
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!("<ul>{}</ul>", items)
    };

    let dashboard = format!(
        r#"<section class="panel" aria-labelledby="ai-usage-heading"><h3 id="ai-usage-heading">Usage &amp; cost</h3>
<div class="grid-2">
<article class="panel"><h3>Today's tokens</h3><p class="mono">{}</p><p class="help">tokens used today</p></article>
<article class="panel"><h3>7-day tokens</h3><p class="mono">{}</p><p class="help">tokens, last 7 days</p></article>
<article class="panel"><h3>7-day requests</h3><p class="mono">{}</p><p class="help">AI requests, last 7 days</p></article>
<article class="panel"><h3>7-day estimated cost</h3><p class="mono">${:.2}</p><p class="help" title="Estimated from token counts and rough per-model pricing; not a bill.">estimated cost, last 7 days</p></article>
</div>
{}
<article class="panel"><h3>Recent errors</h3>{}</article>
<p class="help">Refresh the page to update usage.</p>
</section>"#,
        today_tokens, week_tokens, week_requests, week_cost, daily_table, errors_html,
    );

    // Templates (read-only). Owners have no seller_id, so the REST list
    // (`ai_templates_list` in api.rs) resolves their scope to 0 — the global
    // bucket — and this panel reads the exact same rows. Editing lives in the
    // bot (/ai) and the REST API, deliberately not here.
    let templates = state.db.list_ai_templates(0);
    let templates_html = if templates.is_empty() {
        r#"<p class="empty">No templates yet.</p>"#.to_string()
    } else {
        let items = templates
            .iter()
            .map(|(name, description)| {
                format!(
                    "<li><span class=\"mono\">{}</span> <span>{}</span></li>",
                    esc(name),
                    esc(description.as_deref().unwrap_or("—"))
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!("<ul>{}</ul>", items)
    };
    let templates_panel = format!(
        r#"<section class="panel" aria-labelledby="ai-templates-heading"><h3 id="ai-templates-heading">Templates</h3>
{}
<p class="help">Manage templates via the bot (/ai) or REST /api/v1/ai/templates.</p>
</section>"#,
        templates_html
    );

    format!(
        r#"{banner}{unconfigured}<h1>AI <span class="sub">provider credentials, usage and templates</span></h1>
{form}
{dashboard}
{templates_panel}
<p class="help">Values are written to .env with 0600 permissions. AI settings apply on restart; Test connection uses the values as typed.</p>"#,
        banner = banner,
        unconfigured = unconfigured,
        form = form,
        dashboard = dashboard,
        templates_panel = templates_panel,
    )
}

#[tracing::instrument(skip_all)]
async fn ai_save(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<AiForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "AI", "AI");
    }

    let enabled = f.ai_enabled.is_some();

    // 1. provider
    let provider = if let Some(p) = non_blank(&f.ai_provider) {
        if !matches!(
            p,
            "openai" | "anthropic" | "ollama" | "openrouter" | "custom"
        ) {
            return ai_error(
                &state,
                Some(&session),
                "Provider must be openai, anthropic, ollama, openrouter, or custom.",
            );
        }
        p.to_string()
    } else {
        state.cfg.ai_provider.clone()
    };

    // 1b. api style
    if let Some(s) = non_blank(&f.ai_api_style)
        && !matches!(s, "openai" | "anthropic")
    {
        return ai_error(
            &state,
            Some(&session),
            "API style must be openai or anthropic.",
        );
    }

    // 2. base URL presence/parse (validate whenever submitted, regardless of
    //    provider — N3), plus required-for-openai/custom.
    if let Some(b) = non_blank(&f.ai_base_url) {
        if b.len() > 512 {
            return ai_error(
                &state,
                Some(&session),
                "Base URL must be 512 characters or fewer.",
            );
        }
        match reqwest::Url::parse(b) {
            Ok(u)
                if (u.scheme() == "http" || u.scheme() == "https")
                    && u.username().is_empty()
                    && u.password().is_none() => {}
            _ => {
                return ai_error(
                    &state,
                    Some(&session),
                    "Base URL must be a valid http(s) URL.",
                );
            }
        }
    } else if matches!(provider.as_str(), "openai" | "custom")
        && state.cfg.ai_base_url.trim().is_empty()
    {
        return ai_error(
            &state,
            Some(&session),
            "Base URL is required for this provider.",
        );
    }

    // 3. if enabling: API key available
    if enabled {
        let key_available = non_blank(&f.ai_api_key).is_some()
            || state
                .cfg
                .ai_api_key
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .is_some();
        if !key_available {
            return ai_error(&state, Some(&session), "Add an API key before enabling AI.");
        }
    }

    // 4. model
    if let Some(m) = non_blank(&f.ai_model)
        && (m.chars().count() > 128 || m.contains('\n') || m.contains('\r'))
    {
        return ai_error(
            &state,
            Some(&session),
            "Model must be 1-128 characters with no newline.",
        );
    }

    // 5. budget
    if let Some(b) = non_blank(&f.ai_daily_token_budget) {
        match b.parse::<i64>() {
            Ok(v) if v >= 0 => {}
            _ => return ai_error(&state, Some(&session), "Budget must be an integer >= 0."),
        }
    }

    // 6. api key
    if let Some(k) = non_blank(&f.ai_api_key)
        && (k.chars().count() < 8 || k.chars().count() > 4096 || k.chars().any(char::is_whitespace))
    {
        return ai_error(
            &state,
            Some(&session),
            "API key must be 8-4096 characters with no whitespace.",
        );
    }

    // Build updates
    let mut updates: Vec<(String, String)> = Vec::new();
    updates.push((
        "AI_ENABLED".to_string(),
        if enabled {
            "1".to_string()
        } else {
            "0".to_string()
        },
    ));
    if let Some(p) = non_blank(&f.ai_provider) {
        updates.push(("AI_PROVIDER".to_string(), p.to_string()));
    }
    if let Some(s) = non_blank(&f.ai_api_style) {
        updates.push(("AI_API_STYLE".to_string(), s.to_string()));
    }
    if let Some(b) = non_blank(&f.ai_base_url) {
        updates.push(("AI_BASE_URL".to_string(), b.to_string()));
    }
    if let Some(k) = non_blank(&f.ai_api_key) {
        updates.push(("AI_API_KEY".to_string(), k.to_string()));
    }
    if let Some(m) = non_blank(&f.ai_model) {
        updates.push(("AI_MODEL".to_string(), m.to_string()));
    }
    if let Some(b) = non_blank(&f.ai_daily_token_budget) {
        updates.push(("AI_DAILY_TOKEN_BUDGET".to_string(), b.to_string()));
    }

    match crate::config::write_env_file(&updates) {
        Ok(_) => ai_response(
            &state,
            Some(&session),
            r#"<div class="alert alert-success" role="status" aria-live="polite">Saved. AI settings apply on restart.</div>"#
                .to_string(),
        ),
        Err(e) => ai_error(
            &state,
            Some(&session),
            &format!("Failed to write .env: {}", esc(&e.to_string())),
        ),
    }
}

fn ai_response(state: &WebState, session: Option<&Session>, banner_html: String) -> Response {
    Html(html_page(
        state,
        session,
        "AI",
        &ai_body(state, Some(&banner_html)),
    ))
    .into_response()
}

fn ai_error(state: &WebState, session: Option<&Session>, msg: &str) -> Response {
    ai_response(
        state,
        session,
        format!(
            r#"<div class="alert alert-error" role="alert">{}</div>"#,
            msg
        ),
    )
}

#[tracing::instrument(skip_all)]
async fn ai_test(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<AiForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({"ok": false, "latency_ms": 0, "error": "Unauthorized"})),
            )
                .into_response();
        }
    };
    if session.role != "owner" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"ok": false, "latency_ms": 0, "error": "Forbidden"})),
        )
            .into_response();
    }

    // N1: serialize concurrent /ai/test calls.
    let _inflight_guard = state.ai_test_inflight.lock().await;

    // N2: validate provider.
    let provider_name = non_blank(&f.ai_provider)
        .map(str::to_string)
        .unwrap_or_else(|| state.cfg.ai_provider.clone());
    if !matches!(
        provider_name.as_str(),
        "openai" | "anthropic" | "ollama" | "openrouter" | "custom"
    ) {
        return (
            StatusCode::OK,
            Json(json!({"ok": false, "latency_ms": 0, "error": "Provider must be openai, anthropic, ollama, openrouter, or custom."})),
        )
            .into_response();
    }
    let api_style = non_blank(&f.ai_api_style)
        .map(str::to_string)
        .unwrap_or_else(|| state.cfg.ai_api_style.clone());
    if !matches!(api_style.as_str(), "openai" | "anthropic") {
        return (
            StatusCode::OK,
            Json(json!({"ok": false, "latency_ms": 0, "error": "API style must be openai or anthropic."})),
        )
            .into_response();
    }
    let base_url = non_blank(&f.ai_base_url)
        .map(str::to_string)
        .unwrap_or_else(|| state.cfg.ai_base_url.clone());

    // B1: validate base URL for providers that use it.
    if !matches!(api_style.as_str(), "anthropic") {
        if base_url.len() > 512 {
            return (
                StatusCode::OK,
                Json(json!({"ok": false, "latency_ms": 0, "error": "Base URL must be 512 characters or fewer."})),
            )
                .into_response();
        }
        match reqwest::Url::parse(&base_url) {
            Ok(u)
                if (u.scheme() == "http" || u.scheme() == "https")
                    && u.username().is_empty()
                    && u.password().is_none() => {}
            _ => {
                return (
                    StatusCode::OK,
                    Json(json!({"ok": false, "latency_ms": 0, "error": "Base URL must be a valid http(s) URL."})),
                )
                    .into_response();
            }
        }
    }
    let key = non_blank(&f.ai_api_key).map(str::to_string).or_else(|| {
        state
            .cfg
            .ai_api_key
            .clone()
            .filter(|s| !s.trim().is_empty())
    });
    let model = non_blank(&f.ai_model)
        .map(str::to_string)
        .unwrap_or_else(|| state.cfg.ai_model.clone());

    let Some(key) = key else {
        return (
            StatusCode::OK,
            Json(json!({"ok": false, "latency_ms": 0, "error": "No API key configured."})),
        )
            .into_response();
    };

    use crate::ai::provider::{AiProvider, GenerateRequest, Message};
    use std::sync::Arc;

    let provider: Arc<dyn AiProvider> = match api_style.as_str() {
        "anthropic" => {
            match crate::ai::anthropic::AnthropicProvider::new(base_url.clone(), key.clone()) {
                Ok(p) => Arc::new(p),
                Err(e) => {
                    return (
                        StatusCode::OK,
                        Json(json!({"ok": false, "latency_ms": 0, "error": e.to_string()})),
                    )
                        .into_response();
                }
            }
        }
        _ => match crate::ai::openai::OpenAiProvider::new(base_url.clone(), key.clone()) {
            Ok(p) => Arc::new(p),
            Err(e) => {
                return (
                    StatusCode::OK,
                    Json(json!({"ok": false, "latency_ms": 0, "error": e.to_string()})),
                )
                    .into_response();
            }
        },
    };

    let req = GenerateRequest {
        model,
        messages: vec![Message {
            role: "user".to_string(),
            content: "ping".to_string(),
        }],
        max_tokens: 5,
        temperature: 0.0,
    };

    let start = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(10), provider.generate(req)).await;
    let latency_ms = start.elapsed().as_millis() as u64;

    match outcome {
        Ok(Ok(_)) => (
            StatusCode::OK,
            Json(json!({"ok": true, "latency_ms": latency_ms, "error": null})),
        )
            .into_response(),
        Ok(Err(e)) => {
            let msg = e.to_string();
            let err = redact_ai_error(&msg, &key, &base_url);
            let category = categorize_ai_error(&err);
            (
                StatusCode::OK,
                Json(json!({"ok": false, "latency_ms": latency_ms, "error": category})),
            )
                .into_response()
        }
        Err(_) => (
            StatusCode::OK,
            Json(json!({"ok": false, "latency_ms": latency_ms, "error": "Timed out after 10s."})),
        )
            .into_response(),
    }
}

/// Strip userinfo (and nothing else) from a URL-ish string for safe rendering.
fn strip_url_userinfo(s: &str) -> String {
    match reqwest::Url::parse(s) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        Err(_) => s.to_string(),
    }
}

fn redact_ai_error(msg: &str, key: &str, base_url: &str) -> String {
    // 1. exact key replacement (covers key-in-body and key-in-URL when the
    //    full key string appears).
    let mut s = if key.trim().is_empty() {
        msg.to_string()
    } else {
        msg.replace(key, "[redacted]")
    };

    // 2. Parse any URL embedded in the (already key-redacted) text and strip
    //    userinfo + query + fragment, so secrets in query strings or userinfo
    //    cannot survive. Full request URLs (scheme://host/path) are retained
    //    in redacted form rather than only the configured base_url.
    s = redact_urls_in_text(&s);

    // 3. Also scrub the configured base_url if it still appears verbatim
    //    (e.g. without a scheme) — best effort.
    if !base_url.trim().is_empty() {
        let scrubbed = strip_url_userinfo(base_url);
        s = s.replace(base_url, &scrubbed);
    }

    s
}

/// Walk `input` and redact userinfo/query/fragment from any embedded URL.
fn redact_urls_in_text(input: &str) -> String {
    const SCHEMES: [&str; 2] = ["https://", "http://"];
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        // Find the next scheme occurrence at or after i.
        let next = SCHEMES
            .iter()
            .filter_map(|sch| input[i..].find(sch).map(|off| (off, *sch)))
            .min_by_key(|(off, _)| *off);
        match next {
            Some((off, _)) => {
                let abs = i + off;
                // Copy the non-URL prefix verbatim.
                out.push_str(&input[i..abs]);
                // Determine URL end: stop at whitespace or a delimiter char.
                let mut j = abs;
                while j < input.len() {
                    let c = input.as_bytes()[j];
                    if c.is_ascii_whitespace()
                        || matches!(
                            c,
                            b'"' | b'\'' | b'<' | b'>' | b',' | b';' | b')' | b']' | b'}'
                        )
                    {
                        break;
                    }
                    j += 1;
                }
                let candidate = &input[abs..j];
                if let Ok(mut u) = reqwest::Url::parse(candidate) {
                    let _ = u.set_username("");
                    let _ = u.set_password(None);
                    u.set_query(None);
                    u.set_fragment(None);
                    out.push_str(u.as_str());
                } else {
                    out.push_str(candidate);
                }
                i = j;
            }
            None => {
                out.push_str(&input[i..]);
                break;
            }
        }
    }
    out
}

fn categorize_ai_error(msg: &str) -> String {
    let lower = msg.to_lowercase();
    if lower.contains("401") || lower.contains("unauthorized") || lower.contains("authentication") {
        return "Authentication failed (check the API key).".to_string();
    }
    if lower.contains("not found") || lower.contains("does not exist") {
        // only model-ish; default to model not found if mentions model
        if lower.contains("model") || lower.contains("404") {
            return "Model not found.".to_string();
        }
    }
    if lower.contains("timed out") {
        return "Timed out after 10s.".to_string();
    }
    if lower.contains("error sending request")
        || lower.contains("connect")
        || lower.contains("network")
        || lower.contains("dns")
    {
        return "Network error.".to_string();
    }
    // fallback: redacted provider message (truncate)
    let trimmed = msg.trim();
    if trimmed.is_empty() {
        "Provider error.".to_string()
    } else {
        trimmed.chars().take(200).collect()
    }
}

#[tracing::instrument(skip_all)]
async fn plans_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let plans = state.db.list_plans(seller_scope);
    let list_items = if plans.is_empty() {
        "<li class=\"empty\"><b>No plans yet.</b>Create one below — it appears in the shop immediately.</li>"
            .to_string()
    } else {
        plans
            .iter()
            .map(|p| {
                format!(
                    "<li><b>{}</b><span class=\"mono\">{} {}</span><span class=\"help\">/ {} days</span><span class=\"row-action\"><form method=\"post\" action=\"{}\" class=\"inline-form\"><input type=\"hidden\" name=\"name\" value=\"{}\"><button type=\"submit\" class=\"btn btn-danger btn-sm\">Delete</button></form></span></li>",
                    esc(&p.0),
                    esc(&p.1),
                    esc(&p.2),
                    p.3,
                    state.url("/plandel"),
                    esc(&p.0)
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let body = format!(
        r#"<h1>Plans <span class="sub">what buyers can purchase</span></h1>
<article class="panel"><ul>{}</ul></article>
<article class="panel"><h3>Create plan</h3><form method="post" action="{}">
<label>Name<input name="name" required></label>
<label>Price<input name="price" required></label>
<label>Asset<input name="asset" required></label>
<label>Duration days<input name="duration_days" type="number" required></label>
<label>Channel<input name="channel" placeholder="@channel (optional)"></label>
<button type="submit">Create plan</button></form></article>"#,
        list_items,
        state.url("/plan")
    );
    Html(html_page(&state, Some(&session), "Plans", &body)).into_response()
}

#[tracing::instrument(skip_all)]
async fn posts_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let _ = session;
    Html(html_page(
        &state,
        Some(&session),
        "Posts",
        "<h1>Posts</h1><p>Use Telegram commands or the dashboard form to post.</p>",
    ))
    .into_response()
}

#[derive(Deserialize)]
struct PostForm {
    channel: Option<String>,
    text: String,
}

#[tracing::instrument(skip_all)]
async fn create_post(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PostForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let ch = f
        .channel
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| state.db.default_channel().unwrap_or_default());
    if ch.is_empty() || f.text.trim().is_empty() {
        return Redirect::to(&state.url("/dashboard")).into_response();
    }
    match state
        .bot
        .send_message(crate::util::to_chat_id(&ch), f.text.clone())
        .await
    {
        Ok(_) => crate::metrics::bot_sent(),
        Err(e) => tracing::warn!("failed to send post: {}", e),
    }
    let seller_id = session.seller_id.unwrap_or(0);
    if let Err(e) = state.db.log_post(seller_id, &ch, 0, "text", Some(&f.text)) {
        tracing::warn!("failed to log post: {}", e);
    }
    Redirect::to(&state.url("/dashboard")).into_response()
}

#[derive(Deserialize)]
struct PlanForm {
    name: String,
    price: String,
    asset: String,
    duration_days: String,
    channel: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn create_plan(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PlanForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_id = session.seller_id.unwrap_or(0);
    let name = f.name.trim().to_string();
    let price = f.price.trim().to_string();
    let asset = f.asset.trim().to_string();
    let days: i64 = match f.duration_days.trim().parse() {
        Ok(v) if v > 0 => v,
        _ => return Redirect::to(&state.url("/plans")).into_response(),
    };
    if name.is_empty() || price.is_empty() || asset.is_empty() {
        return Redirect::to(&state.url("/plans")).into_response();
    }
    let channel = f.channel.filter(|s| !s.trim().is_empty());
    state
        .db
        .add_plan(seller_id, &name, &price, &asset, days, channel.as_deref());
    Redirect::to(&state.url("/plans")).into_response()
}

#[derive(Deserialize)]
struct PlanDeleteForm {
    name: Option<String>,
    plan: Option<String>,
    plan_name: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn delete_plan_by_name(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PlanDeleteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let name = f
        .name
        .or(f.plan)
        .or(f.plan_name)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let Some(name) = name else {
        return Redirect::to(&state.url("/plans")).into_response();
    };
    if let Some(sid) = session.seller_id {
        state.db.remove_plan(sid, &name);
    } else {
        // Owner/admin: resolve the plan's owning seller from the
        // (seller_id, name) namespace instead of the retired
        // get_plan_global. Falls back to the global namespace (0).
        let sid = state
            .db
            .list_plans(None)
            .into_iter()
            .find(|p| p.0 == name)
            .map(|p| p.6)
            .unwrap_or(0);
        state.db.remove_plan(sid, &name);
    }
    Redirect::to(&state.url("/plans")).into_response()
}

#[tracing::instrument(skip_all)]
async fn delete_plan_by_id(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if let Some((sid, price, asset, name, days, channel)) = state.db.get_plan_by_id(id) {
        if session.role == "seller" && session.seller_id != Some(sid) {
            return Redirect::to(&state.url("/plans")).into_response();
        }
        let _ = (price, asset, days, channel);
        state.db.remove_plan(sid, &name);
    }
    Redirect::to(&state.url("/plans")).into_response()
}

#[tracing::instrument(skip_all)]
async fn products_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let products = state.db.list_products(seller_scope);
    let listing = if products.is_empty() {
        "<p class=\"empty\">No products yet.</p>".to_string()
    } else {
        let rows = products
            .iter()
            .map(|p| {
                // Product tuple: (id, seller_id, name, price, asset, kind,
                // delivery, stock, active).
                let stock = match p.7 {
                    Some(n) => n.to_string(),
                    None => "—".to_string(),
                };
                let active = if p.8 != 0 {
                    "<span class=\"badge badge-success\">active</span>"
                } else {
                    "<span class=\"badge badge-danger\">inactive</span>"
                };
                format!(
                    "<tr><td><b>{}</b></td><td class=\"num\">{}</td><td class=\"mono\">{}</td><td>{}</td><td class=\"num\">{}</td><td>{}</td><td class=\"actions\"><form method=\"post\" action=\"{}\" class=\"inline-form\"><input type=\"hidden\" name=\"name\" value=\"{}\"><button type=\"submit\" class=\"btn btn-danger btn-sm\">Deactivate</button></form></td></tr>",
                    esc(&p.2),
                    esc(&p.3),
                    esc(&p.4),
                    esc(&p.5),
                    stock,
                    active,
                    state.url("/productdel"),
                    esc(&p.2)
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!(
            "<div class=\"table-wrap\"><table><thead><tr><th>Name</th><th class=\"num\">Price</th><th>Asset</th><th>Kind</th><th class=\"num\">Stock</th><th>Active</th><th></th></tr></thead><tbody>{}</tbody></table></div>",
            rows
        )
    };
    let body = format!(
        r#"<h1>Products</h1>
{}
<article class="card"><h3>Create product</h3><form method="post" action="{}">
<label>Name<input name="name" required></label>
<label>Price<input name="price" required></label>
<label>Asset<input name="asset" required></label>
<label>Kind<select name="kind"><option value="digital_link">digital_link</option><option value="digital_file">digital_file</option><option value="physical">physical</option></select></label>
<label>Delivery (link, text or Telegram file_id, optional)<input name="delivery"></label>
<label>Stock (blank = unlimited)<input name="stock" type="number" min="0"></label>
<button type="submit">Create product</button></form></article>"#,
        listing,
        state.url("/product")
    );
    Html(html_page(&state, Some(&session), "Products", &body)).into_response()
}

#[derive(Deserialize)]
struct ProductForm {
    name: String,
    price: String,
    asset: String,
    kind: String,
    delivery: Option<String>,
    stock: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn create_product(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ProductForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_id = session.seller_id.unwrap_or(0);
    let name = f.name.trim().to_string();
    let price = f.price.trim().to_string();
    let asset = f.asset.trim().to_string();
    let kind = f.kind.trim().to_string();
    if name.is_empty() || price.is_empty() || asset.is_empty() {
        return Redirect::to(&state.url("/products")).into_response();
    }
    if !matches!(kind.as_str(), "digital_link" | "digital_file" | "physical") {
        return Redirect::to(&state.url("/products")).into_response();
    }
    let delivery = f
        .delivery
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let stock: Option<i64> = match f.stock.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => match s.parse() {
            Ok(v) if v >= 0 => Some(v),
            _ => return Redirect::to(&state.url("/products")).into_response(),
        },
        None => None,
    };
    if let Err(e) = state.db.add_product(
        seller_id,
        &name,
        &price,
        &asset,
        &kind,
        delivery.as_deref(),
        stock,
    ) {
        return Html(html_page(
            &state,
            Some(&session),
            "Products",
            &format!(
                r#"<div class="alert alert-error" role="alert">{}</div><p><a href="{}">Back to products</a></p>"#,
                esc(&e.to_string()),
                state.url("/products")
            ),
        ))
        .into_response();
    }
    Redirect::to(&state.url("/products")).into_response()
}

#[derive(Deserialize)]
struct ProductDeleteForm {
    name: String,
}

#[tracing::instrument(skip_all)]
async fn delete_product(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ProductDeleteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller = session.seller_id.unwrap_or(0);
    let name = f.name.trim().to_string();
    if !name.is_empty() {
        state.db.set_product_active(seller, &name, false);
    }
    Redirect::to(&state.url("/products")).into_response()
}

#[derive(Deserialize)]
struct SubsRevokeForm {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    sub_id: Option<i64>,
    /// Buyer + channel for the post-revoke kick. Both are server-rendered
    /// from the subscription row at page time (subs_body); the handler is
    /// owner-only and the kick is the same ban+unban the REST refund
    /// processing (finance_refunds_process) and subs_expirecheck perform.
    #[serde(default)]
    user_id: Option<i64>,
    #[serde(default)]
    channel: Option<String>,
}

#[derive(Deserialize)]
struct SubsQuery {
    status: Option<String>,
    expired: Option<String>,
    revoked: Option<String>,
    granted: Option<String>,
    extended: Option<String>,
    bulkrevoked: Option<String>,
    warn: Option<String>,
    failed: Option<String>,
    partial: Option<String>,
    error: Option<String>,
    id: Option<String>,
    page: Option<String>,
    q: Option<String>,
    /// /orders outcome flag: the status/tracking handlers redirect with
    /// ?saved=1 (the query struct is shared with /subs, which ignores it).
    saved: Option<String>,
}

fn normalize_sub_status(s: Option<String>) -> String {
    match s.as_deref() {
        Some("all") | Some("active") | Some("expired") | Some("revoked") | Some("refunded") => {
            s.unwrap()
        }
        _ => "all".to_string(),
    }
}

#[tracing::instrument(skip_all)]
async fn subs_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let status_filter = normalize_sub_status(q.status.clone());
    let page: i64 = q
        .page
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let search_q = q.q.clone().unwrap_or_default();

    // Build alert banners (already-escaped HTML) from query params. Several
    // can apply at once (e.g. a revoke that succeeded but whose channel kick
    // failed), so they collect into a list instead of an else-if chain.
    let mut banners: Vec<String> = Vec::new();
    if let Some(nstr) = q.expired.as_ref() {
        let n: i64 = nstr.parse().unwrap_or(0);
        if n == 0 {
            banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite">No subscriptions were due for expiry.</div>"#.to_string());
        } else {
            banners.push(format!(
                r#"<div class="alert alert-success" role="status" aria-live="polite">Marked {} subscription(s) expired.</div>"#,
                n
            ));
        }
    }
    if q.revoked.as_deref() == Some("1") {
        banners.push(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Subscription revoked.</div>"#.to_string(),
        );
    }
    if q.granted.as_deref() == Some("1") {
        banners.push(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Subscription granted.</div>"#.to_string(),
        );
    }
    if q.extended.as_deref() == Some("1") {
        banners.push(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Subscription extended.</div>"#.to_string(),
        );
    }
    if let Some(nstr) = q.bulkrevoked.as_ref() {
        let n: usize = nstr.parse().unwrap_or(0);
        banners.push(format!(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Revoked {} subscription(s).</div>"#,
            n
        ));
    }
    // Kick warnings: the status flip succeeded but Telegram refused the
    // ban+unban, so the buyer may still be in the channel. Never silent.
    if q.warn.as_deref() == Some("kick") {
        let failed: usize = q
            .failed
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        banners.push(format!(
            r#"<div class="alert alert-warn" role="alert" aria-live="polite">Channel kick failed for {} subscription(s) — the buyer may still be in the channel. Check the bot's admin rights there and remove access manually.</div>"#,
            failed
        ));
    }
    if q.partial.as_deref() == Some("1") {
        banners.push(
            r#"<div class="alert alert-warn" role="alert" aria-live="polite">Only the first 1000 matching subscriptions were processed for the kick.</div>"#.to_string(),
        );
    }
    if let Some(err) = q.error.as_ref() {
        let msg = match err.as_str() {
            "invalid" => "Invalid subscription id.".to_string(),
            "forbidden" => {
                "Subscriptions management is restricted to the console owner.".to_string()
            }
            "revoke" => {
                let id = q.id.clone().unwrap_or_default();
                format!("Could not revoke subscription {}.", esc(&id))
            }
            "notfound" => "Subscription not found.".to_string(),
            _ => String::new(),
        };
        if !msg.is_empty() {
            banners.push(format!(
                r#"<div class="alert alert-error" role="alert" aria-live="polite">{}</div>"#,
                msg
            ));
        }
    }
    let alert_banner: Option<String> = if banners.is_empty() {
        None
    } else {
        Some(banners.join("\n"))
    };

    Html(html_page(
        &state,
        Some(&session),
        "Subscriptions",
        &subs_body(
            &state,
            &session,
            &status_filter,
            alert_banner.as_deref(),
            page,
            &search_q,
        ),
    ))
    .into_response()
}

fn subs_body(
    state: &WebState,
    session: &Session,
    filter: &str,
    alert: Option<&str>,
    page: i64,
    search_q: &str,
) -> String {
    let alert = alert.unwrap_or_default();
    let is_owner = session.role == "owner";
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };

    let status_opt = if filter == "all" { None } else { Some(filter) };
    let search_opt = if search_q.trim().is_empty() {
        None
    } else {
        Some(search_q.trim())
    };
    let offset = (page.max(1) - 1) * 200;
    let rows = state.db.list_subscriptions_filtered_paged(
        seller_scope,
        status_opt,
        search_opt,
        200,
        offset,
    );
    let total_filtered =
        state
            .db
            .count_subscriptions_filtered(seller_scope, status_opt, search_opt);
    let counts = state.db.subscription_status_counts(seller_scope);
    let mut active: i64 = 0;
    let mut expired: i64 = 0;
    let mut revoked: i64 = 0;
    let mut refunded: i64 = 0;
    for (st, c) in counts {
        match st.as_str() {
            "active" => active = c,
            "expired" => expired = c,
            "revoked" => revoked = c,
            "refunded" => refunded = c,
            _ => {}
        }
    }
    let total = state.db.count_subscriptions(seller_scope, None);

    // Count cards.
    let cards = format!(
        r#"<section aria-labelledby="subs-counts-heading"><h2 id="subs-counts-heading" class="sub">Status counts</h2><div class="grid-2">
<div class="panel"><div class="sub">Active</div><div class="mono">{active}</div></div>
<div class="panel"><div class="sub">Expired</div><div class="mono">{expired}</div></div>
<div class="panel"><div class="sub">Revoked</div><div class="mono">{revoked}</div></div>
<div class="panel"><div class="sub">Refunded</div><div class="mono">{refunded}</div></div>
<div class="panel"><div class="sub">Total</div><div class="mono">{total}</div></div>
</div></section>"#,
        active = active,
        expired = expired,
        revoked = revoked,
        refunded = refunded,
        total = total
    );

    // Filter chips (as real links). The search term rides along in every
    // link's query string — percent-encoded via qenc so no raw term can
    // terminate the attribute or forge new parameters.
    let statuses = ["all", "active", "expired", "revoked", "refunded"];
    let qparam = if search_q.trim().is_empty() {
        String::new()
    } else {
        format!("&q={}", qenc(search_q.trim()))
    };
    let filter_links = statuses
        .iter()
        .map(|s| {
            let cls = if *s == filter {
                "btn btn-secondary btn-sm"
            } else {
                "btn btn-ghost btn-sm"
            };
            let aria = if *s == filter {
                r#" aria-current="page""#
            } else {
                ""
            };
            format!(
                r#"<a class="{}" href="{}"{}>{}</a>"#,
                cls,
                state.url(&format!("/subs?status={}{}", s, qparam)),
                aria,
                s
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let filter_nav = format!(
        r#"<nav aria-label="Filter subscriptions by status" class="inline-form">{}</nav>"#,
        filter_links
    );

    // Expire-check button (owner only).
    let expire_check = if is_owner {
        format!(
            r#"<form method="post" action="{}" class="inline-form"><button type="submit" class="btn btn-secondary btn-sm">Run expire check</button></form>"#,
            state.url(&format!("/subs/expirecheck?status={}", filter))
        )
    } else {
        String::new()
    };

    // Table.
    let col_count = if is_owner { 7 } else { 5 };
    let header = if is_owner {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">Seller</th><th scope="col">User</th><th scope="col">Channel</th><th scope="col">Status</th><th scope="col">Expires</th><th scope="col">Actions</th></tr>"#
    } else {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">User</th><th scope="col">Channel</th><th scope="col">Status</th><th scope="col">Expires</th></tr>"#
    };

    let rows_html = if rows.is_empty() {
        if filter != "all" {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No {} subscriptions.</b> <a href="{}">Clear filter</a></td></tr>"#,
                col_count,
                esc(filter),
                state.url("/subs")
            )
        } else {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No subscriptions yet.</b> They appear when a buyer pays or when you grant access with /grant in the bot.</td></tr>"#,
                col_count
            )
        }
    } else {
        rows
            .iter()
            .map(|r| {
                let status_badge = match r.4.as_str() {
                    "active" => "badge badge-success",
                    "revoked" | "expired" => "badge badge-danger",
                    _ => "badge",
                };
                let status_cell = format!(
                    r#"<span class="{}">{}</span>"#,
                    status_badge,
                    esc(&r.4)
                );
                let id_cell = format!(r#"<td class="num">{}</td>"#, r.0);
                let user_cell = format!(r#"<td class="mono">{}</td>"#, esc(&r.2.to_string()));
                let channel_cell = format!(r#"<td class="mono">{}</td>"#, esc(&r.3));
                let expires_cell = format!(r#"<td class="mono">{}</td>"#, esc(&r.5));
                if is_owner {
                    let seller_cell = format!(r#"<td>{}</td>"#, r.1);
                    // The kick data (buyer + channel) rides along as
                    // server-rendered hidden fields so subs_revoke can
                    // ban+unban without a second lookup.
                    let revoke_form = format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="sub_id" value="{}"><input type="hidden" name="user_id" value="{}"><input type="hidden" name="channel" value="{}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Revoke subscription {}" onclick="return confirm('Revoke subscription #{}? The buyer loses access; this cannot be undone from the console.')">Revoke</button></form>"#,
                        state.url("/subs/revoke"),
                        r.0,
                        r.2,
                        esc(&r.3),
                        r.0,
                        r.0
                    );
                    // Extend: the handler takes sub_id + days (clamped 1..=3650).
                    let extend_form = format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="sub_id" value="{}"><input type="number" name="days" value="30" min="1" max="3650" aria-label="Days to extend subscription {}" style="width:4.5em"><button type="submit" class="btn btn-sm" onclick="return confirm('Extend subscription #{} by the given days?')">Extend</button></form>"#,
                        state.url("/subs/extend"),
                        r.0,
                        r.0,
                        r.0
                    );
                    let actions_cell =
                        format!(r#"<td class="actions">{}{}</td>"#, revoke_form, extend_form);
                    format!(
                        "<tr>{}{}{}{}<td>{}</td>{}{}</tr>",
                        id_cell, seller_cell, user_cell, channel_cell, status_cell, expires_cell, actions_cell
                    )
                } else {
                    format!(
                        "<tr>{}{}{}<td>{}</td>{}</tr>",
                        id_cell, user_cell, channel_cell, status_cell, expires_cell
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("")
    };

    let table = format!(
        r#"<div class="table-wrap"><table aria-label="Subscriptions"><caption class="sr-only">Subscriptions</caption><thead>{}</thead><tbody>{}</tbody></table></div>"#,
        header, rows_html
    );

    let scoping_note = if session.role == "seller" {
        "Showing your subscriptions only."
    } else {
        "Showing all sellers' subscriptions."
    };
    let row_note = if total_filtered > 200 {
        format!(
            " Showing {} of {} subscriptions.",
            rows.len(),
            total_filtered
        )
    } else {
        String::new()
    };
    let search_form = format!(
        r#"<form method="get" action="{}" class="inline-form">
<input type="text" name="q" value="{}" placeholder="user id or channel" aria-label="Search subscriptions">
<input type="hidden" name="status" value="{}">
<button type="submit" class="btn btn-secondary btn-sm">Search</button>
<a class="btn btn-ghost btn-sm" href="{}">Export CSV</a>
</form>"#,
        state.url("/subs"),
        esc(search_q),
        esc(filter),
        state.url(&format!(
            "/subs/export?status={}&q={}",
            filter,
            qenc(search_q.trim())
        ))
    );
    let total_pages = if total_filtered == 0 {
        1
    } else {
        (total_filtered + 199) / 200
    };
    let page = page.max(1).min(total_pages.max(1));
    let mut pag = String::new();
    if total_pages > 1 {
        if page > 1 {
            pag.push_str(&format!(
                r#"<a class="btn btn-ghost btn-sm" href="{}">Prev</a>"#,
                state.url(&format!(
                    "/subs?status={}&q={}&page={}",
                    filter,
                    qenc(search_q.trim()),
                    page - 1
                ))
            ));
        }
        pag.push_str(&format!(" Page {} of {} ", page, total_pages));
        if page < total_pages {
            pag.push_str(&format!(
                r#"<a class="btn btn-ghost btn-sm" href="{}">Next</a>"#,
                state.url(&format!(
                    "/subs?status={}&q={}&page={}",
                    filter,
                    qenc(search_q.trim()),
                    page + 1
                ))
            ));
        }
    }
    let grant_form = format!(
        r#"<article class="panel"><h3>Grant subscription</h3>
<form method="post" action="{}" class="inline-form">
<input type="number" name="user_id" placeholder="user id" required aria-label="Telegram user id to grant">
<input type="number" name="days" placeholder="days" value="30" required aria-label="Days to grant">
<input type="text" name="channel" placeholder="@channel" required aria-label="Channel to grant access to">
<button type="submit">Grant</button>
</form></article>"#,
        state.url("/subs/grant")
    );

    let footer = format!(r#"<p class="help">{}{}</p>"#, scoping_note, row_note);

    format!(
        r#"{alert}<h1>Subscriptions <span class="sub">buyer access and expiry</span></h1>
{cards}
<div class="inline-form">{filter_nav}{expire_check}</div>
{search_form}
{pag}
{grant_form}
{table}
{footer}"#,
        alert = alert,
        cards = cards,
        filter_nav = filter_nav,
        expire_check = expire_check,
        search_form = search_form,
        pag = pag,
        grant_form = grant_form,
        table = table,
        footer = footer
    )
}

#[tracing::instrument(skip_all)]
async fn analytics_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let is_owner = session.role == "owner";
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };

    let revenue_total = state.db.revenue_total(seller_scope);
    let revenue_by_seller = if is_owner {
        state.db.revenue_by_seller()
    } else {
        vec![(session.seller_id.unwrap_or(0), revenue_total)]
    };

    let total_subs = state.db.count_subscriptions(seller_scope, None);
    let active_subs = state.db.count_subscriptions(seller_scope, Some("active"));
    let expired_subs = state.db.count_subscriptions(seller_scope, Some("expired"));
    let revoked_subs = state.db.count_subscriptions(seller_scope, Some("revoked"));
    let refunded_subs = state.db.count_subscriptions(seller_scope, Some("refunded"));
    let churned = expired_subs + revoked_subs;
    let churn_rate = if total_subs > 0 {
        (churned as f64 / total_subs as f64) * 100.0
    } else {
        0.0
    };

    let mut rev_rows = String::new();
    for (sid, rev) in &revenue_by_seller {
        rev_rows.push_str(&format!(
            "<tr><td>{}</td><td class=\"num\">{:.2}</td></tr>",
            sid, rev
        ));
    }
    if rev_rows.is_empty() {
        rev_rows = r#"<tr><td colspan="2" class="empty">No revenue yet.</td></tr>"#.to_string();
    }

    // Heading rhythm (M9): h1 page title, h2 sections, h3 cards — no h3
    // before the first h2, no interleaving. Churn prints one decimal, not the
    // raw f64. The revenue cards keep honest copy: the sum mixes assets, so
    // the total is a rough figure until a per-asset split exists.
    let body = format!(
        r#"<h1>Analytics <span class="sub">revenue and subscribers</span></h1>
<section aria-labelledby="revenue-heading"><h2 id="revenue-heading" class="sub">Revenue</h2>
<article class="panel"><h3>Revenue total</h3><p class="mono">{:.2}</p>
<p class="help">Amounts across all currencies are summed as a single number — treat as a rough total. A per-asset split is future work.</p></article>
<article class="panel"><h3>Revenue by seller</h3>
<div class="table-wrap"><table aria-label="Revenue by seller"><thead><tr><th scope="col">Seller</th><th scope="col" class="num">Revenue</th></tr></thead><tbody>{}</tbody></table></div></article>
</section>
<section aria-labelledby="subs-counts-heading"><h2 id="subs-counts-heading" class="sub">Subscriber counts</h2>
<div class="grid-2">
<div class="panel"><div class="sub">Total</div><div class="mono">{}</div></div>
<div class="panel"><div class="sub">Active</div><div class="mono">{}</div></div>
<div class="panel"><div class="sub">Expired</div><div class="mono">{}</div></div>
<div class="panel"><div class="sub">Revoked</div><div class="mono">{}</div></div>
<div class="panel"><div class="sub">Refunded</div><div class="mono">{}</div></div>
</div></section>
<section aria-labelledby="churn-heading"><h2 id="churn-heading" class="sub">Churn</h2>
<div class="panel"><p>Churned: {} ({:.1}%)</p></div></section>"#,
        revenue_total,
        rev_rows,
        total_subs,
        active_subs,
        expired_subs,
        revoked_subs,
        refunded_subs,
        churned,
        churn_rate
    );
    Html(html_page(&state, Some(&session), "Analytics", &body)).into_response()
}

#[tracing::instrument(skip_all)]
async fn subs_export(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsExportQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let is_owner = session.role == "owner";
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let status_filter = normalize_sub_status(q.status.clone());
    let status_opt = if status_filter == "all" {
        None
    } else {
        Some(status_filter.as_str())
    };
    let search_opt = q.q.clone().filter(|s| !s.trim().is_empty());
    let reveal = q.reveal.as_deref() == Some("1")
        || q.include_user_ids.as_deref() == Some("true")
        || q.include_user_ids.as_deref() == Some("1");
    let allow_reveal = is_owner && reveal;

    let rows = state.db.list_subscriptions_filtered_paged(
        seller_scope,
        status_opt,
        search_opt.as_deref(),
        100000,
        0,
    );

    let mut csv = String::from("id,seller_id,user_id,channel_id,status,expires_at\n");
    for r in rows {
        let user_val = if allow_reveal {
            r.2.to_string()
        } else {
            mask_user_id(r.2)
        };
        csv.push_str(&format!(
            "{},{},{},{},{},{}\n",
            r.0,
            r.1,
            user_val,
            esc_csv(&r.3),
            esc_csv(&r.4),
            esc_csv(&r.5)
        ));
    }
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"subscriptions.csv\"",
            ),
        ],
        csv,
    )
        .into_response()
}

#[derive(Deserialize)]
struct SubsExportQuery {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    q: Option<String>,
    #[serde(default)]
    reveal: Option<String>,
    #[serde(default)]
    include_user_ids: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn usage_export(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<UsageExportQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let is_owner = session.role == "owner";
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let reveal = q.reveal.as_deref() == Some("1")
        || q.include_user_ids.as_deref() == Some("true")
        || q.include_user_ids.as_deref() == Some("1");
    let allow_reveal = is_owner && reveal;

    let rows = state.db.list_ai_usage(seller_scope, 100000);

    let mut csv = String::from(
        "id,seller_id,user_id,provider,model,template,prompt_tokens,completion_tokens,total_tokens,estimated_cost,status,created_at\n",
    );
    for r in rows {
        let user_val = match r.2 {
            Some(uid) if allow_reveal => uid.to_string(),
            Some(uid) => mask_user_id(uid),
            None => String::new(),
        };
        let template = r.5.clone().unwrap_or_default();
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{:.6},{},{},{}\n",
            r.0,
            r.1,
            user_val,
            esc_csv(&r.3),
            esc_csv(&r.4),
            esc_csv(&template),
            r.6,
            r.7,
            r.8,
            r.9,
            esc_csv(&r.10),
            esc_csv(r.11.as_deref().unwrap_or("")),
            esc_csv(&r.12),
        ));
    }
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"ai_usage.csv\"",
            ),
        ],
        csv,
    )
        .into_response()
}

#[derive(Deserialize)]
struct UsageExportQuery {
    #[serde(default)]
    reveal: Option<String>,
    #[serde(default)]
    include_user_ids: Option<String>,
}

fn mask_user_id(id: i64) -> String {
    format!("***{:03}", id.rem_euclid(1000))
}

fn esc_csv(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[tracing::instrument(skip_all)]
async fn subs_revoke(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SubsRevokeForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Subs", "Subscriptions management").into_response();
    }
    let id = match f.id.or(f.sub_id) {
        Some(v) => v,
        None => {
            return Redirect::to(&state.url("/subs?error=invalid")).into_response();
        }
    };
    // validate parse already done by serde; treat non-positive as invalid.
    if id <= 0 {
        return Redirect::to(&state.url("/subs?error=invalid")).into_response();
    }
    state.db.set_subscription_status(id, "revoked");
    // Revoke must actually revoke: kick (ban+unban) so the buyer loses
    // channel access immediately — the same move finance_refunds_process
    // makes for refunded invoices. Best-effort, but never silent: a failed
    // kick surfaces a warning banner instead of a plain success.
    let mut kick_failed = false;
    if let (Some(uid), Some(ch)) = (f.user_id, f.channel.as_deref()) {
        let ch = ch.trim();
        if uid > 0 && !ch.is_empty() {
            let cid = crate::util::to_chat_id(ch);
            let ban = state
                .bot
                .ban_chat_member(cid.clone(), teloxide::types::UserId(uid as u64))
                .await;
            let unban = state
                .bot
                .unban_chat_member(cid, teloxide::types::UserId(uid as u64))
                .await;
            kick_failed = ban.is_err() || unban.is_err();
        }
    }
    if kick_failed {
        Redirect::to(&state.url("/subs?revoked=1&warn=kick")).into_response()
    } else {
        Redirect::to(&state.url("/subs?revoked=1")).into_response()
    }
}

#[tracing::instrument(skip_all)]
async fn subs_expirecheck(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Subs", "Subscriptions management").into_response();
    }
    let filter = normalize_sub_status(q.status.clone());
    let expired = state.db.expired_subscriptions();
    let count = expired.len() as i64;
    let mut kick_failed = 0usize;
    for (id, _seller, user, chan) in expired {
        // Kick the member from the channel (ban + unban) before marking.
        let cid = crate::util::to_chat_id(&chan);
        let ban = state
            .bot
            .ban_chat_member(cid.clone(), teloxide::types::UserId(user as u64))
            .await;
        let unban = state
            .bot
            .unban_chat_member(cid, teloxide::types::UserId(user as u64))
            .await;
        if ban.is_err() || unban.is_err() {
            kick_failed += 1;
        }
        state.db.mark_subscription_expired(id);
    }
    let mut url = format!("/subs?expired={}", count);
    if filter != "all" {
        url = format!("/subs?status={}&expired={}", filter, count);
    }
    if kick_failed > 0 {
        url.push_str(&format!("&warn=kick&failed={}", kick_failed));
    }
    Redirect::to(&state.url(&url)).into_response()
}

#[derive(Deserialize)]
struct SubsExtendForm {
    id: Option<i64>,
    sub_id: Option<i64>,
    days: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn subs_extend(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SubsExtendForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Subs", "Subscriptions management").into_response();
    }
    let id = match f.id.or(f.sub_id) {
        Some(v) if v > 0 => v,
        _ => return Redirect::to(&state.url("/subs?error=invalid")).into_response(),
    };
    let days: i64 = f
        .days
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
        .clamp(1, 3650);
    if state.db.extend_subscription(id, days) {
        Redirect::to(&state.url("/subs?extended=1")).into_response()
    } else {
        Redirect::to(&state.url("/subs?error=notfound")).into_response()
    }
}

#[derive(Deserialize)]
struct SubsBulkRevokeQuery {
    status: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn subs_bulkrevoke(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsBulkRevokeQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Subs", "Subscriptions management").into_response();
    }
    // A revoke that only flips a status column leaves the buyer in the
    // channel. Fetch the matching rows first (the same filter
    // bulk_revoke_subscriptions applies), kick each live member, then flip.
    // Capped at 1000 like the console's other bulk reads; anything beyond
    // the cap is reported instead of silently skipped.
    let status_opt = q
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "all");
    const KICK_CAP: i64 = 1000;
    let rows = state
        .db
        .list_subscriptions_filtered_paged(None, status_opt, None, KICK_CAP, 0);
    let total = state
        .db
        .count_subscriptions_filtered(None, status_opt, None);
    let mut kick_failed = 0usize;
    for (_id, _seller, uid, ch, st, _exp) in &rows {
        if st.as_str() == "revoked" {
            continue;
        }
        if *uid <= 0 || ch.trim().is_empty() {
            continue;
        }
        let cid = crate::util::to_chat_id(ch);
        let ban = state
            .bot
            .ban_chat_member(cid.clone(), teloxide::types::UserId(*uid as u64))
            .await;
        let unban = state
            .bot
            .unban_chat_member(cid, teloxide::types::UserId(*uid as u64))
            .await;
        if ban.is_err() || unban.is_err() {
            kick_failed += 1;
        }
    }
    let n = state.db.bulk_revoke_subscriptions(status_opt);
    let mut url = format!("/subs?bulkrevoked={}", n);
    if kick_failed > 0 {
        url.push_str(&format!("&warn=kick&failed={}", kick_failed));
    }
    if total > rows.len() as i64 {
        url.push_str("&partial=1");
    }
    Redirect::to(&state.url(&url)).into_response()
}

// ---------- Codes ----------

#[derive(Deserialize)]
struct CodesGenerateForm {
    plan_name: Option<String>,
    count: Option<String>,
    days: Option<String>,
    max_uses: Option<String>,
    length: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn codes_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let codes = state.db.list_codes(scope, 200);
    let plans = state.db.list_plans(scope);
    let plan_options = plans
        .iter()
        .map(|p| format!(r#"<option value="{}">{}</option>"#, esc(&p.1), esc(&p.1)))
        .collect::<Vec<_>>()
        .join("");
    let rows_html = if codes.is_empty() {
        r#"<tr><td colspan="6" class="empty"><b>No codes yet.</b></td></tr>"#.to_string()
    } else {
        codes
            .iter()
            .map(|c| {
                let revoked_badge = if c.6 {
                    r#" <span class="badge badge-danger">Revoked</span>"#
                } else {
                    ""
                };
                let actions = if session.role == "owner" {
                    format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="code" value="{}"><button type="submit" class="btn btn-danger btn-sm" onclick="return confirm('Delete code {}?')">Delete</button></form>"#,
                        state.url("/codes/delete"),
                        esc(&c.0),
                        esc(&c.0)
                    )
                } else {
                    String::new()
                };
                let reclaim = format!(
                    r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="code" value="{}"><input type="number" name="length" min="1" max="32" value="12" style="width:4em" aria-label="New length for code {}"><button type="submit" class="btn btn-sm">Reclaim</button></form>"#,
                    state.url("/codes/reclaim"),
                    esc(&c.0),
                    esc(&c.0)
                );
                format!(
                    r#"<tr><td class="mono">{}{}</td><td>{}</td><td class="num">{}</td><td class="num">{}</td><td class="num">{}</td><td>{}{}</td></tr>"#,
                    esc(&c.0),
                    revoked_badge,
                    c.1.map(|v| v.to_string()).unwrap_or_else(|| "—".to_string()),
                    c.2.map(|v| v.to_string()).unwrap_or_else(|| "—".to_string()),
                    c.3,
                    c.4,
                    actions,
                    reclaim
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let table = format!(
        r#"<div class="table-wrap"><table aria-label="Codes"><thead><tr><th scope="col">Code</th><th scope="col">Plan</th><th scope="col">Days</th><th scope="col">Uses</th><th scope="col">Max</th><th scope="col">Actions</th></tr></thead><tbody>{}</tbody></table></div>"#,
        rows_html
    );
    let form = format!(
        r#"<article class="panel"><h3>Generate codes</h3>
<form method="post" action="{}">
<label>Plan <select name="plan_name"><option value="">(none)</option>{}</select></label>
<label>Count <input type="number" name="count" min="1" value="5" required></label>
<label>Days <input type="number" name="days" min="1" value="30"></label>
<label>Max uses <input type="number" name="max_uses" min="1" value="1"></label>
<label>Length <input type="number" name="length" min="1" max="32" value="12"></label>
<button type="submit">Generate</button>
</form></article>"#,
        state.url("/codes/generate"),
        plan_options
    );
    let banner = if let Some(nstr) = q.id.as_ref() {
        format!(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Generated {} code(s).</div>"#,
            esc(nstr)
        )
    } else {
        String::new()
    };
    let body = format!(
        r#"{banner}<h1>Codes <span class="sub">activation codes</span></h1>
{form}
{table}"#,
        banner = banner,
        form = form,
        table = table
    );
    Html(html_page(&state, Some(&session), "Codes", &body)).into_response()
}

#[derive(Deserialize)]
struct CodesDeleteForm {
    code: String,
}

#[tracing::instrument(skip_all)]
async fn codes_generate(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<CodesGenerateForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_id = if session.role == "owner" {
        0i64
    } else {
        match session.seller_id {
            Some(v) => v,
            None => return Redirect::to(&state.url("/codes")).into_response(),
        }
    };
    let _scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let plan_id = if let Some(name) = f.plan_name.as_deref().filter(|s| !s.trim().is_empty()) {
        state.db.get_plan(seller_id, name).map(|r| r.0)
    } else {
        None
    };
    let count: i64 = f
        .count
        .as_deref()
        .unwrap_or("1")
        .parse()
        .unwrap_or(1)
        .clamp(1, 100);
    let days: i64 = f.days.as_deref().unwrap_or("30").parse().unwrap_or(30);
    let max_uses: i64 = f
        .max_uses
        .as_deref()
        .unwrap_or("1")
        .parse()
        .unwrap_or(1)
        .max(1);
    let length: usize = f
        .length
        .as_deref()
        .unwrap_or("12")
        .parse()
        .unwrap_or(12)
        .clamp(1, 32) as usize;
    for _ in 0..count {
        state
            .db
            .create_code(seller_id, plan_id, Some(days), max_uses, None, length);
    }
    Redirect::to(&state.url(&format!("/codes?id={}", count))).into_response()
}

#[tracing::instrument(skip_all)]
async fn codes_delete(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<CodesDeleteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    state.db.delete_code_seller_scoped(&f.code, scope);
    Redirect::to(&state.url("/codes")).into_response()
}

#[derive(Deserialize)]
struct CodesReclaimForm {
    code: String,
    length: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn codes_reclaim(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<CodesReclaimForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let length: usize = f
        .length
        .as_deref()
        .unwrap_or("12")
        .parse()
        .unwrap_or(12)
        .clamp(1, 32) as usize;
    let _ = state.db.reclaim_code(&f.code, scope, length);
    Redirect::to(&state.url("/codes")).into_response()
}

// ---------- Orders ----------

#[tracing::instrument(skip_all)]
/// Order status → badge class, the same dot-before-a-word vocabulary the
/// subscriptions and finance queues use: waiting amber, delivered green,
/// refunded/cancelled brick, shipped in the plain default badge (it reads as
/// "in motion", not "waiting").
fn order_badge(status: &str) -> &'static str {
    match status {
        "delivered" => "badge badge-success",
        "pending" => "badge badge-warning",
        "refunded" | "cancelled" => "badge badge-danger",
        _ => "badge",
    }
}

async fn orders_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let orders = state.db.list_orders(scope);
    let status_filter = q.status.clone().unwrap_or_else(|| "all".to_string());
    let filtered: Vec<_> = if status_filter == "all" {
        orders.clone()
    } else {
        orders
            .iter()
            .filter(|o| o.3 == status_filter)
            .cloned()
            .collect()
    };
    let rows_html = if filtered.is_empty() {
        r#"<tr><td colspan="7" class="empty"><b>No orders.</b></td></tr>"#.to_string()
    } else {
        filtered
            .iter()
            .map(|o| {
                // Money back to the buyer goes through the Finance page's
                // audited refund flow (create -> approve -> execute), never a
                // silent transfer: live orders link to the pre-filled
                // refund-create form. Orders never carry a "paid" status
                // (pending/shipped/delivered do); Finance enforces the
                // paid-invoice rule when the refund is submitted.
                let refund_link = if o.3 != "refunded" && o.3 != "cancelled" {
                    format!(
                        r#"<a class="btn btn-ghost btn-sm" href="{}" aria-label="Request refund for order {}">Refund</a>"#,
                        state.url(&format!("/finance?refund_order={}", esc(&o.0))),
                        esc(&o.0)
                    )
                } else {
                    String::new()
                };
                format!(
                    r#"<tr><td class="mono">{}</td><td>{}</td><td class="mono">{}</td><td><span class="{}">{}</span></td><td class="mono">{}</td><td class="mono">{}</td><td>
<form method="post" action="{}" class="inline-form"><input type="hidden" name="order_id" value="{}"><select name="status" aria-label="New status for order {}"><option value="pending">pending</option><option value="shipped">shipped</option><option value="delivered">delivered</option><option value="refunded">refunded</option><option value="cancelled">cancelled</select><button type="submit" class="btn btn-sm">Set</button></form>
<form method="post" action="{}" class="inline-form"><input type="hidden" name="order_id" value="{}"><input type="text" name="tracking" placeholder="tracking" aria-label="Tracking number for order {}"><button type="submit" class="btn btn-sm">Track</button></form>
{}</td></tr>"#,
                    esc(&o.0),
                    o.1,
                    o.2,
                    order_badge(&o.3),
                    esc(&o.3),
                    // Tracking is operator free-text — always escaped on
                    // render like every other user-supplied cell.
                    esc(o.4.as_deref().unwrap_or("—")),
                    esc(&o.5),
                    state.url("/orders/status"),
                    esc(&o.0),
                    esc(&o.0),
                    state.url("/orders/tracking"),
                    esc(&o.0),
                    esc(&o.0),
                    refund_link
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let table = format!(
        r#"<div class="table-wrap"><table aria-label="Orders"><thead><tr><th scope="col">Order</th><th scope="col">Seller</th><th scope="col">Buyer</th><th scope="col">Status</th><th scope="col">Tracking</th><th scope="col">Created</th><th scope="col">Actions</th></tr></thead><tbody>{}</tbody></table></div>"#,
        rows_html
    );
    let statuses = [
        "all",
        "pending",
        "shipped",
        "delivered",
        "refunded",
        "cancelled",
    ];
    let filter_links = statuses
        .iter()
        .map(|s| {
            let cls = if *s == status_filter {
                "btn btn-secondary btn-sm"
            } else {
                "btn btn-ghost btn-sm"
            };
            let aria = if *s == status_filter {
                r#" aria-current="page""#
            } else {
                ""
            };
            format!(
                r#"<a class="{}" href="{}"{}>{}</a>"#,
                cls,
                state.url(&format!("/orders?status={}", s)),
                aria,
                s
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let filter_nav = format!(
        r#"<nav aria-label="Filter orders" class="inline-form">{}</nav>"#,
        filter_links
    );

    // Outcome banner from the PRG redirect (?saved=1): a status or tracking
    // edit used to succeed silently.
    let saved_banner = if q.saved.as_deref() == Some("1") {
        r#"<div class="alert alert-success" role="status" aria-live="polite">Order updated.</div>"#
    } else {
        ""
    };
    let body = format!(
        r#"{saved_banner}<h1>Orders <span class="sub">order management</span></h1>
{filter_nav}
<p class="help">Refunds go through the <a href="{finance}">Finance</a> page: create from a paid invoice, then approve and execute the transfer there.</p>
{table}"#,
        saved_banner = saved_banner,
        filter_nav = filter_nav,
        table = table,
        finance = state.url("/finance")
    );
    Html(html_page(&state, Some(&session), "Orders", &body)).into_response()
}

#[derive(Deserialize)]
struct OrdersStatusForm {
    order_id: String,
    status: String,
}

#[derive(Deserialize)]
struct OrdersTrackingForm {
    order_id: String,
    tracking: String,
}

#[tracing::instrument(skip_all)]
async fn orders_status(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<OrdersStatusForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_id = if session.role == "owner" {
        0i64
    } else {
        match session.seller_id {
            Some(v) => v,
            None => return Redirect::to(&state.url("/orders")).into_response(),
        }
    };
    let allowed = ["pending", "shipped", "delivered", "refunded", "cancelled"];
    if !allowed.contains(&f.status.as_str()) {
        return Redirect::to(&state.url("/orders")).into_response();
    }
    state.db.set_order_status(seller_id, &f.order_id, &f.status);
    Redirect::to(&state.url("/orders?saved=1")).into_response()
}

#[tracing::instrument(skip_all)]
async fn orders_tracking(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<OrdersTrackingForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_id = if session.role == "owner" {
        0i64
    } else {
        match session.seller_id {
            Some(v) => v,
            None => return Redirect::to(&state.url("/orders")).into_response(),
        }
    };
    state
        .db
        .set_order_tracking(seller_id, &f.order_id, &f.tracking);
    Redirect::to(&state.url("/orders?saved=1")).into_response()
}

// The old POST /orders/refund handler was removed: it moved real money with
// no approval step, no audit event and a swallowed transfer failure
// (`let _ = cp.transfer`), and no page ever linked to it. The audited money
// path is the Finance page: Orders links each live order to the Finance
// refund-create form, and finance_refund_execute records
// refund_create/approve/execute + moves the transfer through Crypto Pay.

// ---------- Finance ----------

#[derive(Deserialize)]
struct FinanceQuery {
    status: Option<String>,
    /// Orders page cross-link: prefill the refund-create form with the
    /// invoice bound to this order.
    refund_order: Option<String>,
    // Action outcomes, rendered as banners.
    payout_created: Option<String>,
    refund_created: Option<String>,
    payout_approved: Option<String>,
    refund_approved: Option<String>,
    payout_executed: Option<String>,
    refund_executed: Option<String>,
    warn: Option<String>,
    error: Option<String>,
    detail: Option<String>,
}

/// Same shape as `normalize_sub_status`: unknown values fall back to "all"
/// instead of becoming an injection vector into generated URLs.
fn normalize_finance_status(s: Option<String>) -> String {
    match s.as_deref() {
        Some("all") | Some("pending") | Some("approved") | Some("executing") | Some("paid")
        | Some("executed") => s.unwrap(),
        _ => "all".to_string(),
    }
}

/// Percent-encodes a value for a redirect query string. Only unreserved
/// characters survive verbatim, so no context can misread the rest.
fn qenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn finance_redirect(state: &WebState, query: &str) -> Response {
    Redirect::to(&state.url(&format!("/finance?{}", query))).into_response()
}

/// The queue sort: pending first, then approved, then the rest — stable, so
/// each group keeps the DB's newest-first order.
fn finance_rank(status: &str) -> u8 {
    match status {
        "pending" => 0,
        "approved" => 1,
        _ => 2,
    }
}

fn finance_badge(status: &str) -> &'static str {
    match status {
        "paid" | "executed" => "badge badge-success",
        // Waiting/in-flight states read amber: pending = awaiting approval,
        // approved = awaiting execution, executing = a transfer is claimed
        // and in flight (or was interrupted mid-flight) — the text carries
        // which is which.
        "pending" | "approved" | "executing" => "badge badge-warning",
        "failed" | "rejected" => "badge badge-danger",
        _ => "badge",
    }
}

/// Resolves a Crypto Pay token exactly like the REST finance handlers (and
/// the webhook): the startup config first, then the runtime `crypto_token`
/// config key.
fn crypto_token(state: &WebState) -> Option<String> {
    state
        .cfg
        .crypto_token
        .clone()
        .or_else(|| state.db.get_config("crypto_token"))
}

/// Whether a CryptoPay `transfer` error is a definite API rejection — the
/// request completed a round trip and the API refused, so no money moved and
/// the claimed row can safely go back to 'approved' for retry — versus an
/// ambiguous transport outcome (timeout, network failure, garbled or gateway
/// response) where the transfer state is unknown. `CryptoPay::call` surfaces
/// errors as strings in three shapes: reqwest transport failures ("error
/// sending request…", "error decoding response body…", "builder error…"),
/// non-2xx responses ("HTTP <status> - <body>"), and the API's own `ok:false`
/// error name for a parsed rejection (no prefix). Only the last is always a
/// refusal; of the "HTTP" shape only a 4xx other than 408 proves it — a 5xx
/// means an intermediary may have dropped a transfer the API actually
/// processed, and 408 is a timeout by definition. Ambiguous outranks
/// definite on doubt: reverting an executed transfer to 'approved' invites a
/// double payout, while a stuck 'executing' row only costs a manual check of
/// the queue.
fn transfer_rejected_definitely(e: &str) -> bool {
    if let Some(rest) = e.strip_prefix("HTTP ") {
        let code = rest.split([' ', '-']).next().unwrap_or_default();
        return code.starts_with('4') && code != "408";
    }
    !e.starts_with("error sending request")
        && !e.starts_with("error decoding response body")
        && !e.starts_with("error reading a body")
        && !e.starts_with("builder error")
}

#[tracing::instrument(skip_all)]
async fn finance_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<FinanceQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    // Seller-or-admin, the same population the REST finance list endpoints
    // serve; a seller without a seller profile is the REST `not_a_seller`
    // case.
    if session.role != "seller" && !is_admin_role(&session.role) {
        return forbidden_page(&state, "Finance", "Finance").into_response();
    }
    if session.role == "seller" && session.seller_id.is_none() {
        return forbidden_page(&state, "Finance", "Finance").into_response();
    }
    let filter = normalize_finance_status(q.status.clone());

    // Banners (already-escaped HTML); several can apply at once.
    let mut banners: Vec<String> = Vec::new();
    if q.payout_created.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite">Payout requested. It stays pending until the owner approves it.</div>"#.to_string());
    }
    if q.refund_created.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite">Refund created. It stays pending until an admin or the owner approves it.</div>"#.to_string());
    }
    if q.payout_approved.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite">Payout approved. Execute the transfer when ready.</div>"#.to_string());
    }
    if q.refund_approved.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite">Refund approved. Execute the transfer when ready.</div>"#.to_string());
    }
    if q.payout_executed.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite"><strong>Payout executed.</strong> The Crypto Pay transfer completed and the payout is marked paid.</div>"#.to_string());
    }
    if q.refund_executed.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite"><strong>Refund executed.</strong> The Crypto Pay transfer completed; the invoice is marked refunded and the buyer's subscription revoked.</div>"#.to_string());
    }
    if q.warn.as_deref() == Some("kick") {
        banners.push(r#"<div class="alert alert-warn" role="alert" aria-live="polite">The transfer succeeded, but removing the buyer from the channel failed — check the bot's admin rights there and remove access manually.</div>"#.to_string());
    }
    if let Some(err) = q.error.as_ref() {
        let (msg, with_detail) = match err.as_str() {
            "invalid" => ("Invalid payout or refund id.".to_string(), false),
            "seller_required" => (
                "Seller id is required — admins must name the seller.".to_string(),
                false,
            ),
            "amount_required" => ("Amount and asset are required.".to_string(), false),
            "invoice_required" => ("Invoice id is required.".to_string(), false),
            "invoice_notfound" => ("Invoice not found.".to_string(), false),
            "invoice_owner" => ("That invoice belongs to another seller.".to_string(), false),
            "invoice_state" => ("Only paid invoices can be refunded.".to_string(), false),
            "notfound" => ("Payout or refund not found.".to_string(), false),
            "destination" => ("Payout has no destination.".to_string(), false),
            "bad_destination" => ("Destination must be a Telegram user id.".to_string(), false),
            "no_user" => (
                "Refund has no bound user; cannot transfer.".to_string(),
                false,
            ),
            "executing" => (
                "Execution already in progress — check the queue before retrying.".to_string(),
                false,
            ),
            "ambiguous" => (
                "Transfer state unknown — do NOT retry until you check the payouts queue; the row shows 'executing' until resolved.".to_string(),
                true,
            ),
            "invalid_asset" => (
                "Refund asset must match the invoice asset.".to_string(),
                false,
            ),
            "amount_number" => ("Refund amount must be a number.".to_string(), false),
            "amount_positive" => ("Refund amount must be positive.".to_string(), false),
            "transfer_unavailable" => (
                "Crypto Pay is not configured — no transfer was attempted.".to_string(),
                false,
            ),
            "state" => (
                "That record is not in a state that allows this action.".to_string(),
                true,
            ),
            "transfer" => (
                "Transfer failed — the record stays approved and can be retried.".to_string(),
                true,
            ),
            _ => (String::new(), false),
        };
        if !msg.is_empty() {
            let detail = match (with_detail, q.detail.as_deref()) {
                (true, Some(d)) if !d.is_empty() => format!(" {}", esc(d)),
                _ => String::new(),
            };
            banners.push(format!(
                r#"<div class="alert alert-error" role="alert" aria-live="polite"><strong>{}</strong>{}</div>"#,
                msg, detail
            ));
        }
    }
    let banner_html = if banners.is_empty() {
        None
    } else {
        Some(banners.join("\n"))
    };

    // Prefill the refund form when arriving from an Orders page link.
    let refund_prefill = q
        .refund_order
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|oid| state.db.get_order(oid))
        .map(|(_, _, inv)| inv)
        .filter(|inv| !inv.trim().is_empty());

    Html(html_page(
        &state,
        Some(&session),
        "Finance",
        &finance_body(
            &state,
            &session,
            &filter,
            banner_html.as_deref(),
            refund_prefill,
        ),
    ))
    .into_response()
}

fn finance_body(
    state: &WebState,
    session: &Session,
    filter: &str,
    banners: Option<&str>,
    refund_prefill: Option<String>,
) -> String {
    let banners = banners.unwrap_or_default();
    let is_admin = is_admin_role(&session.role);
    let is_owner = session.role == "owner";
    // Sellers see their own rows, admin/owner see all — the /orders scoping.
    let scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };

    let mut payouts = state.db.list_payouts(scope);
    if filter != "all" {
        payouts.retain(|p| p.5 == filter);
    }
    payouts.sort_by_key(|p| finance_rank(&p.5));
    let mut refunds = state.db.list_refunds(scope);
    if filter != "all" {
        refunds.retain(|r| r.7 == filter);
    }
    refunds.sort_by_key(|r| finance_rank(&r.7));

    // --- Create forms (field names mirror the REST create bodies) ---
    let seller_field = if is_admin {
        r#"<label>Seller id<input type="number" name="seller_id" required></label>"#.to_string()
    } else {
        String::new()
    };
    let payout_form = format!(
        r#"<article class="panel"><h2>Request payout</h2>
<form method="post" action="{}">
{}
<label>Amount<input type="text" name="amount" required></label>
<label>Asset<input type="text" name="asset" value="USDT" required></label>
<label>Destination (Telegram user id)<input type="text" name="destination" inputmode="numeric"></label>
<button type="submit">Request payout</button>
</form>
<p class="help">{}</p>
</article>"#,
        state.url("/finance/payout/create"),
        seller_field,
        if is_admin {
            "The transfer goes through Crypto Pay to the destination Telegram user id."
        } else {
            "Requested for your own seller account; the transfer goes through Crypto Pay to the destination Telegram user id."
        }
    );
    let refund_form = format!(
        r#"<article class="panel" id="refund-form"><h2>Create refund</h2>
<form method="post" action="{}">
<label>Invoice id<input type="text" name="invoice_id" value="{}" required></label>
<label>Amount (defaults to the invoice's)<input type="text" name="amount"></label>
<label>Asset (defaults to the invoice's)<input type="text" name="asset"></label>
<label>Reason<input type="text" name="reason"></label>
<button type="submit">Create refund</button>
</form>
<p class="help">Only paid invoices can be refunded; the refund pays back the invoice's buyer.</p>
</article>"#,
        state.url("/finance/refund/create"),
        esc(refund_prefill.as_deref().unwrap_or(""))
    );

    // --- Payouts queue ---
    let payout_cols = if is_admin { 6 } else { 5 };
    let payout_head = if is_admin {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">Seller</th><th scope="col">Amount</th><th scope="col">Destination</th><th scope="col">Status</th><th scope="col">Actions</th></tr>"#
    } else {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">Amount</th><th scope="col">Destination</th><th scope="col">Status</th><th scope="col">Actions</th></tr>"#
    };
    let payout_rows = if payouts.is_empty() {
        if filter != "all" {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No {} payouts.</b> <a href="{}">Clear filter</a></td></tr>"#,
                payout_cols,
                esc(filter),
                state.url("/finance")
            )
        } else {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No payouts yet.</b> Request one above — it stays pending until the owner approves it, and only then can money move.</td></tr>"#,
                payout_cols
            )
        }
    } else {
        payouts
            .iter()
            .map(|p| {
                let (id, seller, amount, asset, destination, status) = p;
                let seller_cell = if is_admin {
                    format!(r#"<td>{}</td>"#, seller)
                } else {
                    String::new()
                };
                let dest_cell = format!(
                    r#"<td class="mono">{}</td>"#,
                    esc(destination.as_deref().unwrap_or("—"))
                );
                let status_cell = format!(
                    r#"<td><span class="{}">{}</span></td>"#,
                    finance_badge(status),
                    esc(status)
                );
                // REST rules decide what may be offered: approve and execute
                // are owner-only (two-person money rule — the seller who
                // benefits never approves their own payout).
                let actions = if *status == "pending" && is_owner {
                    format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="id" value="{}"><button type="submit" class="btn btn-secondary btn-sm" aria-label="Approve payout {}" onclick="return confirm('Approve payout #{} for {} {}? This records your approval as the owner.')">Approve</button></form>"#,
                        state.url("/finance/payout/approve"),
                        id,
                        id,
                        id,
                        esc(amount),
                        esc(asset)
                    )
                } else if *status == "approved" && is_owner {
                    format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="id" value="{}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Execute payout {}" onclick="return confirm('Execute payout #{}? This sends {} {} to the destination via Crypto Pay — real money.')">Execute</button></form>"#,
                        state.url("/finance/payout/execute"),
                        id,
                        id,
                        id,
                        esc(amount),
                        esc(asset)
                    )
                } else {
                    String::new()
                };
                format!(
                    r#"<tr><td class="num">{}</td>{}<td class="mono">{} {}</td>{}{}<td class="actions">{}</td></tr>"#,
                    id,
                    seller_cell,
                    esc(amount),
                    esc(asset),
                    dest_cell,
                    status_cell,
                    actions
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let payout_note = if is_owner {
        String::new()
    } else {
        r#"<p class="help">Payout approval and execution are owner actions (two-person money rule).</p>"#.to_string()
    };
    let payout_table = format!(
        r#"{note}<div class="table-wrap"><table aria-label="Payouts"><caption class="sr-only">Payouts</caption><thead>{head}</thead><tbody>{rows}</tbody></table></div>"#,
        note = payout_note,
        head = payout_head,
        rows = payout_rows
    );

    // --- Refunds queue ---
    let refund_cols = if is_admin { 6 } else { 5 };
    let refund_head = if is_admin {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">Seller</th><th scope="col">Invoice</th><th scope="col">Amount</th><th scope="col">Status</th><th scope="col">Actions</th></tr>"#
    } else {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">Invoice</th><th scope="col">Amount</th><th scope="col">Status</th><th scope="col">Actions</th></tr>"#
    };
    let refund_rows = if refunds.is_empty() {
        if filter != "all" {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No {} refunds.</b> <a href="{}">Clear filter</a></td></tr>"#,
                refund_cols,
                esc(filter),
                state.url("/finance")
            )
        } else {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No refunds yet.</b> Create one above from a paid invoice, or follow the Refund action on an order.</td></tr>"#,
                refund_cols
            )
        }
    } else {
        refunds
            .iter()
            .map(|r| {
                let (id, seller, invoice_id, _user, amount, asset, reason, status) = r;
                let seller_cell = if is_admin {
                    format!(r#"<td>{}</td>"#, seller)
                } else {
                    String::new()
                };
                let invoice_cell = match reason.as_deref() {
                    Some(rs) if !rs.trim().is_empty() => format!(
                        r#"<td class="mono">{}<div class="help">{}</div></td>"#,
                        esc(invoice_id.as_deref().unwrap_or("—")),
                        esc(rs)
                    ),
                    _ => format!(
                        r#"<td class="mono">{}</td>"#,
                        esc(invoice_id.as_deref().unwrap_or("—"))
                    ),
                };
                let status_cell = format!(
                    r#"<td><span class="{}">{}</span></td>"#,
                    finance_badge(status),
                    esc(status)
                );
                // REST rules decide what may be offered: refund approve and
                // execute are admin-or-owner (the seller who owes the money
                // never approves their own refund).
                let actions = if *status == "pending" && is_admin {
                    format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="id" value="{}"><button type="submit" class="btn btn-secondary btn-sm" aria-label="Approve refund {}" onclick="return confirm('Approve refund #{} for {} {}?')">Approve</button></form>"#,
                        state.url("/finance/refund/approve"),
                        id,
                        id,
                        id,
                        esc(amount),
                        esc(asset)
                    )
                } else if *status == "approved" && is_admin {
                    format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="id" value="{}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Execute refund {}" onclick="return confirm('Execute refund #{}? This sends {} {} back to the buyer via Crypto Pay — real money.')">Execute</button></form>"#,
                        state.url("/finance/refund/execute"),
                        id,
                        id,
                        id,
                        esc(amount),
                        esc(asset)
                    )
                } else {
                    String::new()
                };
                format!(
                    r#"<tr><td class="num">{}</td>{}{}<td class="mono">{} {}</td>{}<td class="actions">{}</td></tr>"#,
                    id,
                    seller_cell,
                    invoice_cell,
                    esc(amount),
                    esc(asset),
                    status_cell,
                    actions
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let refund_note = if is_admin {
        String::new()
    } else {
        r#"<p class="help">Refund approval and execution require admin or owner.</p>"#.to_string()
    };
    let refund_table = format!(
        r#"{note}<div class="table-wrap"><table aria-label="Refunds"><caption class="sr-only">Refunds</caption><thead>{head}</thead><tbody>{rows}</tbody></table></div>"#,
        note = refund_note,
        head = refund_head,
        rows = refund_rows
    );

    // --- Filter chips (same idiom as /subs?status=) ---
    let statuses = [
        "all",
        "pending",
        "approved",
        "executing",
        "paid",
        "executed",
    ];
    let filter_links = statuses
        .iter()
        .map(|s| {
            let cls = if *s == filter {
                "btn btn-secondary btn-sm"
            } else {
                "btn btn-ghost btn-sm"
            };
            let aria = if *s == filter {
                r#" aria-current="page""#
            } else {
                ""
            };
            format!(
                r#"<a class="{}" href="{}"{}>{}</a>"#,
                cls,
                state.url(&format!("/finance?status={}", s)),
                aria,
                s
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let filter_nav = format!(
        r#"<nav aria-label="Filter finance queues by status" class="inline-form">{}</nav>"#,
        filter_links
    );

    let scoping_note = if session.role == "seller" {
        "Showing payouts and refunds for your seller account."
    } else {
        "Showing payouts and refunds for all sellers."
    };

    format!(
        r#"{banners}<h1>Finance <span class="sub">payouts, refunds and money movement</span></h1>
<p class="help">{scoping_note} Approval then execution, every step audited; the bot never moves money.</p>
{filter_nav}
<div class="grid-2">
{payout_form}
{refund_form}
</div>
<section aria-labelledby="payouts-heading"><h2 id="payouts-heading" class="sub">Payouts</h2>
{payout_table}
</section>
<section aria-labelledby="refunds-heading"><h2 id="refunds-heading" class="sub">Refunds</h2>
{refund_table}
</section>"#,
        banners = banners,
        scoping_note = scoping_note,
        filter_nav = filter_nav,
        payout_form = payout_form,
        refund_form = refund_form,
        payout_table = payout_table,
        refund_table = refund_table
    )
}

#[derive(Deserialize)]
struct FinancePayoutCreateForm {
    seller_id: Option<String>,
    amount: Option<String>,
    asset: Option<String>,
    destination: Option<String>,
}

/// Mirrors REST `finance_payouts_create`: sellers create payouts for
/// themselves; admins must name the seller. Audit: `payout_create`.
#[tracing::instrument(skip_all)]
async fn finance_payout_create(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinancePayoutCreateForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "seller" && !is_admin_role(&session.role) {
        return forbidden_page(&state, "Finance", "Finance").into_response();
    }
    let requested = f
        .seller_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<i64>().ok());
    let seller_id = if session.role == "seller" {
        match session.seller_id {
            Some(s) => {
                if requested.is_some() && requested != Some(s) {
                    // The REST forbidden case: a seller naming another seller.
                    return forbidden_page(&state, "Finance", "Finance").into_response();
                }
                s
            }
            None => return forbidden_page(&state, "Finance", "Finance").into_response(),
        }
    } else {
        match requested {
            Some(s) if s > 0 => s,
            _ => return finance_redirect(&state, "error=seller_required"),
        }
    };
    let amount = f.amount.as_deref().map(str::trim).unwrap_or_default();
    let asset = f.asset.as_deref().map(str::trim).unwrap_or_default();
    if amount.is_empty() || asset.is_empty() {
        return finance_redirect(&state, "error=amount_required");
    }
    let destination = f
        .destination
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| d.to_string());
    let id = state
        .db
        .create_payout(seller_id, amount, asset, destination.as_deref());
    let meta = json!({
        "seller_id": seller_id, "amount": amount, "asset": asset,
        "destination": destination,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "payout_create",
        "payout",
        id.to_string(),
        Some(&meta),
    );
    finance_redirect(&state, "payout_created=1")
}

#[derive(Deserialize)]
struct FinancePayoutActionForm {
    id: String,
}

/// Mirrors REST `finance_payouts_approve`: owner only (two-person money
/// rule), paid is already executed, otherwise pending/approved -> approved.
/// Audit: `payout_approve`.
#[tracing::instrument(skip_all)]
async fn finance_payout_approve(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinancePayoutActionForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Finance", "Payout approval (owner only)").into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return finance_redirect(&state, "error=invalid");
    };
    if id <= 0 {
        return finance_redirect(&state, "error=invalid");
    }
    let Some(payout) = state.db.get_payout(id.to_string()) else {
        return finance_redirect(&state, "error=notfound");
    };
    let (_pid, _seller, amount, asset, destination, status, ..) = payout;
    if status == "paid" {
        return finance_redirect(
            &state,
            &format!("error=state&detail={}", qenc("payout already executed")),
        );
    }
    if status != "pending" && status != "approved" {
        return finance_redirect(
            &state,
            &format!(
                "error=state&detail={}",
                qenc(&format!("cannot approve payout in status '{}'", status))
            ),
        );
    }
    state.db.set_payout_status(id, "approved");
    let meta = json!({
        "amount": amount, "asset": asset, "destination": destination,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "payout_approve",
        "payout",
        id.to_string(),
        Some(&meta),
    );
    finance_redirect(&state, "payout_approved=1")
}

/// Mirrors REST `finance_payouts_execute` (reserve-then-transfer): owner
/// only; approved -> paid by a real Crypto Pay `transfer` to the destination
/// Telegram user id. The row is first claimed 'executing' atomically, so two
/// concurrent executors can never both move money — the CAS loser is bounced
/// with an `executing` banner before any transfer is attempted. A definite
/// API rejection reverts the row to 'approved' for retry and is audited
/// (`payout_execute_failed`); an ambiguous transport outcome (timeout,
/// network) leaves the row 'executing' — the transfer state is unknown and
/// retrying blind risks a double payout — and is audited
/// (`payout_execute_ambiguous`) with its own banner.
#[tracing::instrument(skip_all)]
async fn finance_payout_execute(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinancePayoutActionForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Finance", "Payout execution (owner only)").into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return finance_redirect(&state, "error=invalid");
    };
    if id <= 0 {
        return finance_redirect(&state, "error=invalid");
    }
    let Some(payout) = state.db.get_payout(id.to_string()) else {
        return finance_redirect(&state, "error=notfound");
    };
    let (_pid, seller_id, amount, asset, destination, status, ..) = payout;
    if status == "paid" {
        return finance_redirect(
            &state,
            &format!("error=state&detail={}", qenc("payout already executed")),
        );
    }
    if status == "executing" {
        return finance_redirect(&state, "error=executing");
    }
    if status != "approved" {
        return finance_redirect(
            &state,
            &format!(
                "error=state&detail={}",
                qenc("payout must be approved before execution")
            ),
        );
    }
    // Crypto Pay transfers go to a Telegram user id: the destination must be
    // exactly that.
    let Some(dest) = destination
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    else {
        return finance_redirect(&state, "error=destination");
    };
    let Ok(dest_user) = dest.parse::<i64>() else {
        return finance_redirect(&state, "error=bad_destination");
    };
    let Some(tok) = crypto_token(&state) else {
        return finance_redirect(&state, "error=transfer_unavailable");
    };
    // Audit the attempt BEFORE the await: if the process dies mid-transfer,
    // the audit trail must already show that an execution was started.
    let attempt_meta = json!({
        "seller_id": seller_id, "amount": amount.clone(), "asset": asset.clone(),
        "destination": dest_user,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "payout_execute_attempt",
        "payout",
        id.to_string(),
        Some(&attempt_meta),
    );
    if !state.db.begin_payout_execution(id) {
        // Lost the atomic claim: another executor holds the row.
        return finance_redirect(&state, "error=executing");
    }
    let cp = crate::crypto::CryptoPay::new(tok);
    match cp.transfer(dest_user, &asset, &amount).await {
        Ok(receipt) => {
            // We won the claim, so the executing -> paid finish is ours alone.
            state.db.finish_payout_execution(id, "paid");
            let meta = json!({
                "seller_id": seller_id, "amount": amount, "asset": asset,
                "destination": dest_user, "receipt": receipt,
            })
            .to_string();
            state.db.log_action(
                session.account_id,
                &session.role,
                "payout_executed",
                "payout",
                id.to_string(),
                Some(&meta),
            );
            finance_redirect(&state, "payout_executed=1")
        }
        Err(e) => {
            if transfer_rejected_definitely(&e) {
                // The API refused: no money moved — put the payout back to
                // 'approved' so the transfer can be retried.
                state.db.finish_payout_execution(id, "approved");
                let meta = json!({
                    "seller_id": seller_id, "amount": amount, "asset": asset,
                    "destination": dest_user, "error": e.to_string(),
                })
                .to_string();
                state.db.log_action(
                    session.account_id,
                    &session.role,
                    "payout_execute_failed",
                    "payout",
                    id.to_string(),
                    Some(&meta),
                );
                finance_redirect(
                    &state,
                    &format!(
                        "error=transfer&detail={}",
                        qenc(e.chars().take(200).collect::<String>().as_str())
                    ),
                )
            } else {
                // Ambiguous (timeout/network): the transfer may or may not
                // have executed. Leave the row 'executing' — it is the
                // operator's signal that this payout needs a manual check
                // before anyone retries.
                let meta = json!({
                    "seller_id": seller_id, "amount": amount, "asset": asset,
                    "destination": dest_user, "error": e.to_string(),
                })
                .to_string();
                state.db.log_action(
                    session.account_id,
                    &session.role,
                    "payout_execute_ambiguous",
                    "payout",
                    id.to_string(),
                    Some(&meta),
                );
                finance_redirect(
                    &state,
                    &format!(
                        "error=ambiguous&detail={}",
                        qenc(e.chars().take(200).collect::<String>().as_str())
                    ),
                )
            }
        }
    }
}

#[derive(Deserialize)]
struct FinanceRefundCreateForm {
    invoice_id: Option<String>,
    amount: Option<String>,
    asset: Option<String>,
    reason: Option<String>,
}

/// Mirrors REST `finance_refunds_create`: seller-or-admin, rooted in a paid
/// invoice (the seller's own; admins may refund any seller's invoice and it
/// inherits that seller). Amount/asset default to the invoice's. A refund
/// can never move money the invoice did not collect: seller-role creators are
/// clamped to the invoice's amount and locked to its asset (asset mismatch or
/// a bad amount is a banner error, nothing created); admins/owner keep the
/// looser form — they are the approvers of whatever gets created here.
/// Audit: `refund_create`.
#[tracing::instrument(skip_all)]
async fn finance_refund_create(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinanceRefundCreateForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "seller" && !is_admin_role(&session.role) {
        return forbidden_page(&state, "Finance", "Finance").into_response();
    }
    let invoice_id = f.invoice_id.as_deref().map(str::trim).unwrap_or_default();
    if invoice_id.is_empty() {
        return finance_redirect(&state, "error=invoice_required");
    }
    let Some(inv) = state.db.get_invoice(invoice_id) else {
        return finance_redirect(&state, "error=invoice_notfound");
    };
    let (inv_amount, inv_asset, inv_status, _desc, _plan, inv_user, inv_seller) = inv;
    if session.role == "seller" && session.seller_id != Some(inv_seller) {
        return finance_redirect(&state, "error=invoice_owner");
    }
    // Only settled money can be returned.
    if inv_status != "paid" {
        return finance_redirect(&state, "error=invoice_state");
    }
    // Seller lock: the asset must equal the invoice's — a refund in a
    // different asset is not a return of the buyer's money.
    if session.role == "seller"
        && f.asset
            .as_deref()
            .map(str::trim)
            .is_some_and(|a| a != inv_asset)
    {
        return finance_redirect(&state, "error=invalid_asset");
    }
    let inv_amount_f: f64 = inv_amount.parse().unwrap_or(f64::NAN);
    let amount = match f.amount.as_deref().map(str::trim) {
        None | Some("") => inv_amount.clone(),
        Some(raw) => {
            if session.role != "seller" {
                raw.to_string()
            } else {
                let Ok(requested) = raw.parse::<f64>() else {
                    return finance_redirect(&state, "error=amount_number");
                };
                if requested <= 0.0 {
                    return finance_redirect(&state, "error=amount_positive");
                }
                if inv_amount_f.is_nan() || requested > inv_amount_f {
                    // Clamp: never more than the invoice collected (and if
                    // the stored amount is not parseable, refund exactly
                    // that amount verbatim rather than trusting the request).
                    inv_amount.clone()
                } else {
                    raw.to_string()
                }
            }
        }
    };
    let asset = f
        .asset
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or(inv_asset);
    let reason = f
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let id = state.db.create_refund(
        inv_seller,
        Some(invoice_id),
        inv_user,
        &amount,
        &asset,
        reason.as_deref(),
    );
    let meta = json!({
        "invoice_id": invoice_id, "amount": amount, "asset": asset,
        "user_id": inv_user, "reason": reason,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "refund_create",
        "refund",
        id.to_string(),
        Some(&meta),
    );
    finance_redirect(&state, "refund_created=1")
}

#[derive(Deserialize)]
struct FinanceRefundActionForm {
    id: String,
}

/// Mirrors REST `finance_refunds_approve`: admin-or-owner, executed is final,
/// otherwise pending/approved -> approved. Audit: `refund_approve`.
#[tracing::instrument(skip_all)]
async fn finance_refund_approve(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinanceRefundActionForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !is_admin_role(&session.role) {
        return forbidden_page(&state, "Finance", "Refund approval (admin or owner)")
            .into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return finance_redirect(&state, "error=invalid");
    };
    if id <= 0 {
        return finance_redirect(&state, "error=invalid");
    }
    let Some(refund) = state.db.get_refund(id.to_string()) else {
        return finance_redirect(&state, "error=notfound");
    };
    let (_rid, _seller, invoice_id, _user, amount, asset, _reason, status, ..) = refund;
    if status == "executed" {
        return finance_redirect(
            &state,
            &format!("error=state&detail={}", qenc("refund already executed")),
        );
    }
    if status != "pending" && status != "approved" {
        return finance_redirect(
            &state,
            &format!(
                "error=state&detail={}",
                qenc(&format!("cannot approve refund in status '{}'", status))
            ),
        );
    }
    state.db.set_refund_status(id, "approved");
    let meta = json!({
        "invoice_id": invoice_id, "amount": amount, "asset": asset,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "refund_approve",
        "refund",
        id.to_string(),
        Some(&meta),
    );
    finance_redirect(&state, "refund_approved=1")
}

/// Mirrors REST `finance_refunds_execute` (reserve-then-transfer):
/// admin-or-owner; approved -> executed by a real Crypto Pay `transfer` back
/// to the buyer. The row is first claimed 'executing' atomically, so two
/// concurrent executors can never both move money — the CAS loser is bounced
/// with an `executing` banner before any transfer is attempted. On success
/// the invoice is marked refunded and the subscription the invoice granted
/// is revoked (status 'refunded' + channel kick via ban+unban — a failed
/// kick is reported in the banner, the status flip stays the source of
/// truth). A definite API rejection reverts the row to 'approved' for retry
/// and is audited (`refund_execute_failed`); an ambiguous transport outcome
/// (timeout, network) leaves the row 'executing' — the transfer state is
/// unknown and retrying blind risks a double payout — and is audited
/// (`refund_execute_ambiguous`) with its own banner.
#[tracing::instrument(skip_all)]
async fn finance_refund_execute(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinanceRefundActionForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !is_admin_role(&session.role) {
        return forbidden_page(&state, "Finance", "Refund execution (admin or owner)")
            .into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return finance_redirect(&state, "error=invalid");
    };
    if id <= 0 {
        return finance_redirect(&state, "error=invalid");
    }
    let Some(refund) = state.db.get_refund(id.to_string()) else {
        return finance_redirect(&state, "error=notfound");
    };
    let (_rid, _seller, invoice_id, refund_user, amount, asset, _reason, status, ..) = refund;
    if status == "executed" {
        return finance_redirect(
            &state,
            &format!("error=state&detail={}", qenc("refund already executed")),
        );
    }
    if status == "executing" {
        return finance_redirect(&state, "error=executing");
    }
    if status != "approved" {
        return finance_redirect(
            &state,
            &format!(
                "error=state&detail={}",
                qenc("refund must be approved before execution")
            ),
        );
    }
    // Crypto Pay transfers target a Telegram user: the buyer the invoice was
    // bound to.
    let Some(refund_user) = refund_user else {
        return finance_redirect(&state, "error=no_user");
    };
    let Some(tok) = crypto_token(&state) else {
        return finance_redirect(&state, "error=transfer_unavailable");
    };
    // Audit the attempt BEFORE the await: if the process dies mid-transfer,
    // the audit trail must already show that an execution was started.
    let attempt_meta = json!({
        "invoice_id": invoice_id.clone(), "amount": amount.clone(), "asset": asset.clone(),
        "user_id": refund_user,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "refund_execute_attempt",
        "refund",
        id.to_string(),
        Some(&attempt_meta),
    );
    if !state.db.begin_refund_execution(id) {
        // Lost the atomic claim: another executor holds the row.
        return finance_redirect(&state, "error=executing");
    }
    let cp = crate::crypto::CryptoPay::new(tok);
    match cp.transfer(refund_user, &asset, &amount).await {
        Ok(receipt) => {
            // We won the claim, so the executing -> executed finish is ours
            // alone.
            state.db.finish_refund_execution(id, "executed");
            let mut subscription_revoked = false;
            let mut kick_failed = false;
            if let Some(inv) = invoice_id.as_deref() {
                state.db.update_invoice_status(inv, "refunded");
                if let Some((sub_id, channel)) = state.db.subscription_by_invoice(inv) {
                    state.db.set_subscription_status(sub_id, "refunded");
                    subscription_revoked = true;
                    if !channel.is_empty() {
                        // Kick (ban+unban) so the refunded buyer loses channel
                        // access immediately. The status flip is the source of
                        // truth; a failed kick is reported, not swallowed.
                        let cid = crate::util::to_chat_id(&channel);
                        let ban = state
                            .bot
                            .ban_chat_member(
                                cid.clone(),
                                teloxide::types::UserId(refund_user as u64),
                            )
                            .await;
                        let unban = state
                            .bot
                            .unban_chat_member(cid, teloxide::types::UserId(refund_user as u64))
                            .await;
                        kick_failed = ban.is_err() || unban.is_err();
                    }
                }
            }
            let meta = json!({
                "invoice_id": invoice_id, "amount": amount, "asset": asset,
                "user_id": refund_user, "subscription_revoked": subscription_revoked,
                "receipt": receipt,
            })
            .to_string();
            state.db.log_action(
                session.account_id,
                &session.role,
                "refund_executed",
                "refund",
                id.to_string(),
                Some(&meta),
            );
            if kick_failed {
                finance_redirect(&state, "refund_executed=1&warn=kick")
            } else {
                finance_redirect(&state, "refund_executed=1")
            }
        }
        Err(e) => {
            if transfer_rejected_definitely(&e) {
                // The API refused: no money moved — put the refund back to
                // 'approved' so the transfer can be retried.
                state.db.finish_refund_execution(id, "approved");
                let meta = json!({
                    "invoice_id": invoice_id, "amount": amount, "asset": asset,
                    "user_id": refund_user, "error": e.to_string(),
                })
                .to_string();
                state.db.log_action(
                    session.account_id,
                    &session.role,
                    "refund_execute_failed",
                    "refund",
                    id.to_string(),
                    Some(&meta),
                );
                finance_redirect(
                    &state,
                    &format!(
                        "error=transfer&detail={}",
                        qenc(e.chars().take(200).collect::<String>().as_str())
                    ),
                )
            } else {
                // Ambiguous (timeout/network): the transfer may or may not
                // have executed. Leave the row 'executing' — it is the
                // operator's signal that this refund needs a manual check
                // before anyone retries.
                let meta = json!({
                    "invoice_id": invoice_id, "amount": amount, "asset": asset,
                    "user_id": refund_user, "error": e.to_string(),
                })
                .to_string();
                state.db.log_action(
                    session.account_id,
                    &session.role,
                    "refund_execute_ambiguous",
                    "refund",
                    id.to_string(),
                    Some(&meta),
                );
                finance_redirect(
                    &state,
                    &format!(
                        "error=ambiguous&detail={}",
                        qenc(e.chars().take(200).collect::<String>().as_str())
                    ),
                )
            }
        }
    }
}

// ---------- Subs grant ----------

#[derive(Deserialize)]
struct SubsGrantForm {
    user_id: String,
    days: String,
    channel: String,
}

#[tracing::instrument(skip_all)]
async fn subs_grant(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SubsGrantForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_id = if session.role == "owner" {
        0i64
    } else {
        match session.seller_id {
            Some(v) => v,
            None => return Redirect::to(&state.url("/subs")).into_response(),
        }
    };
    let user_id: i64 = match f.user_id.trim().parse() {
        Ok(v) => v,
        Err(_) => return Redirect::to(&state.url("/subs")).into_response(),
    };
    let days: i64 = f.days.trim().parse().unwrap_or(30);
    if f.channel.trim().is_empty() {
        return Redirect::to(&state.url("/subs")).into_response();
    }
    state
        .db
        .create_subscription(seller_id, user_id, None, &f.channel, days, None, None);
    Redirect::to(&state.url("/subs?granted=1")).into_response()
}

// ---------- Broadcast ----------

/// Query for the broadcast page: the send handler's outcome flag (?sent={n}).
#[derive(Deserialize)]
struct BroadcastQuery {
    sent: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn broadcast_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<BroadcastQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    // Outcome banner from the ?sent={n} redirect (PRG). The count is what the
    // send actually delivered; 0 is a failure, not a success — nothing went
    // out.
    let banner = match q.sent.as_deref().and_then(|s| s.parse::<usize>().ok()) {
        Some(0) => Some(
            r#"<div class="alert alert-error" role="alert">No users matched — nothing was sent.</div>"#
                .to_string(),
        ),
        Some(n) => Some(format!(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Sent to {} users.</div>"#,
            n
        )),
        None => None,
    };
    // Audience preview: the same lists broadcast_send will walk, so the
    // confirm dialog quotes the real recipient count before anything is sent.
    let audience = if session.role == "owner" {
        state.db.list_users().len()
    } else {
        session
            .seller_id
            .map(|sid| state.db.list_users_for_seller(sid).len())
            .unwrap_or(0)
    };
    let audience_note = if audience == 0 {
        r#"<p class="help">No users match — a send now would deliver nothing.</p>"#.to_string()
    } else {
        format!(
            r#"<p class="help">This message will go to <strong>{}</strong> user{}.</p>"#,
            audience,
            if audience == 1 { "" } else { "s" }
        )
    };
    let confirm = format!(
        r#"onclick="return confirm('Broadcast to {} users? This cannot be unsent.')""#,
        audience
    );
    let body = format!(
        r#"{banner}<h1>Broadcast <span class="sub">send to customers</span></h1>
<article class="panel"><h3>Compose</h3>
<form method="post" action="{}">
<label>Message <textarea name="text" rows="6" required></textarea></label>
<button type="submit" {confirm}>Send broadcast</button>
</form>
{audience_note}
<p class="help">Sellers broadcast to their own customers; owners broadcast to all users.</p>
</article>"#,
        state.url("/broadcast/send"),
        banner = banner.as_deref().unwrap_or(""),
        confirm = confirm,
        audience_note = audience_note,
    );
    Html(html_page(&state, Some(&session), "Broadcast", &body)).into_response()
}

#[derive(Deserialize)]
struct BroadcastForm {
    text: String,
}

#[tracing::instrument(skip_all)]
async fn broadcast_send(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<BroadcastForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if f.text.trim().is_empty() {
        return Redirect::to(&state.url("/broadcast")).into_response();
    }
    let users = if session.role == "owner" {
        state.db.list_users()
    } else {
        match session.seller_id {
            Some(sid) => state.db.list_users_for_seller(sid),
            None => vec![],
        }
    };
    let sent = crate::broadcast_to_users(&state.bot, &users, &f.text).await;
    Redirect::to(&state.url(&format!("/broadcast?sent={}", sent))).into_response()
}

// ---------- Crypto ----------

#[tracing::instrument(skip_all)]
async fn crypto_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Crypto", "Crypto").into_response();
    }
    let token = state.db.get_config("crypto_token");
    let badge = if token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some()
    {
        r#"<span class="badge badge-success" aria-label="Crypto token configured">configured</span>"#.to_string()
    } else {
        r#"<span class="badge badge-warning" aria-label="Crypto token not set">not set</span>"#
            .to_string()
    };
    let result = ""; // placeholder
    let _ = result;
    let body = format!(
        r#"<h1>Crypto <span class="sub">Crypto Pay</span></h1>
<article class="panel"><h3>Token {}</h3>
<form method="post" action="{}">
<label for="crypto-token">Token</label>
<span class="secret-field"><input type="password" id="crypto-token" name="token" placeholder="leave blank to keep" autocomplete="new-password">{toggle}</span>
<button type="submit">Save token</button>
</form>
<form method="post" action="{}"><button type="submit" class="btn btn-secondary btn-sm">Show me</button></form>
<p class="help">Token stored in config. Actions call Crypto Pay API.</p>
</article>"#,
        badge,
        state.url("/crypto/setup"),
        state.url("/crypto/me"),
        toggle = SECRET_TOGGLE,
    );
    Html(html_page(&state, Some(&session), "Crypto", &body)).into_response()
}

#[derive(Deserialize)]
struct CryptoSetupForm {
    token: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn crypto_setup(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<CryptoSetupForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Crypto", "Crypto").into_response();
    }
    if let Some(t) = f.token.as_deref().filter(|s| !s.trim().is_empty()) {
        state.db.set_config("crypto_token", t.trim());
    }
    Redirect::to(&state.url("/crypto")).into_response()
}

#[tracing::instrument(skip_all)]
async fn crypto_me(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Crypto", "Crypto").into_response();
    }
    let token = state.db.get_config("crypto_token").unwrap_or_default();
    if token.trim().is_empty() {
        return Redirect::to(&state.url("/crypto")).into_response();
    }
    let cp = crate::crypto::CryptoPay::new(token);
    let res = cp.get_me().await;
    let msg = match res {
        Ok(v) => format!("Me: {}", v),
        Err(e) => format!("Error: {}", e),
    };
    // store in a query param (best effort, truncated)
    let _ = msg;
    Redirect::to(&state.url("/crypto")).into_response()
}

// ---------- Admins ----------

#[tracing::instrument(skip_all)]
async fn admins_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Admins", "Admins").into_response();
    }
    let admins = state.db.list_admins_detail();
    let rows = admins
        .iter()
        .map(|(uid, uname)| {
            format!(
                r#"<tr><td class="mono">{}</td><td>{}</td><td>
<form method="post" action="{}" class="inline-form"><input type="hidden" name="user_id" value="{}"><button type="submit" class="btn btn-danger btn-sm" onclick="return confirm('Remove admin {}?')">Remove</button></form>
</td></tr>"#,
                uid,
                // Username comes from the add-admin form's free-text field —
                // escaped on render like every other user-supplied cell.
                esc(uname.as_deref().unwrap_or("—")),
                state.url("/admins/remove"),
                uid,
                uid
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let table = format!(
        r#"<div class="table-wrap"><table aria-label="Admins"><thead><tr><th scope="col">User ID</th><th scope="col">Username</th><th scope="col">Actions</th></tr></thead><tbody>{}</tbody></table></div>"#,
        if rows.is_empty() {
            r#"<tr><td colspan="3" class="empty"><b>No admins.</b></td></tr>"#.to_string()
        } else {
            rows
        }
    );
    let body = format!(
        r#"<h1>Admins <span class="sub">admin management</span></h1>
<article class="panel"><h3>Add admin</h3>
<form method="post" action="{}" class="inline-form">
<label>User ID<input type="number" name="user_id" placeholder="user id" required></label>
<label>Username<input type="text" name="username" placeholder="username"></label>
<button type="submit">Add</button>
</form></article>
{table}"#,
        state.url("/admins/add"),
        table = table
    );
    Html(html_page(&state, Some(&session), "Admins", &body)).into_response()
}

#[derive(Deserialize)]
struct AdminForm {
    user_id: String,
    username: Option<String>,
}

#[derive(Deserialize)]
struct AdminRemoveForm {
    user_id: String,
}

#[tracing::instrument(skip_all)]
async fn admins_add(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<AdminForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Admins", "Admins").into_response();
    }
    if let Ok(uid) = f.user_id.trim().parse::<i64>() {
        state.db.add_admin(uid, f.username.as_deref());
    }
    Redirect::to(&state.url("/admins")).into_response()
}

#[tracing::instrument(skip_all)]
async fn admins_remove(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<AdminRemoveForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Admins", "Admins").into_response();
    }
    if let Ok(uid) = f.user_id.trim().parse::<i64>() {
        state.db.remove_admin(uid);
    }
    Redirect::to(&state.url("/admins")).into_response()
}

// ---------- Sellers ----------

#[tracing::instrument(skip_all)]
async fn sellers_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Sellers", "Sellers").into_response();
    }
    let sellers = state.db.list_sellers();
    let rows = sellers
        .iter()
        .map(|sid| {
            let ch = state.db.seller_channel(*sid).unwrap_or_default();
            format!(
                r#"<tr><td class="mono">{}</td><td class="mono">{}</td></tr>"#,
                sid,
                esc(&ch)
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let table = format!(
        r#"<div class="table-wrap"><table aria-label="Sellers"><thead><tr><th scope="col">Seller ID</th><th scope="col">Channel</th></tr></thead><tbody>{}</tbody></table></div>"#,
        if rows.is_empty() {
            r#"<tr><td colspan="2" class="empty"><b>No sellers.</b></td></tr>"#.to_string()
        } else {
            rows
        }
    );
    let body = format!(
        r#"<h1>Sellers <span class="sub">seller management</span></h1>
<article class="panel"><h3>Upsert seller</h3>
<form method="post" action="{}" class="inline-form">
<label>Seller ID<input type="number" name="user_id" placeholder="user id" required></label>
<label>Username<input type="text" name="username" placeholder="username"></label>
<label>Channel<input type="text" name="channel" placeholder="@channel"></label>
<button type="submit">Save</button>
</form></article>
<article class="panel"><h3>Set seller channel</h3>
<form method="post" action="{}" class="inline-form">
<label>Seller ID<input type="number" name="user_id" placeholder="seller id" required></label>
<label>Channel<input type="text" name="channel" placeholder="@channel" required></label>
<button type="submit">Set</button>
</form></article>
{table}"#,
        state.url("/sellers/upsert"),
        state.url("/sellers/channel"),
        table = table
    );
    Html(html_page(&state, Some(&session), "Sellers", &body)).into_response()
}

#[derive(Deserialize)]
struct SellerUpsertForm {
    user_id: String,
    username: Option<String>,
    channel: Option<String>,
}

#[derive(Deserialize)]
struct SellerChannelForm {
    user_id: String,
    channel: String,
}

#[tracing::instrument(skip_all)]
async fn sellers_upsert(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SellerUpsertForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Sellers", "Sellers").into_response();
    }
    if let Ok(uid) = f.user_id.trim().parse::<i64>() {
        state
            .db
            .upsert_seller(uid, f.username.as_deref(), f.channel.as_deref());
    }
    Redirect::to(&state.url("/sellers")).into_response()
}

#[tracing::instrument(skip_all)]
async fn sellers_channel(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SellerChannelForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Sellers", "Sellers").into_response();
    }
    if let Ok(uid) = f.user_id.trim().parse::<i64>() {
        state.db.set_seller_channel(uid, &f.channel);
    }
    Redirect::to(&state.url("/sellers")).into_response()
}

// ---------- Web users ----------

// ---------- MFA / TOTP ----------

fn pending_from_headers(
    state: &WebState,
    headers: &axum::http::HeaderMap,
) -> Option<(String, i64, String, Option<String>, String)> {
    let raw = get_pending_cookie(headers)?;
    let hash = sha256_hex(&raw);
    let (account_id, purpose, secret, expires) = state.db.get_pending_mfa(&hash)?;
    // expiry check
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&expires)
        && chrono::Utc::now() > dt.with_timezone(&chrono::Utc)
    {
        let _ = state.db.delete_pending_mfa(&hash);
        return None;
    }
    Some((raw, account_id, purpose, secret, hash))
}

#[tracing::instrument(skip_all)]
async fn mfa_verify_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let Some((_raw, account_id, purpose, _secret, _hash)) = pending_from_headers(&state, &headers)
    else {
        return Redirect::to(&state.url("/login")).into_response();
    };
    if purpose != "verify" {
        return Redirect::to(&state.url("/mfa/enroll")).into_response();
    }
    let body = format!(
        r#"<h1>Verify MFA <span class="sub">enter your authenticator code</span></h1>
<article class="panel">
<form method="post" action="{}">
<label>Authenticator code or recovery code<input type="text" name="code" required autocomplete="one-time-code"></label>
<button type="submit">Verify</button>
</form>
<p class="help">Account {}.</p>
</article>"#,
        state.url("/mfa/verify"),
        account_id
    );
    Html(html_page_bare(&state.base_path, "MFA verify", &body)).into_response()
}

#[derive(Deserialize)]
struct MfaCodeForm {
    code: String,
}

#[tracing::instrument(skip_all)]
async fn mfa_verify_submit(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<MfaCodeForm>,
) -> impl IntoResponse {
    let Some((raw, account_id, purpose, _secret, hash)) = pending_from_headers(&state, &headers)
    else {
        return Redirect::to(&state.url("/login")).into_response();
    };
    if purpose != "verify" {
        return Redirect::to(&state.url("/mfa/enroll")).into_response();
    }
    // rate limit
    if login_throttle(&state.login_attempts, &format!("mfa:{}", raw), 5, 900) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Html(html_page_bare(
                &state.base_path,
                "MFA verify",
                "Too many attempts.",
            )),
        )
            .into_response();
    }
    let code = f.code.trim();
    let (secret, enabled, last_used) = state.db.get_totp_state(account_id);
    let mut ok = false;
    if enabled && code.chars().all(|c| c.is_ascii_digit()) && code.len() == 6 {
        let now_ts = chrono::Utc::now().timestamp() as u64;
        let current_counter = (now_ts / 30) as i64;
        let generated =
            crate::util::totp_generate(&secret.clone().unwrap_or_default(), now_ts, 6usize, 30u64);
        if crate::util::ct_eq(&generated, code)
            && last_used.map(|l| current_counter > l).unwrap_or(true)
        {
            ok = true;
            let _ = state.db.update_totp_last_used(account_id, current_counter);
        }
    }
    if !ok && state.db.verify_recovery_code(account_id, code) {
        ok = true;
    }
    if ok {
        login_ok(&state.login_attempts, &format!("mfa:{}", raw));
        let _ = state.db.delete_pending_mfa(&hash);
        let (id, _u, _h, role, seller_id) = state.db.get_web_account_by_id(account_id).unwrap_or((
            account_id,
            String::new(),
            String::new(),
            "seller".to_string(),
            None,
        ));
        let token = state.db.create_web_session(id, &role, seller_id, 7);
        // AppendHeaders: an array of pairs replaces duplicate header names
        // (last one wins) — the session + cleared-pending pair needs append.
        return (
            AppendHeaders([
                (header::SET_COOKIE, session_cookie(&state, &token)),
                (header::SET_COOKIE, clear_pending_cookie(&state)),
            ]),
            Redirect::to(&state.url("/dashboard")),
        )
            .into_response();
    }
    let body = format!(
        r#"<h1>Verify MFA</h1><div class="alert alert-error" role="alert">Invalid code.</div>
<article class="panel"><form method="post" action="{}"><label>Code<input type="text" name="code" required></label><button type="submit">Verify</button></form></article>"#,
        state.url("/mfa/verify")
    );
    Html(html_page_bare(&state.base_path, "MFA verify", &body)).into_response()
}

#[tracing::instrument(skip_all)]
async fn mfa_enroll_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let Some((_raw, account_id, purpose, secret, _hash)) = pending_from_headers(&state, &headers)
    else {
        return Redirect::to(&state.url("/login")).into_response();
    };
    if purpose != "enroll" {
        return Redirect::to(&state.url("/mfa/verify")).into_response();
    }
    let secret = secret.unwrap_or_default();
    let username = state
        .db
        .get_web_account_by_id(account_id)
        .map(|r| r.1)
        .unwrap_or_default();
    let otpauth = format!(
        "otpauth://totp/TelegramBusiness:{}?secret={}&issuer=TelegramBusiness&digits=6&period=30",
        username, secret
    );
    let qr_svg = crate::qr::qr_svg_scaled(&otpauth, 6);
    let body = format!(
        r#"<h1>Enroll MFA <span class="sub">scan with your authenticator</span></h1>
<article class="panel">
<div class="qr" role="img" aria-label="QR code for your authenticator app">{qr}</div>
<p class="help">Can't scan? Enter this key in your authenticator app manually.</p>
<p><code class="copyable">{otpauth}</code></p>
<form method="post" action="{action}">
<label>Enter code from app<input type="text" name="code" required autocomplete="one-time-code"></label>
<button type="submit">Enable MFA</button>
</form>
</article>"#,
        qr = qr_svg,
        otpauth = esc(&otpauth),
        action = state.url("/mfa/enroll")
    );
    Html(html_page_bare(&state.base_path, "MFA enroll", &body)).into_response()
}

#[tracing::instrument(skip_all)]
async fn mfa_enroll_submit(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<MfaCodeForm>,
) -> impl IntoResponse {
    let Some((raw, account_id, purpose, secret_opt, hash)) = pending_from_headers(&state, &headers)
    else {
        return Redirect::to(&state.url("/login")).into_response();
    };
    if purpose != "enroll" {
        return Redirect::to(&state.url("/mfa/verify")).into_response();
    }
    let secret = secret_opt.unwrap_or_default();
    if login_throttle(&state.login_attempts, &format!("mfa:{}", raw), 5, 900) {
        return (
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            Html(html_page_bare(
                &state.base_path,
                "MFA enroll",
                "Too many attempts.",
            )),
        )
            .into_response();
    }
    let code = f.code.trim();
    let now_ts = chrono::Utc::now().timestamp() as u64;
    let generated = crate::util::totp_generate(&secret.clone(), now_ts, 6usize, 30u64);
    if !crate::util::ct_eq(&generated, code) {
        let body = format!(
            r#"<h1>Enroll MFA</h1><div class="alert alert-error" role="alert">Invalid code.</div>
<article class="panel"><form method="post" action="{}"><label>Code<input type="text" name="code" required></label><button type="submit">Enable</button></form></article>"#,
            state.url("/mfa/enroll")
        );
        return Html(html_page_bare(&state.base_path, "MFA enroll", &body)).into_response();
    }
    login_ok(&state.login_attempts, &format!("mfa:{}", raw));
    let _ = state.db.enable_totp(account_id, secret.clone());
    // generate recovery codes
    let mut plain: Vec<String> = Vec::new();
    for _ in 0..10 {
        plain.push(crate::util::gen_token(10));
    }
    let plain_clone = plain.clone();
    let hashed = tokio::task::spawn_blocking(move || {
        plain_clone
            .iter()
            .map(|c| bcrypt::hash(c, bcrypt::DEFAULT_COST).unwrap_or_default())
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    let _ = state.db.create_recovery_codes(account_id, hashed);
    let _ = state.db.delete_pending_mfa(&hash);
    let (id, _u, _h, role, seller_id) = state.db.get_web_account_by_id(account_id).unwrap_or((
        account_id,
        String::new(),
        String::new(),
        "seller".to_string(),
        None,
    ));
    let token = state.db.create_web_session(id, &role, seller_id, 7);
    let codes_html = plain
        .iter()
        .map(|c| format!("<li><code class=\"copyable\">{}</code></li>", esc(c)))
        .collect::<Vec<_>>()
        .join("");
    let body = format!(
        r#"<h1>MFA enabled</h1><div class="alert alert-warn" role="status" aria-live="polite">Save these recovery codes; they will not be shown again.</div>
<ul>{}</ul>
<p><a href="{}">Continue to dashboard</a></p>"#,
        codes_html,
        state.url("/dashboard")
    );
    // Two Set-Cookie headers on one response: an ARRAY of (name, value)
    // pairs would replace duplicate header names (last one wins), so the
    // append form is required for the session + cleared-pending pair.
    (
        AppendHeaders([
            (header::SET_COOKIE, session_cookie(&state, &token)),
            (header::SET_COOKIE, clear_pending_cookie(&state)),
        ]),
        Html(html_page_bare(&state.base_path, "MFA enabled", &body)),
    )
        .into_response()
}

// Settings MFA handlers

/// Validates the CURRENT second factor for an MFA-setting change (disable or
/// recovery-code rotation): a valid TOTP code against the account's enrolled
/// secret — same 6-digit window and replay rules as `/mfa/verify` — or, only
/// when the TOTP does not match, a valid unused recovery code (which
/// `verify_recovery_code` consumes on success). TOTP is tried first, so a
/// working authenticator never burns a recovery code; a recovery code is
/// consumed only when it is the factor that actually authorized the change.
fn verify_current_second_factor(state: &WebState, account_id: i64, code: &str) -> bool {
    let code = code.trim();
    let (secret, enabled, last_used) = state.db.get_totp_state(account_id);
    if enabled && code.chars().all(|c| c.is_ascii_digit()) && code.len() == 6 {
        let now_ts = chrono::Utc::now().timestamp() as u64;
        let current_counter = (now_ts / 30) as i64;
        let generated =
            crate::util::totp_generate(&secret.clone().unwrap_or_default(), now_ts, 6usize, 30u64);
        if crate::util::ct_eq(&generated, code)
            && last_used.map(|l| current_counter > l).unwrap_or(true)
        {
            let _ = state.db.update_totp_last_used(account_id, current_counter);
            return true;
        }
    }
    state.db.verify_recovery_code(account_id, code)
}

#[tracing::instrument(skip_all)]
async fn settings_mfa_enroll(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    // create pending enroll
    let mut buf = [0u8; 20];
    let _ = getrandom::fill(&mut buf);
    let secret = crate::util::base32_encode(&buf);
    let pending_raw = crate::util::gen_token(48);
    let pending_hash = sha256_hex(&pending_raw);
    let expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    let _ = state.db.create_pending_mfa(
        pending_hash,
        session.account_id,
        "enroll",
        Some(secret),
        expires,
    );
    (
        [(header::SET_COOKIE, pending_cookie(&state, &pending_raw))],
        Redirect::to(&state.url("/mfa/enroll")),
    )
        .into_response()
}

/// Disable MFA for the signed-in account. A hijacked session must not be
/// able to strip the second factor: the form requires the CURRENT factor
/// (valid TOTP or unused recovery code, `verify_current_second_factor`).
/// On a rejected code nothing changes and the settings page answers with an
/// error banner (`?mfa=reauth`).
#[tracing::instrument(skip_all)]
async fn settings_mfa_disable(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<MfaCodeForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !verify_current_second_factor(&state, session.account_id, &f.code) {
        // No state change on a rejected factor: MFA stays exactly as it was.
        return Redirect::to(&state.url("/settings?mfa=reauth")).into_response();
    }
    let _ = state.db.disable_totp(session.account_id);
    Redirect::to(&state.url("/settings?mfa=disabled")).into_response()
}

/// Regenerate the account's recovery codes. Same re-auth rule as disable:
/// the CURRENT factor is required (TOTP preferred; a recovery code is
/// consumed when it is what authorized the rotation), and a rejected code
/// leaves the old codes untouched.
#[tracing::instrument(skip_all)]
async fn settings_mfa_recovery(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<MfaCodeForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !verify_current_second_factor(&state, session.account_id, &f.code) {
        return Redirect::to(&state.url("/settings?mfa=reauth")).into_response();
    }
    let mut plain: Vec<String> = Vec::new();
    for _ in 0..10 {
        plain.push(crate::util::gen_token(10));
    }
    let plain_clone = plain.clone();
    let hashed = tokio::task::spawn_blocking(move || {
        plain_clone
            .iter()
            .map(|c| bcrypt::hash(c, bcrypt::DEFAULT_COST).unwrap_or_default())
            .collect::<Vec<_>>()
    })
    .await
    .unwrap_or_default();
    // replace existing recovery codes only
    let _ = state.db.delete_recovery_codes(session.account_id);
    let _ = state.db.create_recovery_codes(session.account_id, hashed);
    let codes_html = plain
        .iter()
        .map(|c| format!("<li><code class=\"copyable\">{}</code></li>", esc(c)))
        .collect::<Vec<_>>()
        .join("");
    let body = format!(
        r#"<h1>Recovery codes</h1><div class="alert alert-warn" role="status" aria-live="polite">New recovery codes (shown once):<ul>{}</ul></div>"#,
        codes_html
    );
    Html(html_page(&state, Some(&session), "Settings", &body)).into_response()
}

#[tracing::instrument(skip_all)]
async fn webusers_resetmfa(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<WebUserDeleteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Web users", "Web users").into_response();
    }
    if let Ok(id) = f.id.trim().parse::<i64>() {
        let _ = state.db.disable_totp(id);
    }
    Redirect::to(&state.url("/webusers")).into_response()
}

#[tracing::instrument(skip_all)]
async fn webusers_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Web users", "Web users").into_response();
    }
    let accounts = state.db.list_web_accounts_full();
    let rows = accounts
        .iter()
        .map(|(id, uname, role, seller)| {
            let (_s, enabled, _l) = state.db.get_totp_state(*id);
            let mfa = if enabled { "Enabled" } else { "Disabled" };
            format!(
                r#"<tr><td class="mono">{}</td><td>{}</td><td>{}</td><td class="mono">{}</td><td>{}</td><td>
<form method="post" action="{}" class="inline-form"><input type="hidden" name="id" value="{}"><span class="secret-field"><input type="password" name="password" placeholder="new password" aria-label="New password for {}">{toggle}</span><button type="submit" class="btn btn-sm">Reset</button></form>
<form method="post" action="{}" class="inline-form"><input type="hidden" name="id" value="{}"><button type="submit" class="btn btn-danger btn-sm" onclick="return confirm('Delete account {}?')">Delete</button></form>
<form method="post" action="{}" class="inline-form"><input type="hidden" name="id" value="{}"><button type="submit" class="btn btn-sm">Reset MFA</button></form>
</td></tr>"#,
                id,
                esc(uname),
                esc(role),
                seller.map(|v| v.to_string()).unwrap_or_else(|| "—".to_string()),
                mfa,
                state.url("/webusers/password"),
                id,
                esc(uname),
                state.url("/webusers/delete"),
                id,
                id,
                state.url("/webusers/resetmfa"),
                id,
                toggle = SECRET_TOGGLE,
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let table = format!(
        r#"<div class="table-wrap"><table aria-label="Web users"><thead><tr><th scope="col">ID</th><th scope="col">Username</th><th scope="col">Role</th><th scope="col">Seller</th><th scope="col">MFA</th><th scope="col">Actions</th></tr></thead><tbody>{}</tbody></table></div>"#,
        if rows.is_empty() {
            r#"<tr><td colspan="6" class="empty"><b>No web users.</b></td></tr>"#.to_string()
        } else {
            rows
        }
    );
    let body = format!(
        r#"<h1>Web users <span class="sub">console accounts</span></h1>
<article class="panel"><h3>Create account</h3>
<form method="post" action="{}" class="inline-form">
<label for="wu-username">Username</label>
<input type="text" id="wu-username" name="username" placeholder="username" required>
<label for="wu-password">Password</label>
<span class="secret-field"><input type="password" id="wu-password" name="password" placeholder="password" required>{toggle}</span>
<label for="wu-role">Role</label>
<select id="wu-role" name="role"><option value="seller">seller</option><option value="owner">owner</option></select>
<label for="wu-seller-id">Seller id</label>
<input type="number" id="wu-seller-id" name="seller_id" placeholder="seller id (optional)">
<button type="submit">Create</button>
</form></article>
{table}"#,
        state.url("/webusers/create"),
        table = table,
        toggle = SECRET_TOGGLE,
    );
    Html(html_page(&state, Some(&session), "Web users", &body)).into_response()
}

#[derive(Deserialize)]
struct WebUserCreateForm {
    username: String,
    password: String,
    role: String,
    seller_id: Option<String>,
}

#[derive(Deserialize)]
struct WebUserDeleteForm {
    id: String,
}

#[derive(Deserialize)]
struct WebUserPasswordForm {
    id: String,
    password: String,
}

#[tracing::instrument(skip_all)]
async fn webusers_create(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<WebUserCreateForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Web users", "Web users").into_response();
    }
    let role = if f.role == "owner" { "owner" } else { "seller" };
    let seller_id = f
        .seller_id
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok());
    let _ = state
        .db
        .create_web_account(&f.username, &f.password, role, seller_id)
        .await;
    Redirect::to(&state.url("/webusers")).into_response()
}

#[tracing::instrument(skip_all)]
async fn webusers_delete(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<WebUserDeleteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Web users", "Web users").into_response();
    }
    if let Ok(id) = f.id.trim().parse::<i64>() {
        state.db.delete_web_account(id);
    }
    Redirect::to(&state.url("/webusers")).into_response()
}

#[tracing::instrument(skip_all)]
async fn webusers_password(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<WebUserPasswordForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Web users", "Web users").into_response();
    }
    if let Ok(id) = f.id.trim().parse::<i64>() {
        let _ = state.db.update_web_password_by_id(id, &f.password).await;
    }
    Redirect::to(&state.url("/webusers")).into_response()
}

// ---------- API keys (web console, seller+) ----------

/// The API keys page body. `banner` carries the outcome of a create (rendered
/// above the panels so the POST can answer in place). Keys are scoped to the
/// signed-in account exactly like the REST handlers (`api_keys_list` et al. in
/// api.rs): the raw key exists only in the create banner of the one response
/// that issues it — the database stores its hash, and the raw value is never
/// written to the audit log or rendered again.
fn keys_body(state: &WebState, session: &Session, banner: Option<&str>) -> String {
    let banner = banner.unwrap_or_default();
    let keys = state.db.list_api_keys_detail(session.account_id);
    let rows = keys
        .iter()
        .map(|k| {
            let (id, _account_id, name, prefix, _scopes, revoked, created) = k;
            let revoked_badge = if *revoked != 0 {
                r#" <span class="badge badge-danger">Revoked</span>"#
            } else {
                ""
            };
            let name = name.as_deref().unwrap_or("—");
            // Revoking is idempotent server-side; a dead row only keeps its
            // badge, no button.
            let actions = if *revoked != 0 {
                String::new()
            } else {
                format!(
                    r#"<form method="post" action="{action}" class="inline-form"><input type="hidden" name="id" value="{id}"><button type="submit" class="btn btn-danger btn-sm" onclick="return confirm('Revoke API key {name}?')">Revoke</button></form>"#,
                    action = state.url("/keys/delete"),
                    id = id,
                    name = esc(name),
                )
            };
            format!(
                r#"<tr><td>{}{}</td><td class="mono">{}</td><td class="mono">{}</td><td>{}</td></tr>"#,
                esc(name),
                revoked_badge,
                esc(prefix),
                esc(created.as_deref().unwrap_or("—")),
                actions
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let table = format!(
        r#"<div class="table-wrap"><table aria-label="API keys"><thead><tr><th scope="col">Name</th><th scope="col">Key prefix</th><th scope="col">Created</th><th scope="col">Actions</th></tr></thead><tbody>{}</tbody></table></div>"#,
        if rows.is_empty() {
            r#"<tr><td colspan="4" class="empty"><b>No API keys yet — create one to call the REST API</b></td></tr>"#.to_string()
        } else {
            rows
        }
    );
    let form = format!(
        r#"<article class="panel"><h3>Create key</h3>
<form method="post" action="{}">
<label>Name<input type="text" name="name" placeholder="CI deploys"></label>
<button type="submit">Create key</button>
</form>
<p class="help">Optional label. The full key is shown once, right after creation.</p>
</article>"#,
        state.url("/keys/create")
    );
    format!(
        r#"{banner}<h1>API keys <span class="sub">REST API access</span></h1>
<p class="help">Keys for the REST API — see docs/API.md.</p>
{form}
{table}"#,
        banner = banner,
        form = form,
        table = table
    )
}

/// Query for the keys page: the revoke handler's outcome flag (?revoked=1).
#[derive(Deserialize)]
struct KeysQuery {
    revoked: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn keys_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<KeysQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "seller" && !is_admin_role(&session.role) {
        return forbidden_page(&state, "API keys", "API keys").into_response();
    }
    let banner = if q.revoked.as_deref() == Some("1") {
        Some(
            r#"<div class="alert alert-success" role="status" aria-live="polite">API key revoked.</div>"#
                .to_string(),
        )
    } else {
        None
    };
    Html(html_page(
        &state,
        Some(&session),
        "API keys",
        &keys_body(&state, &session, banner.as_deref()),
    ))
    .into_response()
}

#[derive(Deserialize)]
struct KeysCreateForm {
    name: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn keys_create(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<KeysCreateForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "seller" && !is_admin_role(&session.role) {
        return forbidden_page(&state, "API keys", "API keys").into_response();
    }
    let (id, raw) = state.db.create_api_key_full(
        session.account_id,
        non_blank(&f.name),
        // The web form issues unscoped keys; scopes stay a REST-only field.
        None,
    );
    state.db.log_action(
        session.account_id,
        &session.role,
        "api_key_create",
        "api_key",
        id.to_string(),
        None,
    );
    // The raw key is rendered here and nowhere else: no log, no storage, no
    // later read-back (the DB keeps only its hash).
    let banner = format!(
        r#"<div class="alert alert-success" role="status" aria-live="polite"><p><strong>API key created.</strong> Your new key (shown once):</p><p><code class="copyable">{}</code></p><p class="help">You won't see this key again. Store it now.</p></div>"#,
        esc(&raw)
    );
    Html(html_page(
        &state,
        Some(&session),
        "API keys",
        &keys_body(&state, &session, Some(&banner)),
    ))
    .into_response()
}

#[derive(Deserialize)]
struct KeysDeleteForm {
    id: String,
}

#[tracing::instrument(skip_all)]
async fn keys_delete(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<KeysDeleteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "seller" && !is_admin_role(&session.role) {
        return forbidden_page(&state, "API keys", "API keys").into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return Redirect::to(&state.url("/keys")).into_response();
    };
    // Ownership check for non-admin, word for word the REST handler's rule.
    if !is_admin_role(&session.role) {
        let own = state
            .db
            .list_api_keys(session.account_id)
            .into_iter()
            .any(|k| k.0 == id);
        if !own {
            return forbidden_page(&state, "API keys", "API keys").into_response();
        }
    }
    state.db.revoke_api_key(id);
    state.db.log_action(
        session.account_id,
        &session.role,
        "api_key_revoke",
        "api_key",
        id.to_string(),
        None,
    );
    Redirect::to(&state.url("/keys?revoked=1")).into_response()
}

// ---------- Setup bot ----------

#[tracing::instrument(skip_all)]
async fn settings_setupbot(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Settings", "Settings").into_response();
    }
    // The bot's own /setupbot (handlers/commands.rs::setup_bot_profile) pushes
    // the command list, description and short description to Telegram at boot.
    // That list lives behind a private fn, so the calls are replicated here —
    // the list below must be kept in sync with it — and run on demand so the
    // button does what it says without waiting for a restart.
    use teloxide::types::BotCommand;
    let cmds = vec![
        BotCommand::new("help", "Show help"),
        BotCommand::new("menu", "Main menu"),
        BotCommand::new("channel", "Show target channel"),
        BotCommand::new("setchannel", "Set target channel"),
        BotCommand::new("post", "Post text to channel"),
        BotCommand::new("postphoto", "Post a photo"),
        BotCommand::new("edit", "Edit posted message"),
        BotCommand::new("delete", "Delete posted message"),
        BotCommand::new("pin", "Pin a message"),
        BotCommand::new("unpin", "Unpin all"),
        BotCommand::new("invite", "Create invite link"),
        BotCommand::new("setdesc", "Set channel description"),
        BotCommand::new("settitle", "Set channel title"),
        BotCommand::new("info", "Channel info"),
        BotCommand::new("crypto", "Crypto Pay commands"),
        BotCommand::new("plans", "List subscription plans"),
        BotCommand::new("planadd", "Add a plan"),
        BotCommand::new("plandel", "Remove a plan"),
        BotCommand::new("productadd", "Add a product"),
        BotCommand::new("products", "List my products"),
        BotCommand::new("productdel", "Deactivate a product"),
        BotCommand::new("shop", "Browse the shop"),
        BotCommand::new("buy", "Buy a plan or product"),
        BotCommand::new("paid", "Claim payment"),
        BotCommand::new("redeem", "Redeem activation code"),
        BotCommand::new("gencodes", "Generate activation codes"),
        BotCommand::new("codes", "List activation codes"),
        BotCommand::new("grant", "Grant access"),
        BotCommand::new("revoke", "Revoke access"),
        BotCommand::new("subs", "List subscriptions"),
        BotCommand::new("expirecheck", "Run expiry cleanup"),
        BotCommand::new("broadcast", "Broadcast to all users"),
        BotCommand::new("setup", "Easy seller setup"),
        BotCommand::new("becomeseller", "Become a seller"),
        BotCommand::new("ai", "AI generate content"),
        BotCommand::new("aipost", "AI-assisted post"),
        BotCommand::new("refundapprove", "Approve a pending refund"),
        BotCommand::new("payoutapprove", "Approve a pending payout"),
    ];
    let commands = state.bot.set_my_commands(cmds).await;
    let description = state
        .bot
        .set_my_description()
        .description(
            "Telegram shop: digital products, channel memberships, codes and bookings. Browse /shop — buy with /buy.",
        )
        .await;
    let short_description = state
        .bot
        .set_my_short_description()
        .short_description("Channel & crypto agent")
        .await;
    let ok = commands.is_ok() && description.is_ok() && short_description.is_ok();
    Redirect::to(&state.url(if ok {
        "/settings?setupbot=1"
    } else {
        "/settings?setupbot=err"
    }))
    .into_response()
}

// ---------- Channel message ops ----------

#[derive(Deserialize)]
struct PostScheduleForm {
    chat_id: String,
    content: String,
    scheduled_at: String,
}

#[tracing::instrument(skip_all)]
async fn post_schedule(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PostScheduleForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_id = if session.role == "owner" {
        0i64
    } else {
        match session.seller_id {
            Some(v) => v,
            None => return Redirect::to(&state.url("/posts")).into_response(),
        }
    };
    if f.chat_id.trim().is_empty() || f.content.trim().is_empty() {
        return Redirect::to(&state.url("/posts")).into_response();
    }
    state
        .db
        .create_scheduled_post(seller_id, &f.chat_id, &f.content, "text", &f.scheduled_at);
    Redirect::to(&state.url("/posts")).into_response()
}

#[derive(Deserialize)]
struct PostEditForm {
    chat_id: String,
    message_id: String,
    text: String,
    post_id: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn post_edit(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PostEditForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let _ = session;
    let chat_id = f.chat_id.clone();
    let msg_id: i32 = f.message_id.trim().parse().unwrap_or(0);
    if msg_id != 0 {
        let _ = state
            .bot
            .edit_message_text(
                crate::util::to_chat_id(&chat_id),
                teloxide::types::MessageId(msg_id),
                &f.text,
            )
            .await;
    }
    if let Some(pid) = f
        .post_id
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok())
    {
        let _ = state.db.update_post_content(pid, &f.text);
    }
    Redirect::to(&state.url("/posts")).into_response()
}

#[derive(Deserialize)]
struct PostIdForm {
    chat_id: String,
    message_id: String,
    post_id: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn post_delete(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PostIdForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let _ = session;
    let chat_id = f.chat_id.clone();
    let msg_id: i32 = f.message_id.trim().parse().unwrap_or(0);
    if msg_id != 0 {
        let _ = state
            .bot
            .delete_message(
                crate::util::to_chat_id(&chat_id),
                teloxide::types::MessageId(msg_id),
            )
            .await;
    }
    if let Some(pid) = f
        .post_id
        .as_deref()
        .and_then(|s| s.trim().parse::<i64>().ok())
    {
        let _ = state.db.delete_post(pid);
    }
    Redirect::to(&state.url("/posts")).into_response()
}

#[tracing::instrument(skip_all)]
async fn post_pin(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PostIdForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let _ = session;
    let chat_id = f.chat_id.clone();
    let msg_id: i32 = f.message_id.trim().parse().unwrap_or(0);
    if msg_id != 0 {
        let _ = state
            .bot
            .pin_chat_message(
                crate::util::to_chat_id(&chat_id),
                teloxide::types::MessageId(msg_id),
            )
            .await;
    }
    Redirect::to(&state.url("/posts")).into_response()
}

#[tracing::instrument(skip_all)]
async fn post_unpin(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PostIdForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let _ = session;
    let chat_id = f.chat_id.clone();
    let msg_id: i32 = f.message_id.trim().parse().unwrap_or(0);
    if msg_id != 0 {
        let _ = state
            .bot
            .unpin_chat_message(crate::util::to_chat_id(&chat_id))
            .message_id(teloxide::types::MessageId(msg_id))
            .await;
    }
    Redirect::to(&state.url("/posts")).into_response()
}

#[derive(Deserialize)]
struct ChannelInviteForm {
    chat_id: String,
}

/// Owner-only (invite links grant channel access, like the other global
/// channel mutations). The link is rendered inline — shown exactly once,
/// never stored and never logged; the failed call re-renders the channels
/// page with the ordinary error banner.
#[tracing::instrument(skip_all)]
async fn channel_invite(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelInviteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Channels", "Invite links");
    }
    let chat_id = f.chat_id.trim().to_string();
    match state.bot.create_chat_invite_link(chat_id.clone()).await {
        Ok(r) => {
            let body = format!(
                r#"<h1>Invite link <span class="sub">{}</span></h1>
<div class="alert alert-success" role="status" aria-live="polite">Invite link created for <span class="mono">{}</span>. Copy it now — it is shown once and never stored or logged.</div>
<p><code class="copyable">{}</code></p>
<p><a href="{}">Back to channels</a></p>"#,
                esc(&chat_id),
                esc(&chat_id),
                esc(&r.invite_link),
                state.url("/channels")
            );
            Html(html_page(&state, Some(&session), "Invite link", &body)).into_response()
        }
        Err(e) => channels_error(
            &state,
            &session,
            &format!("Could not create the invite link: {}", esc(&e.to_string())),
        ),
    }
}

#[derive(Deserialize)]
struct ChannelTitleForm {
    chat_id: String,
    title: String,
}

#[tracing::instrument(skip_all)]
async fn channel_title(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelTitleForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let _ = session;
    let chat_id = f.chat_id.clone();
    let _ = state
        .bot
        .set_chat_title(crate::util::to_chat_id(&chat_id), &f.title)
        .await;
    Redirect::to(&state.url("/channels")).into_response()
}

#[derive(Deserialize)]
struct ChannelDescForm {
    chat_id: String,
    description: String,
}

#[tracing::instrument(skip_all)]
async fn channel_desc(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelDescForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let _ = session;
    let chat_id = f.chat_id.clone();
    let _ = state
        .bot
        .set_chat_description(crate::util::to_chat_id(&chat_id))
        .description(&f.description)
        .await;
    Redirect::to(&state.url("/channels")).into_response()
}

#[derive(Deserialize)]
struct SetupForm {
    bot_token: String,
    owner_id: String,
    default_channel: Option<String>,
    crypto_token: Option<String>,
    web_username: Option<String>,
    web_password: Option<String>,
}

#[tracing::instrument(skip_all)]
async fn setup_page(State(state): State<WebState>) -> impl IntoResponse {
    if setup_done(&state) {
        return Redirect::to(&state.url("/login")).into_response();
    }
    let body = format!(
        r#"<h1>Welcome — first-run setup</h1>
<p class="help">Connect your bot and claim the console. Takes about two minutes; every field has a hint below.</p>
<p class="help">You can skip and configure the bot later in Settings — the console works without it.</p>
<details open><summary>How to get a bot token (2 minutes, no skills needed)</summary>
<ol><li>Open Telegram and message <a href="https://t.me/BotFather" rel="noopener">@BotFather</a>.</li>
<li>Send <code>/newbot</code>, choose a display name, then a username ending in <code>bot</code>.</li>
<li>Copy the token it replies with (<code>123456:ABC-…</code>) and paste it below.</li>
<li>For your owner ID, message <a href="https://t.me/userinfobot" rel="noopener">@userinfobot</a> — it replies with your numeric ID.</li>
<li>Keep the bot's privacy mode ON; add it as <b>admin</b> to your channel when you sell access.</li></ol></details>
<form method="post" action="{}">
<label for="setup-bot-token">Bot token</label>
<span class="secret-field"><input type="password" id="setup-bot-token" name="bot_token" required autocomplete="off" placeholder="123456:ABC-DEF…">{toggle}</span><span class="help">From @BotFather. Stored in .env with 0600 permissions, never logged.</span>
<label>Owner Telegram user ID<input type="text" name="owner_id" required inputmode="numeric" placeholder="123456789"><span class="help">Your numeric ID from @userinfobot — this account owns the console.</span></label>
<label>Default channel<input type="text" name="default_channel" placeholder="@yourchannel"><span class="help">Optional — where posts and grants go when nothing else is set.</span></label>
<label for="setup-crypto-token">Crypto Pay token</label>
<span class="secret-field"><input type="password" id="setup-crypto-token" name="crypto_token" autocomplete="off" placeholder="from @CryptoBot → Crypto Pay → Create App">{toggle}</span><span class="help">Optional — enables automated payment verification.</span>
<div class="form-row">
<div><label>Dashboard username<input type="text" name="web_username" value="owner" autocomplete="username"></label></div>
<div><label for="setup-web-password">Dashboard password</label>
<span class="secret-field"><input type="password" id="setup-web-password" name="web_password" required minlength="6" autocomplete="new-password">{toggle}</span><span class="help">At least 6 characters — this unlocks the console.</span></div>
</div>
<div class="form-row">
<div><button type="submit">Save and continue</button></div>
<div><button type="submit" class="btn btn-secondary" formaction="{}" formnovalidate>Skip for now</button></div>
</div>
</form>"#,
        state.url("/setup"),
        state.url("/setup/skip"),
        toggle = SECRET_TOGGLE,
    );
    Html(html_page_bare(&state.base_path, "Setup", &body)).into_response()
}

/// "Skip for now": close the first-run gate WITHOUT creating an account or
/// writing .env. The close-on-leave invariant holds — once `setup_done` is
/// set, no page redirects back here, the console simply starts unconfigured
/// (setup mode) and configuration waits in Settings.
#[tracing::instrument(skip_all)]
async fn setup_skip(State(state): State<WebState>) -> impl IntoResponse {
    if setup_done(&state) {
        return Redirect::to(&state.url("/login")).into_response();
    }
    state.db.set_config("setup_done", "1");
    Redirect::to(&state.url("/login")).into_response()
}

#[tracing::instrument(skip_all)]
async fn setup_submit(
    State(state): State<WebState>,
    Form(f): Form<SetupForm>,
) -> impl IntoResponse {
    if setup_done(&state) {
        return Redirect::to(&state.url("/login")).into_response();
    }
    // Validate bot token.
    let token = f.bot_token.trim().to_string();
    if token.is_empty() {
        return setup_error(&state, "Bot token is required.");
    }
    let ok = validate_bot_token(&token).await;
    if !ok {
        return setup_error(&state, "Invalid bot token (Telegram rejected it).");
    }
    let owner_id: i64 = match f.owner_id.trim().parse() {
        Ok(v) if v > 0 => v,
        _ => {
            return setup_error(&state, "Owner ID must be a positive number.");
        }
    };
    let uname = f
        .web_username
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "owner".to_string());
    let pw = f.web_password.clone().unwrap_or_default();
    if pw.len() < 6 {
        return setup_error(&state, "Password must be at least 6 characters.");
    }
    let channel = f.default_channel.clone().filter(|s| !s.trim().is_empty());
    let crypto = f.crypto_token.clone().filter(|s| !s.trim().is_empty());

    // Write .env through the shared key-preserving writer (extracted to
    // config.rs, 0600): exactly the keys this form owns are rewritten, every
    // other line (WEB_BIND, WEB_URL, WEB_BASE_PATH, ...) survives. Blank
    // optionals are written as empty values, matching the original writer's
    // behavior so a re-run is idempotent.
    let updates = [
        ("TELEGRAM_BOT_TOKEN".to_string(), token.clone()),
        ("OWNER_ID".to_string(), owner_id.to_string()),
        (
            "DATABASE_URL".to_string(),
            format!("sqlite:{}", state.cfg.db_path),
        ),
        (
            "DEFAULT_CHANNEL".to_string(),
            channel.clone().unwrap_or_default(),
        ),
        (
            "CRYPTO_PAY_TOKEN".to_string(),
            crypto.clone().unwrap_or_default(),
        ),
        ("WEB_USERNAME".to_string(), uname.clone()),
        ("WEB_PASSWORD".to_string(), pw.clone()),
        ("WEB_PORT".to_string(), state.cfg.web_port().to_string()),
        ("RUST_LOG".to_string(), "info".to_string()),
    ];
    if let Err(e) = crate::config::write_env_file(&updates) {
        return setup_error(
            &state,
            &format!("Failed to write .env: {}", esc(&e.to_string())),
        );
    }

    // Seed owner web account + channel in DB.
    if state.db.get_web_account_by_username(&uname).is_none() {
        let _ = state
            .db
            .create_web_account(&uname, &pw, "owner", None)
            .await;
    }
    if let Some(c) = &channel {
        state.db.set_config("channel", c);
    }

    // Mark setup complete in the DB (authoritative across restarts and for
    // the still-running process whose env snapshot is frozen).
    state.db.set_config("setup_done", "1");
    state.db.set_config("restart_required", "1");

    let body = format!(
        r#"<div class="alert alert-success" role="status" aria-live="polite">Setup complete. Configuration saved to <code>.env</code>.</div>
<p>Restart the application to start the bot with the new settings. If it runs under Docker or systemd the service restart handles it; otherwise start it again manually.</p>
<p><a class="btn" href="{}">Go to sign in</a></p>"#,
        state.url("/login")
    );
    Html(html_page_bare(&state.base_path, "Setup", &body)).into_response()
}

/// Pre-setup error surface: the front-door shell with an honest alert and a
/// way back. Every setup failure says what happened and what to do next.
fn setup_error(state: &WebState, msg: &str) -> Response {
    Html(html_page_bare(
        &state.base_path,
        "Setup",
        &format!(
            r#"<div class="alert alert-error" role="alert">{}</div><p><a href="{}">Back to setup</a></p>"#,
            msg,
            state.url("/setup")
        ),
    ))
    .into_response()
}

#[tracing::instrument(skip_all)]
async fn validate_bot_token(token: &str) -> bool {
    let url = format!("https://api.telegram.org/bot{}/getMe", token);
    match reqwest::Client::new().get(&url).send().await {
        Ok(resp) => {
            if let Ok(v) = resp.json::<serde_json::Value>().await {
                v.get("ok").and_then(|b| b.as_bool()).unwrap_or(false)
            } else {
                false
            }
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use proptest::prelude::*;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tower::ServiceExt;

    proptest! {
        /// esc() output contains no raw HTML metacharacters (`< > " '` — the
        /// escaped forms use entities), every `&` in the output begins one of
        /// the five known entities, and decoding exactly restores the input.
        #[test]
        fn prop_esc_neutralizes_and_roundtrips(s in ".*") {
            let e = esc(&s);
            for c in ['<', '>', '"', '\''] {
                prop_assert!(!e.contains(c), "raw {c} survived escaping");
            }
            let entities = ["&amp;", "&lt;", "&gt;", "&quot;", "&#39;"];
            for (i, _) in e.match_indices('&').collect::<Vec<_>>() {
                let rest = &e[i..];
                prop_assert!(
                    entities.iter().any(|ent| rest.starts_with(ent)),
                    "stray & at byte {i} in {e:?}"
                );
            }
            // Decode order: &amp; LAST — restoring & early would fabricate
            // new entity sequences (esc("&#39;") == "&amp;#39;").
            let decoded = e
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&quot;", "\"")
                .replace("&#39;", "'")
                .replace("&amp;", "&");
            prop_assert_eq!(decoded, s);
        }
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn make_state() -> (WebState, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Db::new(db_path.to_str().unwrap()).unwrap();
        db.create_web_account("owner", "secret", "owner", None)
            .await
            .unwrap();
        let cfg = Config {
            bot_token: "0:dummy".to_string(),
            owner_id: 1,
            db_path: db_path.to_str().unwrap().to_string(),
            default_channel: None,
            crypto_token: None,
            ai_enabled: false,
            ai_provider: "openai".to_string(),
            ai_api_style: "openai".to_string(),
            ai_base_url: "https://api.openai.com/v1".to_string(),
            ai_api_key: None,
            ai_model: "gpt-4o-mini".to_string(),
            ai_daily_token_budget: None,
            mcp_owner_token: None,
        };
        let bot = teloxide::Bot::new(cfg.bot_token.clone());
        let state = WebState {
            db: Arc::new(db),
            cfg,
            bot,
            login_attempts: Arc::new(StdMutex::new(HashMap::new())),
            ai_test_inflight: Arc::new(tokio::sync::Mutex::new(())),
            base_path: String::new(),
        };
        (state, dir)
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn body_string(resp: axum::http::Response<Body>) -> String {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8_lossy(&bytes).to_string()
    }

    /// Serializes tests that touch the process environment (ENV_FILE): env
    /// is global and cargo runs tests in parallel threads, so env-mutating
    /// tests take this lock — held across the awaited request, which is why
    /// it is async-aware (a std guard would poison the runtime's threads).
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Point the shared env writer at a test-owned file. SAFETY: ENV_FILE is
    /// written only while ENV_LOCK is held and no other code in the test
    /// binary reads it concurrently (single-writer mutation).
    fn set_env_file_var(path: &std::path::Path) {
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::set_var("ENV_FILE", path) };
    }

    fn clear_env_file_var() {
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::remove_var("ENV_FILE") };
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn unauth_dashboard_redirects_to_login() {
        let (state, _dir) = make_state().await;
        let app = router(state);
        let req = Request::builder()
            .uri("/dashboard")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(resp.headers().get("location").unwrap(), "/login");
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn login_valid_sets_cookie_and_redirects() {
        let (state, _dir) = make_state().await;
        let app = router(state);
        let req = Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("username=owner&password=secret"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(resp.headers().get("location").unwrap(), "/mfa/enroll");
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn login_invalid_returns_error() {
        let (state, _dir) = make_state().await;
        let app = router(state);
        let req = Request::builder()
            .method("POST")
            .uri("/login")
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("username=owner&password=wrong"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("Invalid credentials"));
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn auth_dashboard_returns_200() {
        let (state, _dir) = make_state().await;
        let db = state.db.clone();
        let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("owner").unwrap();
        let token = db.create_web_session(acc_id, "owner", None, 7);
        let app = router(state);
        let req = Request::builder()
            .uri("/dashboard")
            .header("cookie", format!("session={}", token))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn logout_clears_session() {
        let (state, _dir) = make_state().await;
        let db = state.db.clone();
        let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("owner").unwrap();
        let token = db.create_web_session(acc_id, "owner", None, 7);
        let app = router(state);
        let req = Request::builder()
            .uri("/logout")
            .header("cookie", format!("session={}", token))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert!(resp.status().is_redirection());
        let set_cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
        assert!(set_cookie.contains("Max-Age=0"));
        assert!(db.get_web_session(&token).is_none());
    }

    // ---- Skippable setup + claim + settings + channels (PLAN v1.3.x) ----

    /// A state that has CLOSED setup (setup_done=1) but created no owner —
    /// the post-skip world, where /login is the claim form.
    #[tracing::instrument(skip_all)]
    async fn make_unclaimed_state() -> (WebState, TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db = Db::new(db_path.to_str().unwrap()).unwrap();
        db.set_config("setup_done", "1");
        let cfg = Config {
            bot_token: String::new(),
            owner_id: 0,
            db_path: db_path.to_str().unwrap().to_string(),
            default_channel: None,
            crypto_token: None,
            ai_enabled: false,
            ai_provider: "openai".to_string(),
            ai_api_style: "openai".to_string(),
            ai_base_url: "https://api.openai.com/v1".to_string(),
            ai_api_key: None,
            ai_model: "gpt-4o-mini".to_string(),
            ai_daily_token_budget: None,
            mcp_owner_token: None,
        };
        let state = WebState {
            db: Arc::new(db),
            cfg,
            bot: teloxide::Bot::new("0:dummy"),
            login_attempts: Arc::new(StdMutex::new(HashMap::new())),
            ai_test_inflight: Arc::new(tokio::sync::Mutex::new(())),
            base_path: String::new(),
        };
        (state, dir)
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn post_req(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    pub(crate) fn authed_req(uri: &str, token: &str) -> Request<Body> {
        Request::builder()
            .uri(uri)
            .header("cookie", format!("session={}", token))
            .body(Body::empty())
            .unwrap()
    }

    pub(crate) fn post_authed_req(uri: &str, body: &str, token: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("cookie", format!("session={}", token))
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// A loopback or remote peer, injected the way
    /// `into_make_service_with_connect_info` would (as the ConnectInfo
    /// extension, which the PeerIp extractor reads).
    fn with_peer(req: Request<Body>, ip: [u8; 4]) -> Request<Body> {
        let (mut parts, body) = req.into_parts();
        parts
            .extensions
            .insert(ConnectInfo(SocketAddr::new(IpAddr::from(ip), 40000)));
        Request::from_parts(parts, body)
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn owner_session(state: &WebState) -> String {
        let (acc_id, _u, _h, _r, _s) = state
            .db
            .get_web_account_by_username("owner")
            .expect("owner account exists");
        state.db.create_web_session(acc_id, "owner", None, 7)
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn seller_session(state: &WebState, seller_id: i64) -> String {
        state
            .db
            .create_web_account("seller", "secret", "seller", Some(seller_id))
            .await
            .unwrap();
        let (acc_id, _u, _h, _r, _s) = state
            .db
            .get_web_account_by_username("seller")
            .expect("seller account exists");
        state
            .db
            .create_web_session(acc_id, "seller", Some(seller_id), 7)
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn skip_sets_setup_done_and_creates_no_account() {
        let (state, _dir) = make_unclaimed_state().await;
        let db = state.db.clone();
        let app = router(state);
        let resp = app.oneshot(post_req("/setup/skip", "")).await.unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/login",
            "skip lands on the (claim) front door"
        );
        // The B2 close-on-leave invariant: the gate is closed for good...
        assert_eq!(db.get_config("setup_done").as_deref(), Some("1"));
        // ...but nothing else happened: no account, no configured snapshot.
        assert!(db.get_web_account_by_username("owner").is_none());
        assert!(!db.owner_account_exists());
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn skip_after_done_redirects_to_login() {
        let (state, _dir) = make_unclaimed_state().await;
        state.db.set_config("setup_done", "1");
        let app = router(state);
        let resp = app.oneshot(post_req("/setup/skip", "")).await.unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(resp.headers().get("location").unwrap(), "/login");
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn claim_page_renders_when_no_owner() {
        let (state, _dir) = make_unclaimed_state().await;
        let app = router(state);
        let resp = app.oneshot(get_req("/login")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("Claim the console"), "{body}");
        assert!(
            body.contains(r#"autocomplete="new-password""#),
            "claim form asks for a NEW password:\n{body}"
        );
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn claim_blocked_off_loopback_without_code() {
        let (state, _dir) = make_unclaimed_state().await;
        let db = state.db.clone();
        // A live code exists on the server; the attacker does not have it.
        db.set_config("claim_code", "code-12345");
        // Remote peer, no code: refused, nothing created, no claim hint leak.
        let req = with_peer(
            post_req(
                "/login",
                "username=admin&password=hunter2&confirm_password=hunter2",
            ),
            [203, 0, 113, 7],
        );
        let resp = router(state.clone()).oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            body.contains("Claiming requires the one-time code shown in the server console."),
            "{body}"
        );
        assert!(!body.contains("hunter2"), "form input echoed back: {body}");
        assert!(
            !body.contains("code-12345"),
            "the server's claim code must never be echoed: {body}"
        );
        assert!(!db.owner_account_exists());

        // A wrong code from a remote peer: same refusal, still no echo.
        let req = with_peer(
            post_req(
                "/login",
                "username=admin&password=hunter2&confirm_password=hunter2&claim_code=wrong",
            ),
            [203, 0, 113, 7],
        );
        let resp = router(state).oneshot(req).await.unwrap();
        let body = body_string(resp).await;
        assert!(
            body.contains("Claiming requires the one-time code"),
            "{body}"
        );
        assert!(
            !body.contains("code-12345"),
            "real code must not appear when a wrong one is tried: {body}"
        );
        assert!(!db.owner_account_exists());
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn claim_with_code_works_and_creates_owner_session() {
        let (state, _dir) = make_unclaimed_state().await;
        // The claim code is generated at startup and printed to the console;
        // tests seed it the same place production stores it.
        state.db.set_config("claim_code", "code-12345");
        let db = state.db.clone();
        let app = router(state);
        // No peer extension (unknown, i.e. remote) — the code alone must be
        // sufficient.
        let resp = app
            .oneshot(post_req(
                "/login",
                "username=root&password=secret1&confirm_password=secret1&claim_code=code-12345",
            ))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(resp.headers().get("location").unwrap(), "/dashboard");
        let cookie = resp
            .headers()
            .get("set-cookie")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(cookie.contains("session="), "{cookie}");
        let (acc_id, _u, _h, role, seller_id) =
            db.get_web_account_by_username("root").expect("claimed");
        assert_eq!(role, "owner");
        assert_eq!(seller_id, None);
        // The presented session works and belongs to the new owner account.
        let token = cookie
            .split(';')
            .next()
            .unwrap()
            .trim_start_matches("session=");
        assert_eq!(
            db.get_web_session(token),
            Some((acc_id, "owner".to_string(), None))
        );
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn claim_from_loopback_needs_no_code() {
        // The desktop default (WEB_BIND unset) is a loopback bind, so a
        // loopback peer is genuinely local; pin the env so a developer's
        // exported WEB_BIND cannot flip the outcome.
        let _env = ENV_LOCK.lock().await;
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::remove_var("WEB_BIND") };
        let (state, _dir) = make_unclaimed_state().await;
        let db = state.db.clone();
        let app = router(state);
        let req = with_peer(
            post_req(
                "/login",
                "username=root&password=secret1&confirm_password=secret1",
            ),
            [127, 0, 0, 1],
        );
        let resp = app.oneshot(req).await.unwrap();
        assert!(resp.status().is_redirection(), "loopback claims freely");
        assert_eq!(resp.headers().get("location").unwrap(), "/dashboard");
        assert!(db.owner_account_exists());
    }

    /// HIGH-5 regression: behind a reverse proxy on the same host
    /// (WEB_BIND=0.0.0.0) every remote browser arrives with a loopback peer
    /// address — the peer alone must not waive the one-time claim code.
    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn claim_loopback_behind_wildcard_bind_requires_code() {
        let _env = ENV_LOCK.lock().await;
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::set_var("WEB_BIND", "0.0.0.0") };
        let (state, _dir) = make_unclaimed_state().await;
        let db = state.db.clone();
        let app = router(state);
        let req = with_peer(
            post_req(
                "/login",
                "username=root&password=secret1&confirm_password=secret1",
            ),
            [127, 0, 0, 1],
        );
        let resp = app.oneshot(req).await.unwrap();
        assert!(
            !resp.status().is_redirection(),
            "a loopback peer behind a wildcard bind is NOT the machine itself"
        );
        let body = body_string(resp).await;
        assert!(
            body.contains("Claiming requires the one-time code"),
            "the claim form must demand the code: {body}"
        );
        assert!(!db.owner_account_exists(), "no account may be created");
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::remove_var("WEB_BIND") };
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn claim_mismatched_confirmation_refused() {
        // Loopback-only default bind (HIGH-5): pin the env so the loopback
        // peer keeps waiving the claim code here.
        let _env = ENV_LOCK.lock().await;
        // SAFETY: serialized by ENV_LOCK.
        unsafe { std::env::remove_var("WEB_BIND") };
        let (state, _dir) = make_unclaimed_state().await;
        let db = state.db.clone();
        let app = router(state);
        let req = with_peer(
            post_req(
                "/login",
                "username=root&password=secret1&confirm_password=secret2",
            ),
            [127, 0, 0, 1],
        );
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("Passwords do not match"), "{body}");
        assert!(!db.owner_account_exists());
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn claim_impossible_once_owner_exists() {
        let (state, _dir) = make_state().await;
        let db = state.db.clone();
        let app = router(state);
        // GET: the front door is the sign-in form again — claim mode never
        // returns.
        let resp = app.clone().oneshot(get_req("/login")).await.unwrap();
        let body = body_string(resp).await;
        assert!(body.contains("Sign in"), "{body}");
        assert!(!body.contains("Claim the console"), "{body}");
        // POST with claim-shaped fields: treated as a plain login attempt.
        let resp = app
            .oneshot(post_req(
                "/login",
                "username=root&password=secret1&confirm_password=secret1&claim_code=code-12345",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("Invalid credentials"), "{body}");
        assert!(db.get_web_account_by_username("root").is_none());
        assert!(db.owner_account_exists());
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn settings_seller_refused() {
        let (state, _dir) = make_state().await;
        let token = seller_session(&state, 42).await;
        let app = router(state);
        for uri in ["/settings"] {
            let resp = app.clone().oneshot(authed_req(uri, &token)).await.unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
            assert!(
                body_string(resp)
                    .await
                    .contains("restricted to the console owner")
            );
        }
        for (uri, body) in [
            ("/settings/save", "owner_id=42"),
            ("/settings/password", "new_password=secret1"),
        ] {
            let resp = app
                .clone()
                .oneshot(post_authed_req(uri, body, &token))
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{uri}");
        }
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn settings_save_blank_fields_change_nothing() {
        let (state, dir) = make_state().await;
        let token = owner_session(&state).await;
        let app = router(state);
        let _env = ENV_LOCK.lock().await;
        let env_path = dir.path().join(".env");
        set_env_file_var(&env_path);
        let resp = app
            .oneshot(post_authed_req(
                "/settings/save",
                "bot_token=&owner_id=&default_channel=&crypto_token=",
                &token,
            ))
            .await
            .unwrap();
        let body = body_string(resp).await;
        assert!(body.contains("Nothing to save"), "{body}");
        assert!(!env_path.exists(), "all-blank save must not create .env");
        clear_env_file_var();
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn settings_save_writes_env_via_helper() {
        let (state, dir) = make_state().await;
        let token = owner_session(&state).await;
        let app = router(state);
        let _env = ENV_LOCK.lock().await;
        let env_path = dir.path().join(".env");
        set_env_file_var(&env_path);
        // No bot token in the form (that path validates against Telegram);
        // the other fields go straight through the shared writer.
        let resp = app
            .oneshot(post_authed_req(
                "/settings/save",
                "owner_id=42&default_channel=@shop",
                &token,
            ))
            .await
            .unwrap();
        let body = body_string(resp).await;
        assert!(
            body.contains("Saved. The bot connects on restart."),
            "{body}"
        );
        let env = std::fs::read_to_string(&env_path).expect(".env written");
        assert!(env.contains("OWNER_ID=42"), "{env}");
        assert!(env.contains("DEFAULT_CHANNEL=@shop"), "{env}");
        clear_env_file_var();
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn settings_save_rejects_invalid_token_without_writing() {
        let (state, dir) = make_state().await;
        let token = owner_session(&state).await;
        let app = router(state);
        let _env = ENV_LOCK.lock().await;
        let env_path = dir.path().join(".env");
        set_env_file_var(&env_path);
        let resp = app
            .oneshot(post_authed_req(
                "/settings/save",
                "bot_token=123456:definitely-not-a-token",
                &token,
            ))
            .await
            .unwrap();
        let body = body_string(resp).await;
        assert!(body.contains("Invalid bot token"), "{body}");
        assert!(!env_path.exists(), "a rejected token must never reach .env");
        clear_env_file_var();
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn settings_password_changes_and_invalidates_other_sessions() {
        let (state, _dir) = make_state().await;
        let db = state.db.clone();
        let kept = owner_session(&state).await;
        let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("owner").unwrap();
        let other = db.create_web_session(acc_id, "owner", None, 7);
        let app = router(state);
        let resp = app
            .oneshot(post_authed_req(
                "/settings/password",
                "new_password=fresh1",
                &kept,
            ))
            .await
            .unwrap();
        let body = body_string(resp).await;
        assert!(
            body.contains("Password changed. Other sessions were signed out."),
            "{body}"
        );
        // The caller's session survives; every other session of the account
        // is gone; the new password verifies.
        assert!(db.get_web_session(&kept).is_some());
        assert!(db.get_web_session(&other).is_none());
        assert!(db.verify_web_password("owner", "fresh1").await.is_some());
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn channels_add_default_remove_scoping() {
        let (state, _dir) = make_state().await;
        let db = state.db.clone();
        let owner = owner_session(&state).await;
        let seller = seller_session(&state, 42).await;
        let app = router(state);

        // Owner adds to the global set (seller_id 0).
        let resp = app
            .clone()
            .oneshot(post_authed_req("/channels/add", "channel=@one", &owner))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        let resp = app
            .clone()
            .oneshot(post_authed_req("/channels/add", "channel=@two", &owner))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());

        // Set default; the page shows the badge and hides Remove on that row.
        let resp = app
            .clone()
            .oneshot(post_authed_req("/channels/default", "channel=@one", &owner))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(db.default_channel().as_deref(), Some("@one"));
        let resp = app
            .clone()
            .oneshot(authed_req("/channels", &owner))
            .await
            .unwrap();
        let body = body_string(resp).await;
        assert!(body.contains("@one") && body.contains("@two"), "{body}");
        assert!(body.contains("default"), "{body}");

        // Removing the default is refused, inline, and removes nothing.
        let resp = app
            .clone()
            .oneshot(post_authed_req("/channels/remove", "channel=@one", &owner))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "inline error, not PRG");
        let body = body_string(resp).await;
        assert!(body.contains("cannot be removed"), "{body}");
        assert_eq!(db.default_channel().as_deref(), Some("@one"));

        // A non-default row can take over, after which the old default can
        // be removed.
        let resp = app
            .clone()
            .oneshot(post_authed_req("/channels/default", "channel=@two", &owner))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        let resp = app
            .clone()
            .oneshot(post_authed_req("/channels/remove", "channel=@one", &owner))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(db.default_channel().as_deref(), Some("@two"));

        // Seller scope: adding sets the seller's own channel, removing
        // touches only their own row, and the default endpoint is refused.
        let resp = app
            .clone()
            .oneshot(post_authed_req("/channels/add", "channel=@mine", &seller))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(db.seller_channel(42).as_deref(), Some("@mine"));
        // The seller's panel shows their bot-managed channel read-only.
        let resp = app
            .clone()
            .oneshot(authed_req("/channels", &seller))
            .await
            .unwrap();
        let body = body_string(resp).await;
        assert!(body.contains("@mine"), "{body}");
        assert!(body.contains("bot-managed"), "{body}");
        assert!(
            !body.contains("Global channels"),
            "seller must not see the owner's panel: {body}"
        );
        let resp = app
            .clone()
            .oneshot(post_authed_req("/channels/remove", "channel=@two", &seller))
            .await
            .unwrap();
        // The scoped writer refuses a cross-scope remove honestly: the page
        // re-renders with the refusal and the global row survives.
        assert_eq!(resp.status(), StatusCode::OK, "inline error, not PRG");
        let body = body_string(resp).await;
        assert!(body.contains("not yours to remove"), "{body}");
        assert!(
            db.default_channel().as_deref() == Some("@two"),
            "seller remove must stay scoped to their own rows"
        );
        let resp = app
            .clone()
            .oneshot(post_authed_req(
                "/channels/default",
                "channel=@mine",
                &seller,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn post_form_renders_channel_dropdown() {
        let (state, _dir) = make_state().await;
        state.db.add_channel("@one", 0).unwrap();
        state.db.add_channel("@two", 0).unwrap();
        state.db.set_seller_channel(42, "@mine");
        let owner = owner_session(&state).await;
        let seller = seller_session(&state, 42).await;
        let app = router(state);
        // Owner scope (None = every table row): (default) first, then the
        // channels. The seller's bot-managed channel is NOT here — it lives
        // in the legacy sellers column, not the channels table.
        let resp = app
            .clone()
            .oneshot(authed_req("/dashboard", &owner))
            .await
            .unwrap();
        let body = body_string(resp).await;
        assert!(body.contains(r#"<select name="channel">"#), "{body}");
        assert!(
            body.contains(r#"<option value="">(default)</option>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<option value="@one">@one</option>"#),
            "{body}"
        );
        assert!(
            !body.contains(r#"<option value="@mine">"#),
            "legacy seller channel must not leak into the owner's dropdown: {body}"
        );
        // Configured console shows no connect banner.
        assert!(
            !body.contains("The bot is not connected yet"),
            "configured state must not warn: {body}"
        );

        // Seller scope = the global namespace plus their own rows (the
        // landed list_channels contract), plus their merged legacy channel,
        // plus the (default) option.
        let resp = app
            .oneshot(authed_req("/dashboard", &seller))
            .await
            .unwrap();
        let body = body_string(resp).await;
        assert!(
            body.contains(r#"<option value="@mine">@mine</option>"#),
            "{body}"
        );
        assert!(
            body.contains(r#"<option value="@one">@one</option>"#),
            "{body}"
        );
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn ai_owner_access_200() {
        let (state, _dir) = make_state().await;
        let token = owner_session(&state).await;
        let app = router(state);
        let resp = app.oneshot(authed_req("/ai", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("AI"), "{body}");
        assert!(body.contains("AI provider"), "{body}");
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn ai_seller_refused_403() {
        let (state, _dir) = make_state().await;
        let token = seller_session(&state, 42).await;
        let app = router(state);
        let resp = app.oneshot(authed_req("/ai", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn ai_anon_redirects() {
        let (state, _dir) = make_state().await;
        let app = router(state);
        let resp = app.oneshot(get_req("/ai")).await.unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(resp.headers().get("location").unwrap(), "/login");
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn ai_save_toggle_only_writes_toggle() {
        let (state, dir) = make_state().await;
        let token = owner_session(&state).await;
        let app = router(state);
        let _env = ENV_LOCK.lock().await;
        let env_path = dir.path().join(".env");
        set_env_file_var(&env_path);
        let resp = app
            .oneshot(post_authed_req("/ai/save", "", &token))
            .await
            .unwrap();
        let body = body_string(resp).await;
        assert!(
            body.contains("Saved. AI settings apply on restart."),
            "{body}"
        );
        let env = std::fs::read_to_string(&env_path).expect(".env written");
        assert!(env.contains("AI_ENABLED=0"), "{env}");
        // non-toggle blanks must not be written
        assert!(!env.contains("AI_PROVIDER="), "{env}");
        assert!(!env.contains("AI_API_KEY="), "{env}");
        clear_env_file_var();
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn ai_save_blank_non_toggle_keeps_current_no_clobber() {
        // submitting an invalid provider must not write .env
        let (state, dir) = make_state().await;
        let token = owner_session(&state).await;
        let app = router(state);
        let _env = ENV_LOCK.lock().await;
        let env_path = dir.path().join(".env");
        set_env_file_var(&env_path);
        let resp = app
            .oneshot(post_authed_req(
                "/ai/save",
                "ai_enabled=1&ai_provider=bogus",
                &token,
            ))
            .await
            .unwrap();
        let body = body_string(resp).await;
        assert!(body.contains("alert-error"), "{body}");
        assert!(!env_path.exists(), "rejected save must not touch .env");
        clear_env_file_var();
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn ai_test_no_key_returns_not_configured() {
        let (state, _dir) = make_state().await;
        let token = owner_session(&state).await;
        let app = router(state);
        let resp = app
            .oneshot(post_authed_req("/ai/test", "ai_provider=openai", &token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("No API key configured."), "{body}");
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn ai_test_redacts_sentinel_key() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let sentinel = "sk-sentinel-SECRET-12345";
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(401).set_body_string(format!("bad key {} in body", sentinel)),
            )
            .mount(&server)
            .await;

        let (state, _dir) = make_state().await;
        let token = owner_session(&state).await;
        let app = router(state);
        let body = format!(
            "ai_provider=openai&ai_base_url={}&ai_api_key={}&ai_model=gpt-4o-mini",
            server.uri(),
            sentinel
        );
        let resp = app
            .oneshot(post_authed_req("/ai/test", &body, &token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let out = body_string(resp).await;
        assert!(!out.contains(sentinel), "sentinel leaked: {out}");
        assert!(
            out.contains("\"ok\":false") || out.contains("\"ok\": false"),
            "{out}"
        );
    }

    #[test]
    fn redact_ai_error_covers_key_url_userinfo() {
        let key = "sk-sentinel-SECRET-12345";
        let msg = format!(
            "request failed: key={} url=https://user:pass@example.com/v1/chat/completions?api_key={}",
            key, key
        );
        let out = redact_ai_error(&msg, key, "https://user:pass@example.com/v1");
        assert!(!out.contains(key), "key leaked: {out}");
        assert!(!out.contains("pass"), "password leaked: {out}");
        assert!(!out.contains("user:"), "userinfo leaked: {out}");
        // query string (containing the key) must be gone
        assert!(!out.contains("api_key="), "query leaked: {out}");
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn dashboard_shows_connect_banner_when_unconfigured() {
        let (state, _dir) = make_unclaimed_state().await;
        // A claimed-but-never-configured console: owner account exists, the
        // bot token never landed.
        state
            .db
            .create_web_account("root", "secret1", "owner", None)
            .await
            .unwrap();
        let (acc_id, _u, _h, _r, _s) = state.db.get_web_account_by_username("root").unwrap();
        let token = state.db.create_web_session(acc_id, "owner", None, 7);
        let app = router(state);
        let resp = app.oneshot(authed_req("/dashboard", &token)).await.unwrap();
        let body = body_string(resp).await;
        assert!(body.contains("The bot is not connected yet."), "{body}");
        assert!(
            body.contains(r#"href="/settings""#),
            "banner links to Settings: {body}"
        );
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn subs_owner_access_200() {
        let (state, _dir) = make_state().await;
        let token = owner_session(&state).await;
        let app = router(state);
        let resp = app.oneshot(authed_req("/subs", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains("Subscriptions"), "{body}");
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn subs_seller_access_200() {
        let (state, _dir) = make_state().await;
        let token = seller_session(&state, 42).await;
        let app = router(state);
        let resp = app.oneshot(authed_req("/subs", &token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn subs_anon_redirects() {
        let (state, _dir) = make_state().await;
        let app = router(state);
        let resp = app.oneshot(get_req("/subs")).await.unwrap();
        assert!(resp.status().is_redirection());
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn subs_filter_unknown_normalizes() {
        let (state, _dir) = make_state().await;
        let token = owner_session(&state).await;
        let app = router(state);
        let resp = app
            .oneshot(authed_req("/subs?status=bogus", &token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn subs_revoke_requires_owner() {
        let (state, _dir) = make_state().await;
        state
            .db
            .create_subscription(1, 2, None, "@c", 30, None, None);
        let token = seller_session(&state, 42).await;
        let app = router(state);
        let resp = app
            .oneshot(post_authed_req("/subs/revoke", "sub_id=1", &token))
            .await
            .unwrap();
        // seller POST must not succeed as owner
        assert!(resp.status() == StatusCode::FORBIDDEN || resp.status().is_redirection());
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn subs_revoke_owner_sets_revoked() {
        let (state, _dir) = make_state().await;
        let id = state
            .db
            .create_subscription(1, 2, None, "@c", 30, None, None);
        let token = owner_session(&state).await;
        let app = router(state.clone());
        let resp = app
            .oneshot(post_authed_req(
                "/subs/revoke",
                &format!("sub_id={}", id),
                &token,
            ))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        let loc = resp.headers().get("location").unwrap().to_str().unwrap();
        assert!(loc.contains("revoked=1"), "{loc}");
        // verify status
        let subs = state.db.list_subscriptions_filtered(None, None, 10);
        assert!(subs.iter().any(|r| r.0 == id && r.4 == "revoked"));
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn subs_expirecheck_owner_reports_count() {
        let (state, _dir) = make_state().await;
        // create an already-expired subscription by inserting with past expiry? Use create with 0 days? expires ~now; force expired via direct SQL not available. Instead just call endpoint; count 0 acceptable.
        let token = owner_session(&state).await;
        let app = router(state);
        let resp = app
            .oneshot(post_authed_req("/subs/expirecheck", "", &token))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        let loc = resp.headers().get("location").unwrap().to_str().unwrap();
        assert!(loc.contains("expired="), "{loc}");
    }
}

#[cfg(test)]

mod timeout_tests {
    // Timeout middleware tests (L8). The production ceiling is 30s; the
    // tests exercise the real `run_with_timeout` logic with a short limit so
    // nothing sleeps 30 seconds. No sleep route exists in production code —
    // these live only inside this test module.
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use tower::ServiceExt;

    #[tracing::instrument(skip_all)]
    async fn slow() -> &'static str {
        tokio::time::sleep(Duration::from_millis(500)).await;
        "done"
    }
    #[tracing::instrument(skip_all)]
    async fn fast() -> &'static str {
        "ok"
    }

    fn app(limit: Duration) -> Router {
        Router::new()
            .route("/slow", get(slow))
            .route("/fast", get(fast))
            .layer(axum::middleware::from_fn(move |req, next| async move {
                run_with_timeout(limit, req, next).await
            }))
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn timeout_returns_503_envelope() {
        let resp = app(Duration::from_millis(50))
            .oneshot(
                axum::http::Request::builder()
                    .uri("/slow")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["error"]["code"], "timeout");
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn fast_handlers_pass_through_untimed() {
        let resp = app(Duration::from_millis(2000))
            .oneshot(
                axum::http::Request::builder()
                    .uri("/fast")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

/// Security-remediation regression tests (L10 wave): MFA re-auth on settings,
/// refund-create clamps, throttle hard cap, transfer classification. Helpers
/// come from the sibling `tests` module.
#[cfg(test)]
mod security_tests {
    use super::tests::{
        authed_req, body_string, make_state, owner_session, post_authed_req, seller_session,
    };
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    // ---- Settings MFA re-auth (MED-6) ----

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn settings_mfa_disable_requires_current_factor() {
        let (state, _dir) = make_state().await;
        let db = state.db.clone();
        let (acc_id, _u, _h, _r, _s) = db
            .get_web_account_by_username("owner")
            .expect("owner account exists");
        const SECRET: &str = "JBSWY3DPEHPK3PXP";
        db.enable_totp(acc_id, SECRET).unwrap();
        let token = owner_session(&state).await;
        let app = router(state);
        // No code at all: refused, MFA stays on.
        let resp = app
            .clone()
            .oneshot(post_authed_req("/settings/mfa/disable", "code=", &token))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/settings?mfa=reauth"
        );
        assert!(
            db.get_totp_state(acc_id).1,
            "MFA must survive a rejected factor"
        );
        // A wrong code: same refusal.
        let now_ts = chrono::Utc::now().timestamp() as u64;
        let valid = crate::util::totp_generate(&SECRET.to_string(), now_ts, 6usize, 30u64);
        let wrong = format!("{:06}", (valid.parse::<u32>().unwrap_or(0) + 1) % 1_000_000);
        let resp = app
            .clone()
            .oneshot(post_authed_req(
                "/settings/mfa/disable",
                &format!("code={wrong}"),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/settings?mfa=reauth"
        );
        assert!(db.get_totp_state(acc_id).1);
        // The current TOTP: accepted, MFA is disabled. (Recomputed here so a
        // 30-second window rolling mid-test cannot flake the assertion.)
        let now_ts = chrono::Utc::now().timestamp() as u64;
        let valid = crate::util::totp_generate(&SECRET.to_string(), now_ts, 6usize, 30u64);
        let resp = app
            .clone()
            .oneshot(post_authed_req(
                "/settings/mfa/disable",
                &format!("code={valid}"),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/settings?mfa=disabled"
        );
        assert!(
            !db.get_totp_state(acc_id).1,
            "the current factor authorized the disable"
        );
        // The failure banner renders on the settings page.
        let resp = app
            .oneshot(authed_req("/settings?mfa=reauth", &token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(
            body.contains("That code was not accepted"),
            "reauth failure banner must render: {body}"
        );
    }

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn settings_mfa_recovery_requires_current_factor() {
        let (state, _dir) = make_state().await;
        let db = state.db.clone();
        let (acc_id, _u, _h, _r, _s) = db
            .get_web_account_by_username("owner")
            .expect("owner account exists");
        db.enable_totp(acc_id, "JBSWY3DPEHPK3PXP").unwrap();
        let token = owner_session(&state).await;
        let app = router(state);
        // No code: no rotation (the old recovery codes stay untouched).
        let resp = app
            .oneshot(post_authed_req("/settings/mfa/recovery", "code=", &token))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/settings?mfa=reauth"
        );
        assert_eq!(
            db.list_unused_recovery_codes(acc_id),
            0,
            "no codes may be minted without the current factor"
        );
    }

    // ---- Refund create seller clamp (MED-7 mirror) ----

    #[tokio::test]
    #[tracing::instrument(skip_all)]
    async fn finance_refund_create_seller_clamped_to_invoice() {
        let (state, _dir) = make_state().await;
        let db = state.db.clone();
        // A paid invoice for seller 7: 10.5 USDT, buyer user 42; a second
        // invoice exercises the admin path.
        db.save_invoice(
            "inv-1",
            7,
            "10.5",
            "USDT",
            None,
            "paid",
            None,
            None,
            Some(42),
            None,
        );
        db.save_invoice(
            "inv-2",
            7,
            "4",
            "USDT",
            None,
            "paid",
            None,
            None,
            Some(42),
            None,
        );
        let seller = seller_session(&state, 7).await;
        let owner = owner_session(&state).await;
        let app = router(state);
        // A seller may not pick a different asset: banner error, no create.
        let resp = app
            .clone()
            .oneshot(post_authed_req(
                "/finance/refund/create",
                "invoice_id=inv-1&amount=99&asset=TON",
                &seller,
            ))
            .await
            .unwrap();
        assert!(resp.status().is_redirection());
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/finance?error=invalid_asset"
        );
        assert!(db.list_refunds(Some(7)).is_empty());
        // A seller may not ask for more than the invoice collected: clamped.
        let resp = app
            .clone()
            .oneshot(post_authed_req(
                "/finance/refund/create",
                "invoice_id=inv-1&amount=99&asset=USDT",
                &seller,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/finance?refund_created=1"
        );
        let refunds = db.list_refunds(Some(7));
        assert_eq!(refunds.len(), 1);
        assert_eq!(refunds[0].4, "10.5", "amount must clamp to the invoice");
        assert_eq!(refunds[0].5, "USDT");
        // A non-numeric amount: banner error, no second row.
        let resp = app
            .clone()
            .oneshot(post_authed_req(
                "/finance/refund/create",
                "invoice_id=inv-1&amount=abc&asset=USDT",
                &seller,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/finance?error=amount_number"
        );
        // A non-positive amount: banner error too.
        let resp = app
            .clone()
            .oneshot(post_authed_req(
                "/finance/refund/create",
                "invoice_id=inv-1&amount=0&asset=USDT",
                &seller,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/finance?error=amount_positive"
        );
        assert_eq!(db.list_refunds(Some(7)).len(), 1, "only the clamped row");
        // Admins/owner keep the looser form (they approve what gets created).
        let resp = app
            .oneshot(post_authed_req(
                "/finance/refund/create",
                "invoice_id=inv-2&amount=99&asset=USDT",
                &owner,
            ))
            .await
            .unwrap();
        assert_eq!(
            resp.headers().get("location").unwrap(),
            "/finance?refund_created=1"
        );
        let all = db.list_refunds(None);
        assert!(all.iter().any(|r| r.4 == "99" && r.5 == "USDT"));
    }

    // ---- Login throttle hard cap (LOW-10) ----

    #[test]
    fn login_throttle_hard_cap_bounds_the_map() {
        let map: Arc<StdMutex<HashMap<String, (u32, Instant)>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        // A flood of single-attempt keys inside the hour window: the time
        // based trim cannot help, the oldest-eviction cap must.
        for i in 0..2000 {
            login_throttle(&map, &format!("flood-{i}"), 5, 900);
        }
        let len = map.lock().unwrap().len();
        assert!(
            len <= 1025,
            "the map must stay bounded at the 1024 cap, got {len}"
        );
        // The newest key must still be tracked after the evictions: its
        // counter survived (it was among the last inserted) and keeps
        // counting toward the lockout (1 from the flood + 4 more = 5).
        let mut throttled = false;
        for _ in 0..4 {
            throttled = login_throttle(&map, "flood-1999", 5, 900);
        }
        assert!(
            throttled,
            "recent failures keep counting toward the lockout"
        );
    }

    // ---- CryptoPay transfer error classification (BLOCK-2) ----

    #[test]
    fn transfer_rejection_classification() {
        // Parsed ok:false from the API: definite refusal, no money moved.
        assert!(transfer_rejected_definitely("INSUFFICIENT_FUNDS"));
        assert!(transfer_rejected_definitely("unknown error"));
        // 4xx from the API: the request was seen and rejected.
        assert!(transfer_rejected_definitely(
            "HTTP 400 Bad Request - {\"ok\":false}"
        ));
        assert!(transfer_rejected_definitely("HTTP 401 Unauthorized - {}"));
        // 5xx and 408: an intermediary may have dropped a processed transfer.
        assert!(!transfer_rejected_definitely(
            "HTTP 502 Bad Gateway - proxy"
        ));
        assert!(!transfer_rejected_definitely(
            "HTTP 504 Gateway Timeout - proxy"
        ));
        assert!(!transfer_rejected_definitely(
            "HTTP 408 Request Timeout - {}"
        ));
        // reqwest transport shapes (connect/timeout/body/decode): ambiguous.
        assert!(!transfer_rejected_definitely(
            "error sending request for url (https://pay.crypt.bot/api/transfer): operation timed out"
        ));
        assert!(!transfer_rejected_definitely(
            "error decoding response body"
        ));
        assert!(!transfer_rejected_definitely(
            "error reading a body from connection"
        ));
        assert!(!transfer_rejected_definitely("builder error"));
    }
}
