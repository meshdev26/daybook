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

use crate::identity::{IdentityError, IdentityState, IdentityStatus, NormalizedMessage};
use crate::{config::Config, db::Db, util::ct_eq};

// Re-export auth helpers moved to `crate::auth` so existing
// `crate::web::*` call sites keep working.
pub use crate::auth::*;

// ---- submodule declarations (move-only split) ----
mod admin_console;
mod ai;
mod ai_actions;
mod analytics;
mod auth_pages;
mod bookings;
mod bots_add;
mod bots_admin;
mod bots_config;
mod cart;
mod channel_ops;
mod channels;
mod channels_manage;
mod channels_rows;
mod codes;
mod common_pages;
mod content;
mod customers;
mod dashboard;
mod finance;
mod finance_actions;
mod html;
mod identity;
mod keys_setup;
mod mfa_users;
mod monitor;
mod promos;
mod referrals;
mod settings;
mod settings_tokens;
mod subs;
mod wizard;

#[cfg(test)]
mod identity_tests;
#[cfg(test)]
mod security_tests;
#[cfg(test)]
mod smoke_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod timeout_tests;

// ---- glob-import moved items so router() and shared helpers resolve unqualified ----
use admin_console::*;
use ai::*;
use ai_actions::*;
use analytics::*;
use auth_pages::*;
use bookings::*;
use bots_add::*;
use bots_admin::*;
use bots_config::*;
use cart::*;
use channel_ops::*;
use channels::*;
use channels_manage::*;
use channels_rows::*;
use codes::*;
use common_pages::*;
use content::*;
use customers::*;
use dashboard::*;
use finance::*;
use finance_actions::*;
use html::*;
use identity::*;
use keys_setup::*;
use mfa_users::*;
use monitor::*;
use promos::*;
use referrals::*;
use settings::*;
use settings_tokens::*;
use subs::*;
use wizard::*;

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
    pub bots: Arc<crate::bots::BotManager>,
    /// Personal-account identity actor handle (user-identity Phase 1).
    /// `IdentityHandle::unconnected()` where the subsystem is not spawned.
    pub identity: crate::identity::IdentityHandle,
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
        crate::auth::cookie_path(self)
    }
}

/// Authenticated console session. `selected_bot_id` is the global owner's
/// bot-scoping choice (None = primary/all); sellers and admins are pinned
/// to their resolved bot and never carry a selection.
#[derive(Clone)]
pub struct Session {
    pub account_id: i64,
    pub role: String,
    pub seller_id: Option<i64>,
    pub selected_bot_id: Option<i64>,
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

/// Authoritative first-run gate: the `setup_done` DB flag written by
/// `/setup` wins over the startup-frozen env snapshot, so a completed setup
/// cannot be replayed before a restart.
fn setup_done(state: &WebState) -> bool {
    state.db.get_config("setup_done").is_some() || state.cfg.is_configured()
}

pub fn router(state: WebState) -> Router {
    let state_for_layer = state.clone();
    let r = Router::new();
    let r = routes_core(r);
    let r = routes_channels(r);
    let r = routes_content(r);
    let r = routes_commerce(r);
    let r = routes_support(r);
    let r = routes_finance(r);
    let r = routes_admin(r);
    let r = routes_mfa_posts(r);
    let r = routes_identity_bots(r);
    let r = r
        .nest_service("/static", ServeDir::new("static"))
        .fallback(not_found);
    r.with_state(state)
        // Identity status snapshot middleware (user-identity Phase 1): the
        // console shell (`html_page`) is sync and cannot await the actor, so
        // this refreshes the process-wide snapshot before every request and
        // the shell reads it synchronously. See `IDENTITY_SNAPSHOT`.
        .layer(axum::middleware::from_fn_with_state(
            state_for_layer,
            identity_status_refresh,
        ))
}

fn routes_core(r: Router<WebState>) -> Router<WebState> {
    r.route("/", get(index))
        .route("/assets/app.css", get(app_css))
        .route("/assets/app.js", get(app_js))
        .route("/assets/fonts/IBMPlexSans-var.woff2", get(font_sans))
        .route("/assets/fonts/IBMPlexMono-400.woff2", get(font_mono_400))
        .route("/assets/fonts/IBMPlexMono-500.woff2", get(font_mono_500))
        .route("/setup", get(setup_page).post(setup_submit))
        .route("/setup/skip", post(setup_skip))
        .route(
            "/setup/claim",
            get(setup_claim_page).post(setup_claim_submit),
        )
        .route("/login", get(login_page).post(login_submit))
        // SETTINGS_PER_BOT_TOKEN_SPEC §7: the Management panel posts its
        // "Sign out" form here; the GET arm stays for bookmarks and the
        // existing route test. Same method-agnostic handler both ways.
        .route("/logout", get(logout).post(logout))
        .route("/dashboard", get(dashboard))
        .route("/settings", get(settings_page))
        .route("/settings/save", post(settings_save))
        .route("/settings/password", post(settings_password))
        .route("/settings/sidebar", post(settings_sidebar))
        .route("/settings/restart", post(settings_restart))
        .route("/channels", get(channels_page))
        .route("/channels/add", get(channels_add_page).post(channels_add))
        .route("/ai", get(ai_page))
        .route("/ai/save", post(ai_save))
        .route("/ai/test", post(ai_test))
        .route("/usage/export", get(usage_export))
}

fn routes_channels(r: Router<WebState>) -> Router<WebState> {
    r.route("/channels/default", post(channels_default))
        .route("/channels/manage", get(channels_manage_page))
        .route("/channels/name", post(channels_name))
        .route("/channels/remove", post(channels_remove))
        .route("/channels/rename", post(channels_rename))
        .route("/channels/bind-sender", post(channels_bind_sender))
}

fn routes_content(r: Router<WebState>) -> Router<WebState> {
    r.route("/plans", get(plans_page))
        .route("/posts", get(posts_page))
        .route("/post", post(create_post))
        .route("/plan", post(create_plan))
        .route("/plandel", post(delete_plan_by_name))
        .route("/plans/{id}/delete", post(delete_plan_by_id))
        .route("/products", get(products_page))
        .route("/product", post(create_product))
        // The pass's one new route (EDIT_FLOW_SPEC E2): reactivation removes
        // the one-way dead end; reuses Db::set_product_active(true).
        .route("/product/activate", post(product_activate))
        .route("/analytics", get(analytics_page))
        .route("/analytics/export", get(analytics_export))
        .route("/subs", get(subs_page))
        .route("/subs/export", get(subs_export))
}

fn routes_commerce(r: Router<WebState>) -> Router<WebState> {
    r.route("/subs/revoke", post(subs_revoke))
        .route("/subs/expirecheck", post(subs_expirecheck))
        .route("/subs/grant", post(subs_grant))
        .route("/subs/extend", post(subs_extend))
        // EDIT_FLOW_SPEC E10: POST-only — the GET arm let a linkable,
        // prefetchable URL kick up to 1000 buyers out of their channels.
        .route("/subs/bulkrevoke", post(subs_bulkrevoke))
        .route("/codes", get(codes_page))
        .route("/codes/generate", post(codes_generate))
        .route("/codes/delete", post(codes_delete))
        .route("/codes/reclaim", post(codes_reclaim))
        .route("/promos", get(promos_page).post(promos_create))
        .route("/promos/revoke", post(promos_revoke))
        .route("/promos/delete", post(promos_revoke))
        .route("/referrals", get(referrals_page))
        .route("/customers", get(customers_page))
        .route("/cart", get(cart_page))
        .route("/cart/add", post(cart_add))
        .route("/cart/remove", post(cart_remove))
        .route("/cart/clear", post(cart_clear_post))
        .route("/carts", get(carts_page))
}

fn routes_support(r: Router<WebState>) -> Router<WebState> {
    r.route("/tickets", get(tickets_page))
        .route("/tickets/reply", post(tickets_reply))
        .route("/tickets/status", post(tickets_status))
        .route("/orders", get(orders_page))
        .route("/orders/status", post(orders_status))
        .route("/reviews", get(reviews_page))
        .route("/reviews/approve", post(reviews_approve))
        .route("/bookings", get(bookings_page))
        .route("/bookings/status", post(bookings_status))
        .route("/bookings/cancel", post(bookings_cancel))
}

fn routes_finance(r: Router<WebState>) -> Router<WebState> {
    r.route("/finance", get(finance_page))
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
}

fn routes_admin(r: Router<WebState>) -> Router<WebState> {
    r.route("/admins", get(admins_page))
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
        .route("/privacy", get(privacy_page))
        .route("/privacy/export", get(privacy_export))
        .route("/privacy/delete", post(privacy_delete))
}

fn routes_mfa_posts(r: Router<WebState>) -> Router<WebState> {
    r.route("/mfa/verify", get(mfa_verify_page).post(mfa_verify_submit))
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
}

fn routes_identity_bots(r: Router<WebState>) -> Router<WebState> {
    r.route("/identity", get(identity_page))
        .route("/identity/unlock", post(identity_unlock))
        .route("/identity/enable-writes", post(identity_enable_writes))
        .route("/identity/disconnect", post(identity_disconnect))
        .route("/identity/wipe", post(identity_wipe))
        .route("/identity/wizard", get(identity_wizard))
        .route("/identity/wizard/accept", post(identity_wizard_accept))
        .route(
            "/identity/wizard/credentials",
            post(identity_wizard_credentials),
        )
        .route("/identity/wizard/phone", post(identity_wizard_phone))
        .route("/identity/wizard/code", post(identity_wizard_code))
        .route("/identity/wizard/password", post(identity_wizard_password))
        .route(
            "/identity/wizard/passphrase",
            post(identity_wizard_passphrase),
        )
        .route("/monitor", get(monitor_page))
        .route("/bot/select", post(bot_select))
        .route("/bots", get(bots_page).post(bots_save))
        .route("/bots/add", get(bots_add_page).post(bots_add_submit))
        .route("/bots/add/validate", post(bots_add_validate))
        .route("/bots/add/snippet", get(bots_add_snippet))
        .route("/bots/{id}/disable", post(bots_disable))
        .route("/bots/{id}/delete", post(bots_delete))
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

/// The reveal toggle that sits inside every `.secret-field` wrapper (login,
/// claim, setup, settings, AI, crypto, web users). One shared markup, one
/// behavior (APP_JS): a real `<button type="button">` so Enter/Space work and
/// it can never submit the surrounding form. The two stroke-drawn icons ship
/// inline (the PLANE_SVG idiom, currentColor) and swap on the button's
/// `aria-pressed` state, which also carries the state to screen readers.
const SECRET_TOGGLE: &str = r#"<button type="button" class="secret-toggle" aria-label="Show password" aria-pressed="false"><svg class="icon-eye" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M1 12s4-8 11-8 11 8 11 8-4 8-11 8-11-8-11-8z"/><circle cx="12" cy="12" r="3"/></svg><svg class="icon-eye-off" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19m-6.72-1.07a3 3 0 1 1-4.24-4.24"/><line x1="1" y1="1" x2="23" y2="23"/></svg></button>"#;

/// Resolve the effective bot context for a web session.
pub fn effective_bot(state: &WebState, session: &Session) -> Arc<crate::bots::BotContext> {
    if session.role == "seller" {
        if let Some(sid) = session.seller_id {
            return state.bots.for_seller(sid);
        }
        return state.bots.primary.clone();
    }
    if session.role == "admin" {
        if let Some(bid) = state.db.get_admin_bot_id(session.account_id)
            && let Some(ctx) = state.bots.get(bid)
        {
            return ctx;
        }
        return state.bots.primary.clone();
    }
    // global owner (or other privileged): honor selection
    if let Some(bid) = session.selected_bot_id
        && let Some(ctx) = state.bots.get(bid)
    {
        return ctx;
    }
    state.bots.primary.clone()
}

/// Seller ids covered by the session's bot scope: `None` = no filter (the
/// global owner sees all bots), `Some(set)` = the owner is viewing one
/// selected bot and only rows owned by those sellers are in scope. Sellers
/// and admins never reach this — their queries are already seller-scoped.
fn bot_scope_sellers(state: &WebState, session: &Session) -> Option<Vec<i64>> {
    if session.role != "owner" {
        return None;
    }
    session
        .selected_bot_id
        .map(|bid| state.db.sellers_for_bot(bid))
}

/// Membership test against `bot_scope_sellers`: `None` scope admits
/// everything, `Some(set)` admits only rows owned by those sellers.
fn in_bot_scope(scope: &Option<Vec<i64>>, seller_id: i64) -> bool {
    scope.as_ref().is_none_or(|set| set.contains(&seller_id))
}

/// A bot's console display name: the saved name when one is set, else its
/// key ('' name means "show the key instead"). Display-only sugar — identity
/// stays the key everywhere copy must address the bot (banners, confirms).
fn bot_display_name(b: &crate::db::BotRecord) -> &str {
    if b.name.is_empty() { &b.key } else { &b.name }
}

/// Bot scope control for the console shell: a real form (select + submit)
/// for the global owner — `onchange` submit is progressive enhancement and
/// the button keeps it working without JS — and a pinned, plain-text label
/// for roles that cannot switch (their bot is resolved server-side).
fn bot_scope_control(state: &WebState, session: Option<&Session>) -> String {
    let Some(s) = session else {
        return String::new();
    };
    if s.role == "seller" || s.role == "admin" {
        let ctx = effective_bot(state, s);
        return format!(
            r#"<div class="bot-scope bot-scope-pinned" aria-label="Active bot"><span class="help">Bot</span><span class="mono">{}</span></div>"#,
            esc(&ctx.key)
        );
    }
    if s.role != "owner" {
        return String::new();
    }
    let bots = state.db.list_bots_enabled();
    let mut options = r#"<option value=""{sel0}>All bots (primary)</option>"#.to_string();
    if s.selected_bot_id.is_none() {
        options = options.replace("{sel0}", " selected");
    } else {
        options = options.replace("{sel0}", "");
    }
    for b in &bots {
        options.push_str(&format!(
            r#"<option value="{}"{}>{}{}</option>"#,
            b.id,
            if s.selected_bot_id == Some(b.id) {
                " selected"
            } else {
                ""
            },
            esc(bot_display_name(b)),
            b.username
                .as_deref()
                .map(|u| format!(" · {}", esc(u)))
                .unwrap_or_default()
        ));
    }
    format!(
        r#"<form method="post" action="{action}" class="bot-scope"><label for="bot-scope-select">Bot</label><select id="bot-scope-select" name="bot_id" onchange="this.form.submit()">{options}</select><button type="submit" class="btn btn-ghost btn-sm">Switch</button></form>"#,
        action = state.url("/bot/select"),
        options = options
    )
}

/// Mirrors `api.rs`'s `is_admin_role`: admins and owners share the
/// privileged tier, sellers are scoped to their own rows. One definition of
/// the tier so console pages and REST endpoints can never drift apart.
fn is_admin_role(role: &str) -> bool {
    role == "admin" || role == "owner"
}

/// Normalizes a console channel target for owner comparison: trimmed, no
/// leading `@`, ASCII-lowercased. Stored channel names may be `@handles` or
/// numeric chat ids, so the comparison tolerates both forms — the value that
/// is actually sent to Telegram is never rewritten.
fn channel_key(channel: &str) -> String {
    channel.trim().trim_start_matches('@').to_ascii_lowercase()
}

/// Whether the `channels` row for this target belongs to `seller_id`, keyed
/// exactly as stored first and then in normalized form.
fn owns_channel_row(state: &WebState, seller_id: i64, chat_id: &str) -> bool {
    state
        .db
        .get_channel_seller_id(chat_id.trim())
        .or_else(|| state.db.get_channel_seller_id(&channel_key(chat_id)))
        == Some(seller_id)
}

/// Tenant gate for every channel-addressed console mutation (post edits,
/// deletes, pins, the channel title/description, the schedule destination).
///
/// Owner and admin are the global privileged tier and keep the whole console.
/// A seller may address only a channel they own: a `channels` row scoped to
/// them, or their own `sellers.default_channel` (what `/setchannel` writes) —
/// compared case-insensitively so `@Mine` and `mine` are one channel.
/// Everything else (no seller profile, unknown channel, another tenant's
/// channel) fails closed.
///
/// Callers MUST run this before any Bot API call or DB mutation: the bot
/// bound to the target channel is what performs the mutation, so this check is
/// what stops tenant A's session from driving tenant B's bot (review H1).
fn may_mutate_channel(state: &WebState, session: &Session, chat_id: &str) -> bool {
    if session.role == "owner" || is_admin_role(&session.role) {
        return true;
    }
    let key = channel_key(chat_id);
    if key.is_empty() {
        return false;
    }
    let Some(sid) = session.seller_id else {
        return false;
    };
    if owns_channel_row(state, sid, chat_id) {
        return true;
    }
    state
        .db
        .seller_channel(sid)
        .map(|own| channel_key(&own))
        .is_some_and(|own| !own.is_empty() && own == key)
}

/// Post-row ownership for the unscoped `update_post_content` / `delete_post`
/// mutations: owner/admin pass, a seller must own the row, and a `post_id`
/// that does not exist is a denial for a seller (fail closed).
fn may_mutate_post(state: &WebState, session: &Session, post_id: Option<i64>) -> bool {
    if session.role == "owner" || is_admin_role(&session.role) {
        return true;
    }
    let Some(pid) = post_id else {
        return true;
    };
    match (session.seller_id, state.db.get_post_seller(pid)) {
        (Some(sid), Some(owner)) => owner == sid,
        _ => false,
    }
}

/// Composer target gate (`create_post`). The dashboard's channel dropdown
/// offers a seller the global set plus their own channel (`Db::list_channels`)
/// and the resolved default usually lives in the global set, so a seller may
/// address a global row or a channel they own — never another seller's
/// channel (review H1: that send is executed by the victim channel's bot).
fn may_post_to_channel(state: &WebState, session: &Session, chat_id: &str) -> bool {
    if may_mutate_channel(state, session, chat_id) {
        return true;
    }
    let key = channel_key(chat_id);
    if key.is_empty() {
        return false;
    }
    owns_channel_row(state, 0, chat_id)
}
