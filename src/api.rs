use crate::handlers::money::{
    GuardedTransferOutcome, execute_guarded_transfer, reconciliation_cutoffs,
};
use crate::roles::Role;
use crate::web::WebState;
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, patch, post},
};
use serde::{Deserialize, Serialize};
use teloxide::payloads::{
    CreateChatInviteLinkSetters, SetChatDescriptionSetters, UnpinChatMessageSetters,
};
use teloxide::types::ChatMemberKind;
use teloxide::{prelude::Requester, types::MessageId};

// Re-export WebState alias for convenience

#[derive(Serialize)]
struct ApiError {
    error: ApiErrorBody,
}
#[derive(Serialize)]
struct ApiErrorBody {
    code: String,
    message: String,
}

fn err(status: StatusCode, code: &str, msg: &str) -> (StatusCode, Json<ApiError>) {
    (
        status,
        Json(ApiError {
            error: ApiErrorBody {
                code: code.to_string(),
                message: msg.to_string(),
            },
        }),
    )
}

fn session_or_401(
    state: &WebState,
    headers: &HeaderMap,
) -> Result<crate::web::Session, (StatusCode, Json<ApiError>)> {
    crate::auth::session_from_headers(state, headers).ok_or_else(|| {
        err(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing or invalid session",
        )
    })
}

/// Tenant scope for the REST surface, resolved from the server-side session.
///
/// Returns `None` only for the platform-operator tiers (owner, and every admin
/// -- this surface has never narrowed an admin to its own binding, and this
/// change deliberately does not start). A `seller` account with no binding
/// resolves to
/// [`TenantScope::Denied`], whose `as_opt()` is the sentinel `-1` rather than
/// `None`. Before this type existed, the stored `Option` was passed through
/// verbatim, so a missing binding arrived at the `Db` layer as `None` — the
/// "all rows" argument of every seller-scoped helper — and silently widened to
/// the platform view.
///
/// The sentinel is defence in depth, not a universal denial. For the common
/// equality shape (`WHERE seller_id = ?1`) it selects no row, because seller ids
/// are Telegram user ids (positive) plus platform `0`. It cannot neutralise a
/// predicate that *filters in* other ids, so a helper that unions the platform
/// namespace or treats `None`/`Some(0)` as "global" still needs the caller to
/// refuse `Denied` at the boundary. `list_channels` is the known example:
/// `WHERE seller_id = 0 OR seller_id = ?1` returns the platform rows for any
/// argument, `-1` included. See `TenantScope::DENIED_ID` for the inventory.
fn seller_scope(session: &crate::web::Session) -> Option<i64> {
    Role::from_role_str(&session.role)
        .tenant_scope(session.seller_id)
        .as_opt()
}

/// Ownership check for a row id supplied by the caller. The denied sentinel
/// still has to be refused explicitly: `Some(-1) != -1` would otherwise let a
/// buyer name the sentinel itself and pass an equality-based gate.
fn may_act_on_seller(session: &crate::web::Session, target: i64) -> bool {
    let Some(scope) = seller_scope(session) else {
        return true;
    };
    scope != crate::roles::TenantScope::DENIED_ID && scope == target
}

/// §2: every money approval, execution, void and resolution requires the
/// Owner role (not merely Admin+). Returns the spec's `owner_required` 403.
fn owner_gate(session: &crate::web::Session) -> Result<(), (StatusCode, Json<ApiError>)> {
    if Role::from_role_str(&session.role) != Role::Owner {
        return Err(err(
            StatusCode::FORBIDDEN,
            "owner_required",
            "owner required",
        ));
    }
    Ok(())
}

/// §2 separation of duties: the human creator of a refund may never approve
/// or execute it. System-created refunds have no creator audit event and are
/// exempt from this check (they still require an Owner approver).
fn creator_gate(
    db: &crate::db::Db,
    refund_id: i64,
    session: &crate::web::Session,
) -> Result<(), (StatusCode, Json<ApiError>)> {
    if let Some((creator_id, _)) = crate::handlers::money::refund_creator(db, refund_id)
        && creator_id == session.account_id
    {
        return Err(err(
            StatusCode::FORBIDDEN,
            "creator_forbidden",
            "creator cannot approve or execute their own refund",
        ));
    }
    Ok(())
}

/// Tenant gate for channel-addressed REST mutations. Delegates to the single
/// shared policy in `handlers::channel_access` so the REST surface and the
/// console cannot drift. Run before ANY Bot API call or DB row write so one
/// tenant's session cannot drive another tenant's bot.
fn api_may_mutate_channel(state: &WebState, session: &crate::web::Session, chat_id: &str) -> bool {
    crate::handlers::channel_access::may_mutate_channel(
        &state.db,
        &session.role,
        session.seller_id,
        session.account_id,
        session.selected_bot_id,
        chat_id,
    )
}

/// Extract the invoice array from a Crypto Pay `getInvoices` response,
/// tolerating the API's envelope variations: a bare array `[...]`,
/// `{"items":[...]}`, or `{"result":{"items":[...]}}` (this crate's
/// `CryptoPay::call` already unwraps `result`, but be robust to all three
/// shapes). Returns an empty vec when no array is found.
pub fn extract_invoice_items(v: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut cur = v;
    if let Some(result) = cur.get("result") {
        cur = result;
    }
    if let Some(items) = cur.get("items").and_then(|i| i.as_array()) {
        return items.clone();
    }
    cur.as_array().cloned().unwrap_or_default()
}

// ---------- Login throttling ----------
// Shared with the web login: one failure counter per username across both
// interfaces (see `web::login_throttle` / `web::login_ok`).

// ---------- Health ----------
/// Enriched liveness probe. Backward compatible: still a 200 with the
/// original `status` field; adds `db` (SELECT 1 probe), `bot`
/// (`configured` | `setup`), `mcp` (`enabled` | `disabled`) and
/// `uptime_seconds`. No secrets or identifiers in the response.
async fn health(State(state): State<WebState>) -> impl IntoResponse {
    let db_ok = state.db.ping();
    let status = if db_ok { "ok" } else { "error" };
    let bot = if state.cfg.is_configured() {
        "configured"
    } else {
        "setup"
    };
    let mcp = if state
        .cfg
        .mcp_owner_token
        .as_deref()
        .map(|t| !t.trim().is_empty())
        .unwrap_or(false)
    {
        "enabled"
    } else {
        "disabled"
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": status,
            "db": if db_ok { "ok" } else { "error" },
            "bot": bot,
            "mcp": mcp,
            "uptime_seconds": crate::metrics::uptime_secs(),
        })),
    )
}

// ---------- Auth ----------
#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}
#[derive(Serialize)]
struct LoginResp {
    token: String,
    expires_at: String,
    account_id: i64,
    role: String,
    seller_id: Option<i64>,
}

async fn auth_login(State(state): State<WebState>, Json(f): Json<LoginReq>) -> impl IntoResponse {
    // Records this attempt up-front; a successful login clears it below.
    if crate::auth::login_throttle(
        &state.login_attempts,
        &crate::auth::login_throttle_key(&f.username),
        5,
        900,
    ) {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            "too many failed login attempts; try again later",
        )
        .into_response();
    }
    match state.db.verify_web_password(&f.username, &f.password).await {
        Some((account_id, role, seller_id)) => {
            crate::auth::login_ok(
                &state.login_attempts,
                &crate::auth::login_throttle_key(&f.username),
            );
            // A seller-tier account with no seller binding has no tenant to
            // act as (see `Role::tenant_scope`). Refuse it at the mint boundary
            // rather than issuing a session that resolves to a denial on every
            // request: the honest answer is 403, not a credential that silently
            // does nothing. These rows should no longer be creatable
            // (`create_web_account` refuses them); this covers legacy ones.
            if !crate::roles::session_is_usable(&role, seller_id) {
                return err(
                    StatusCode::FORBIDDEN,
                    "account_denied",
                    "this account has no seller binding; ask the owner to fix it",
                )
                .into_response();
            }
            // MFA parity with the web login (web::login_submit): a correct
            // password alone never mints a session for a TOTP-enabled or
            // privileged account.
            let (_totp_secret, totp_enabled, _last) = state.db.get_totp_state(account_id);
            if totp_enabled {
                // Mint a short-TTL pending token (5 minutes, stored only as a
                // sha256 like the web pending cookie); the caller exchanges it
                // plus a TOTP/recovery code at POST /api/v1/auth/mfa/verify
                // for the real session.
                let pending_raw = crate::util::gen_token(48);
                let expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
                let _ = state.db.create_pending_mfa(
                    sha256_hex(&pending_raw),
                    account_id,
                    "verify",
                    None::<String>,
                    expires,
                );
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({
                        "mfa_required": true,
                        "pending_token": pending_raw,
                    })),
                )
                    .into_response();
            }
            if Role::from_role_str(&role).has_level(Role::Admin) {
                // The web flow forces TOTP enrollment for privileged roles
                // before any session exists; enrollment is web-only tonight,
                // so REST refuses rather than lowering that bar.
                return err(
                    StatusCode::UNAUTHORIZED,
                    "mfa_required",
                    "enroll via the web console first",
                )
                .into_response();
            }
            let token = state.db.create_web_session(account_id, &role, seller_id, 7);
            let expires_at = chrono::Utc::now() + chrono::Duration::days(7);
            (
                StatusCode::OK,
                Json(
                    serde_json::to_value(LoginResp {
                        token,
                        expires_at: expires_at.to_rfc3339(),
                        account_id,
                        role,
                        seller_id,
                    })
                    .unwrap_or_default(),
                ),
            )
                .into_response()
        }
        None => err(
            StatusCode::UNAUTHORIZED,
            "invalid_credentials",
            "invalid username or password",
        )
        .into_response(),
    }
}

/// sha256 hex digest of a pending MFA token. Mirrors `web::sha256_hex` (kept
/// private there): the DB stores only the hash, the raw token only ever lives
/// in the login response.
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

#[derive(Deserialize)]
struct MfaVerifyReq {
    /// Raw pending token returned by `auth_login` when it responded
    /// `{"mfa_required": true, "pending_token": ...}`.
    pending_token: String,
    /// TOTP code, or a recovery code when enabled.
    code: String,
}

/// Exchange a pending token plus TOTP (or a recovery code when enabled) for a session.
/// Single-use token, five attempts per 15 min.
async fn auth_mfa_verify(
    State(state): State<WebState>,
    Json(f): Json<MfaVerifyReq>,
) -> impl IntoResponse {
    // Mirror web::pending_from_headers: look the token up by its sha256; the
    // raw value is never stored.
    let hash = sha256_hex(&f.pending_token);
    let Some((account_id, purpose, _secret, expires)) = state.db.get_pending_mfa(&hash) else {
        return err(
            StatusCode::UNAUTHORIZED,
            "mfa_required",
            "invalid or expired pending token",
        )
        .into_response();
    };
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&expires)
        && chrono::Utc::now() > dt.with_timezone(&chrono::Utc)
    {
        let _ = state.db.delete_pending_mfa(&hash);
        return err(
            StatusCode::UNAUTHORIZED,
            "mfa_required",
            "invalid or expired pending token",
        )
        .into_response();
    }
    if purpose != "verify" {
        // Enrollment is web-only tonight (see `auth_login`).
        return err(
            StatusCode::UNAUTHORIZED,
            "mfa_required",
            "enroll via the web console first",
        )
        .into_response();
    }
    // Throttle per pending token with the shared web map (5 failures / 900s),
    // same key shape as the web flow; a success clears the counter below.
    let throttle_key = format!("mfa:{}", f.pending_token);
    if crate::auth::login_throttle(&state.login_attempts, &throttle_key, 5, 900) {
        return err(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
            "too many failed login attempts; try again later",
        )
        .into_response();
    }
    let code = f.code.trim();
    let (secret, enabled, last_used) = state.db.get_totp_state(account_id);
    let mut ok = false;
    if enabled && code.chars().all(|c| c.is_ascii_digit()) && code.len() == 6 {
        let now_ts = chrono::Utc::now().timestamp() as u64;
        for delta in [-30i64, 0i64, 30i64] {
            let ts = (now_ts as i64 + delta).max(0) as u64;
            let counter = (ts / 30) as i64;
            let generated =
                crate::util::totp_generate(&secret.clone().unwrap_or_default(), ts, 6usize, 30u64);
            if crate::util::ct_eq(&generated, code)
                && last_used.map(|l| counter > l).unwrap_or(true)
            {
                ok = true;
                let _ = state.db.update_totp_last_used(account_id, counter);
                break;
            }
        }
    }
    if !ok
        && state.cfg.mfa_recovery_codes_enabled
        && state.db.verify_recovery_code(account_id, code)
    {
        ok = true;
    }
    if !ok {
        let detail = if state.cfg.mfa_recovery_codes_enabled {
            "invalid authenticator or recovery code"
        } else {
            "invalid authenticator code"
        };
        return err(StatusCode::UNAUTHORIZED, "invalid_code", detail).into_response();
    }
    crate::auth::login_ok(&state.login_attempts, &throttle_key);
    let _ = state.db.delete_pending_mfa(&hash);
    let (id, _u, _h, role, seller_id) = state.db.get_web_account_by_id(account_id).unwrap_or((
        account_id,
        String::new(),
        String::new(),
        "seller".to_string(),
        None,
    ));
    let token = state.db.create_web_session(id, &role, seller_id, 7);
    let expires_at = chrono::Utc::now() + chrono::Duration::days(7);
    (
        StatusCode::OK,
        Json(
            serde_json::to_value(LoginResp {
                token,
                expires_at: expires_at.to_rfc3339(),
                account_id: id,
                role,
                seller_id,
            })
            .unwrap_or_default(),
        ),
    )
        .into_response()
}

async fn auth_logout(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    if let Some(token) = crate::auth::get_cookie(&headers) {
        state.db.delete_web_session(&token);
    }
    // Bearer token support
    if let Some(auth) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        && let Some(tok) = auth.strip_prefix("Bearer ")
    {
        state.db.delete_web_session(tok);
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn auth_me(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "account_id": session.account_id,
            "role": session.role,
            "seller_id": session.seller_id,
        })),
    )
        .into_response()
}

#[derive(Deserialize)]
struct PasswordReq {
    /// Required for non-owner accounts: the current password, verified via
    /// the same bcrypt path login uses (`verify_web_password`). The owner's
    /// web session is the strongest identity a deployment has and may omit
    /// it (there is no other owner-password surface — see setup/main.rs).
    current_password: Option<String>,
    new_password: String,
}

/// Extract the session token the way `session_from_headers` resolves it
/// (cookie first, then Bearer) so the password change can keep the caller's
/// session while evicting every other session of the account.
fn caller_token(headers: &HeaderMap) -> Option<String> {
    if let Some(t) = crate::auth::get_cookie(headers) {
        return Some(t);
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t.to_string())
}

async fn auth_password(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<PasswordReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let Some((_id, username, _hash, role, _seller)) =
        state.db.get_web_account_by_id(session.account_id)
    else {
        // A session whose account row vanished (deleted account): be honest
        // instead of silently succeeding.
        return err(
            StatusCode::UNAUTHORIZED,
            "account_not_found",
            "session account no longer exists",
        )
        .into_response();
    };
    // Mirror the setup form's "min 6 chars" rule (web.rs setup_submit).
    if f.new_password.chars().count() < 6 {
        return err(
            StatusCode::BAD_REQUEST,
            "weak_password",
            "password must be at least 6 characters",
        )
        .into_response();
    }
    // Non-owner accounts must prove knowledge of the current password via
    // the same bcrypt verification the login uses.
    if !Role::from_role_str(&role).has_level(Role::Owner) {
        // spawn_blocking hasher is async: awaiting happens here, not in a
        // sync closure (mechanical restructure of the former Option::map).
        let verified = match f.current_password.as_deref() {
            Some(cur) => state.db.verify_web_password(&username, cur).await.is_some(),
            None => false,
        };
        if !verified {
            return err(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "current password missing or incorrect",
            )
            .into_response();
        }
    }
    if let Err(e) = state
        .db
        .update_web_password_by_id(session.account_id, &f.new_password)
        .await
    {
        return err(StatusCode::INTERNAL_SERVER_ERROR, "update_failed", &e).into_response();
    }
    // Password rotation invalidates every OTHER session of this account (a
    // stolen session must not survive it); the caller stays logged in.
    if let Some(tok) = caller_token(&headers) {
        state.db.delete_other_web_sessions(session.account_id, &tok);
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

// ---------- Sellers ----------
async fn sellers_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Admin) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "admin or owner required",
        )
        .into_response();
    }
    let ids = state.db.list_sellers();
    let items: Vec<serde_json::Value> = ids
        .into_iter()
        .map(|id| {
            serde_json::json!({
                "user_id": id, "username": null, "default_channel": state.db.seller_channel(id),
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

async fn sellers_me(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let sid = match session.seller_id {
        Some(s) => s,
        None => {
            return err(StatusCode::FORBIDDEN, "not_a_seller", "no seller profile").into_response();
        }
    };
    let (uid, username, channel) = state.db.get_seller(sid).unwrap_or((sid, None, None));
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "user_id": uid, "username": username, "default_channel": channel,
        })),
    )
        .into_response()
}

async fn seller_get(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !may_act_on_seller(&session, user_id) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "cannot view other sellers",
        )
        .into_response();
    }
    match state.db.get_seller(user_id) {
        Some((uid, username, channel)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "user_id": uid, "username": username, "default_channel": channel,
            })),
        )
            .into_response(),
        None => err(StatusCode::NOT_FOUND, "not_found", "seller not found").into_response(),
    }
}

#[derive(Deserialize)]
struct SellerUpsertReq {
    user_id: i64,
    username: Option<String>,
    default_channel: Option<String>,
}
async fn sellers_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<SellerUpsertReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Admin)
        && session.seller_id != Some(f.user_id)
    {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "cannot create seller for others",
        )
        .into_response();
    }
    state.db.upsert_seller(
        f.user_id,
        f.username.as_deref(),
        f.default_channel.as_deref(),
    );
    (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response()
}

#[derive(Deserialize)]
struct ChannelReq {
    channel: String,
}

/// Outcome of the channel proof-of-control check for channel binding.
enum ChannelProof {
    /// The requesting user is an administrator of the chat (creator counts).
    Proven,
    /// The chat was enumerated but the user is not among its administrators.
    NotAdmin,
    /// The bot could not enumerate the chat's administrators — in practice
    /// it is not a member of the channel (or the handle does not resolve).
    BotNotInChat,
}

/// Proof of control for channel binding: the Telegram user must be an
/// administrator (creator counts) of `channel`. The call doubles as a
/// membership probe — `getChatAdministrators` errors when the bot itself is
/// not in the chat. Timeout-safety: a single request, no retry loops; the
/// bot client's own request timeout bounds the call so a slow Telegram API
/// cannot hang the web request-timeout middleware.
async fn channel_admin_proof(
    bot: &teloxide::Bot,
    channel: &str,
    tg_user_id: Option<i64>,
) -> ChannelProof {
    let Some(tg_user_id) = tg_user_id else {
        // No Telegram identity bound to the session: nothing to prove.
        return ChannelProof::NotAdmin;
    };
    let members = match bot
        .get_chat_administrators(crate::util::to_chat_id(channel))
        .await
    {
        Ok(m) => m,
        Err(_) => return ChannelProof::BotNotInChat,
    };
    let is_admin = members.iter().any(|m| {
        m.user.id.0 as i64 == tg_user_id
            && matches!(
                m.kind,
                ChatMemberKind::Owner(_) | ChatMemberKind::Administrator(_)
            )
    });
    if is_admin {
        ChannelProof::Proven
    } else {
        ChannelProof::NotAdmin
    }
}

async fn seller_set_channel(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Json(f): Json<ChannelReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !may_act_on_seller(&session, user_id) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "cannot modify other seller",
        )
        .into_response();
    }
    let channel = f.channel.trim().to_string();
    // Proof of control (security BLOCK-3): non-admin callers may only bind a
    // channel they actually control. Admins/owner keep the unvalidated free
    // set — the bootstrap path (first-run channel setup) runs before the bot
    // is a member of any chat, and admin/owner console sessions carry no
    // bound Telegram identity to run the administrators check against.
    if !Role::from_role_str(&session.role).has_level(Role::Admin) {
        if let Ok(n) = channel.parse::<i64>() {
            if n >= 0 {
                // Telegram channel/group ids are negative (`-100…` for
                // supergroups/channels, small negatives for basic groups);
                // private chat ids are positive. A non-negative id can never
                // be a postable target channel.
                return err(
                    StatusCode::BAD_REQUEST,
                    "invalid_channel",
                    "channel must be a group/channel id (negative) — private chats cannot be target channels",
                )
                .into_response();
            }
        } else if !channel.starts_with('@') {
            // Anything else (bare name, URL) is not a resolvable target.
            return err(
                StatusCode::BAD_REQUEST,
                "invalid_channel",
                "channel must be a @handle or a group/channel id (negative)",
            )
            .into_response();
        } else {
            // @handle: the seller must be an administrator of that chat. The
            // proof runs through the session's effective bot, so the seller is
            // probed by the bot that will actually post to their channel.
            let proof_bot = crate::web::effective_bot(&state, &session);
            match channel_admin_proof(&proof_bot.bot, &channel, session.seller_id).await {
                ChannelProof::Proven => {}
                ChannelProof::NotAdmin => {
                    return err(
                        StatusCode::FORBIDDEN,
                        "forbidden",
                        "not an admin of that channel",
                    )
                    .into_response();
                }
                ChannelProof::BotNotInChat => {
                    return err(
                        StatusCode::BAD_REQUEST,
                        "invalid_channel",
                        "bot not in channel — add the bot first",
                    )
                    .into_response();
                }
            }
        }
    }
    state.db.set_seller_channel(user_id, &channel);
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

/// Per-seller AI tuning (sellers.ai_provider/ai_model/daily_token_budget/
/// rpm_limit). Only `daily_token_budget` is enforced by the AI service today
/// (see ai::service::generate); provider/model/rpm are stored for the
/// per-seller overrides the global AI config already supports.
#[derive(Deserialize)]
struct SellerAiSettingsReq {
    provider: Option<String>,
    model: Option<String>,
    daily_token_budget: Option<i64>,
    rpm_limit: Option<i64>,
}

async fn seller_get_ai_settings(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !may_act_on_seller(&session, user_id) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "cannot view other sellers",
        )
        .into_response();
    }
    match state.db.get_seller_ai_settings(user_id) {
        Some((provider, model, budget, rpm)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "user_id": user_id,
                "provider": provider,
                "model": model,
                "daily_token_budget": budget,
                "rpm_limit": rpm,
            })),
        )
            .into_response(),
        None => err(StatusCode::NOT_FOUND, "not_found", "seller not found").into_response(),
    }
}

async fn seller_set_ai_settings(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Json(f): Json<SellerAiSettingsReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !may_act_on_seller(&session, user_id) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "cannot modify other seller",
        )
        .into_response();
    }
    // Validation mirrors the global AI form (web::ai_test): the provider must
    // name a known backend, the model is a short free-form string, budgets and
    // limits are non-negative. Omitted fields keep their stored value (the Db
    // setter COALESCEs), so this endpoint is a partial update.
    if let Some(p) = f.provider.as_deref().map(str::trim)
        && !matches!(
            p,
            "openai" | "anthropic" | "ollama" | "openrouter" | "custom"
        )
    {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "provider must be openai, anthropic, ollama, openrouter, or custom",
        )
        .into_response();
    }
    if let Some(m) = f.model.as_deref().map(str::trim)
        && (m.is_empty() || m.len() > 128)
    {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "model must be 1-128 characters",
        )
        .into_response();
    }
    if f.daily_token_budget.is_some_and(|b| b < 0) {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "daily_token_budget must be >= 0",
        )
        .into_response();
    }
    if f.rpm_limit.is_some_and(|r| r < 0) {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "rpm_limit must be >= 0",
        )
        .into_response();
    }
    let provider = f
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let model = f.model.as_deref().map(str::trim).filter(|s| !s.is_empty());
    state
        .db
        .set_seller_ai_settings(user_id, provider, model, f.daily_token_budget, f.rpm_limit);
    let meta = serde_json::json!({
        "provider": provider, "model": model,
        "daily_token_budget": f.daily_token_budget, "rpm_limit": f.rpm_limit,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "ai_settings_update",
        "seller",
        user_id.to_string(),
        Some(&meta),
    );
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

// ---------- Plans ----------
async fn plans_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let plans = state.db.list_plans(scope);
    let items: Vec<serde_json::Value> = plans.into_iter().map(|p| serde_json::json!({
        "name": p.0, "price": p.1, "asset": p.2, "duration_days": p.3, "channel_id": p.4, "active": p.5, "seller_id": p.6,
    })).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct PlanCreateReq {
    name: String,
    price: String,
    asset: String,
    duration_days: i64,
    channel_id: Option<String>,
}
async fn plans_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<PlanCreateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let seller_id = match session.seller_id {
        Some(s) => s,
        None => {
            return err(
                StatusCode::FORBIDDEN,
                "not_a_seller",
                "seller profile required",
            )
            .into_response();
        }
    };
    // Same bound as the bot's /planadd; also guards Db's unchecked
    // expires_at arithmetic (Duration::days) against overflow panics.
    let days = f.duration_days.clamp(1, crate::constants::MAX_PLAN_DAYS);
    state.db.add_plan(
        seller_id,
        &f.name,
        &f.price,
        &f.asset,
        days,
        f.channel_id.as_deref(),
    );
    (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn plans_get(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    // find by id via list (no get_plan_by_id scoping helper exists for seller; use get_plan_by_id then check)
    match state.db.get_plan_by_id(id) {
        Some((sid, price, asset, name, days, channel)) => {
            if !may_act_on_seller(&session, sid) {
                return err(StatusCode::FORBIDDEN, "forbidden", "not your plan").into_response();
            }
            (StatusCode::OK, Json(serde_json::json!({
                "id": id, "seller_id": sid, "name": name, "price": price, "asset": asset, "duration_days": days, "channel_id": channel,
            }))).into_response()
        }
        None => err(StatusCode::NOT_FOUND, "not_found", "plan not found").into_response(),
    }
}

#[derive(Deserialize)]
struct PlanUpdateReq {
    name: Option<String>,
    price: Option<String>,
    asset: Option<String>,
    duration_days: Option<i64>,
    channel_id: Option<String>,
    active: Option<bool>,
}
async fn plans_update(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(f): Json<PlanUpdateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let (sid, _price, _asset, name, days, channel) = match state.db.get_plan_by_id(id) {
        Some(x) => x,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "plan not found").into_response();
        }
    };
    if !may_act_on_seller(&session, sid) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your plan").into_response();
    }
    // Re-add plan with merged fields (add_plan is upsert by seller+name)
    let new_name = f.name.unwrap_or(name);
    let new_price = f.price.unwrap_or(_price);
    let new_asset = f.asset.unwrap_or(_asset);
    let new_days = f
        .duration_days
        .unwrap_or(days)
        .clamp(1, crate::constants::MAX_PLAN_DAYS);
    let new_channel = f.channel_id.or(channel);
    if let Some(active) = f.active {
        if !active {
            state.db.remove_plan(sid, &new_name);
        } else {
            state.db.add_plan(
                sid,
                &new_name,
                &new_price,
                &new_asset,
                new_days,
                new_channel.as_deref(),
            );
        }
    } else {
        state.db.add_plan(
            sid,
            &new_name,
            &new_price,
            &new_asset,
            new_days,
            new_channel.as_deref(),
        );
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn plans_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let (sid, _p, _a, name, _d, _c) = match state.db.get_plan_by_id(id) {
        Some(x) => x,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "plan not found").into_response();
        }
    };
    if !may_act_on_seller(&session, sid) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your plan").into_response();
    }
    state.db.remove_plan(sid, &name);
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

#[derive(Deserialize)]
struct ActiveReq {
    active: bool,
}
async fn plans_active(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(f): Json<ActiveReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let (sid, price, asset, name, days, channel) = match state.db.get_plan_by_id(id) {
        Some(x) => x,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "plan not found").into_response();
        }
    };
    if !may_act_on_seller(&session, sid) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your plan").into_response();
    }
    if f.active {
        state
            .db
            .add_plan(sid, &name, &price, &asset, days, channel.as_deref());
    } else {
        state.db.remove_plan(sid, &name);
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

// ---------- Subscriptions ----------
async fn subs_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let subs = state.db.list_subscriptions(scope, 100);
    let items: Vec<serde_json::Value> = subs.into_iter().map(|s| serde_json::json!({
        "id": s.0, "seller_id": s.1, "user_id": s.2, "channel_id": s.3, "status": s.4, "expires_at": s.5,
    })).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct SubGrantReq {
    user_id: i64,
    plan_id: Option<i64>,
    channel_id: Option<String>,
    days: i64,
    invoice_id: Option<String>,
    code_id: Option<i64>,
}
async fn subs_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<SubGrantReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let seller_id = match session.seller_id {
        Some(s) => s,
        None => {
            return err(
                StatusCode::FORBIDDEN,
                "not_a_seller",
                "seller profile required",
            )
            .into_response();
        }
    };
    let ch = f.channel_id.unwrap_or_default();
    // Clamp to the shared plan-day bound so Db's unchecked expires_at
    // arithmetic can never overflow-panic on a hostile `days`.
    let days = f.days.clamp(1, crate::constants::MAX_PLAN_DAYS);
    let sub_id = state.db.create_subscription(
        seller_id,
        f.user_id,
        f.plan_id,
        &ch,
        days,
        f.invoice_id.as_deref(),
        f.code_id,
    );
    (StatusCode::CREATED, Json(serde_json::json!({"id": sub_id}))).into_response()
}

async fn subs_get(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    // find in list (no get_sub_by_id helper); scan
    let scope = seller_scope(&session);
    let subs = state.db.list_subscriptions(scope, 1000);
    match subs.into_iter().find(|s| s.0 == id) {
        Some(s) => (StatusCode::OK, Json(serde_json::json!({
            "id": s.0, "seller_id": s.1, "user_id": s.2, "channel_id": s.3, "status": s.4, "expires_at": s.5,
        }))).into_response(),
        None => err(StatusCode::NOT_FOUND, "not_found", "subscription not found").into_response(),
    }
}

#[derive(Deserialize)]
struct SubPatchReq {
    status: Option<String>,
    expires_at: Option<String>,
}
async fn subs_patch(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(f): Json<SubPatchReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    // Subscription status is a privileged mutation: only the owner may change
    // it (mirrors the web `subs_revoke` / `subs_expirecheck` owner gates). A
    // seller session never reaches the mutation, even for its own row.
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    // Resolve the target inside the caller's tenant scope before touching it.
    // Owner scope is global, but an id that does not exist must still fail
    // closed (404) rather than mutate an arbitrary row.
    let scope = seller_scope(&session);
    let in_scope = state
        .db
        .list_subscriptions(scope, 1000)
        .iter()
        .any(|s| s.0 == id);
    if !in_scope {
        return err(StatusCode::NOT_FOUND, "not_found", "subscription not found").into_response();
    }
    if let Some(status) = &f.status
        && status == "expired"
    {
        state.db.mark_subscription_expired(id);
    }
    let _ = f.expires_at;
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn subs_expire_check(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let role = Role::from_role_str(&session.role);
    let expired = if role.has_level(Role::Owner) {
        // Owner/admin keep the global sweep, matching the web
        // `/subs/expirecheck` owner gate and the scheduled sweeper.
        let rows = state.db.expired_subscriptions();
        let n = rows.len();
        for (id, _sid, _uid, _ch) in rows {
            state.db.mark_subscription_expired(id);
        }
        n
    } else if role == Role::Seller {
        // Sellers may expire only their own rows. The unconditional
        // seller_id predicate lives in Db, and a seller without a server-set
        // seller_id fails closed rather than widening to a global sweep.
        match seller_scope(&session) {
            Some(sid) => state.db.expire_subscriptions_for_seller(sid),
            None => {
                return err(StatusCode::FORBIDDEN, "forbidden", "seller scope required")
                    .into_response();
            }
        }
    } else {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "seller or owner required",
        )
        .into_response();
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({"expired": expired})),
    )
        .into_response()
}

async fn user_subs(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let subs = state.db.list_subscriptions(scope, 1000);
    let items: Vec<_> = subs.into_iter().filter(|s| s.2 == user_id).map(|s| serde_json::json!({
        "id": s.0, "seller_id": s.1, "user_id": s.2, "channel_id": s.3, "status": s.4, "expires_at": s.5,
    })).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

// ---------- Invoices ----------
async fn invoices_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let invs = state.db.list_invoices(scope, 100);
    let items: Vec<serde_json::Value> = invs
        .into_iter()
        .map(|i| {
            serde_json::json!({
                "invoice_id": i.0, "amount": i.1, "asset": i.2, "status": i.3, "pay_url": i.4,
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct InvoiceCreateReq {
    amount: String,
    asset: String,
    description: Option<String>,
    plan_id: Option<String>,
    user_id: Option<i64>,
}
async fn invoices_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<InvoiceCreateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let Some(seller_id) = session.seller_namespace() else {
        return err(
            StatusCode::FORBIDDEN,
            "not_a_seller",
            "seller profile required",
        )
        .into_response();
    };
    let inv_id = format!("inv_{}", crate::util::gen_token(16));
    state.db.save_invoice(
        &inv_id,
        seller_id,
        &f.amount,
        &f.asset,
        f.description.as_deref(),
        "active",
        None,
        f.plan_id.as_deref(),
        f.user_id,
        None,
    );
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"invoice_id": inv_id})),
    )
        .into_response()
}

async fn invoices_get(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(invoice_id): Path<String>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    match state.db.get_invoice(&invoice_id) {
        Some((amount, asset, status, desc, plan_id, user_id, seller_id)) => {
            if !may_act_on_seller(&session, seller_id) {
                return err(StatusCode::FORBIDDEN, "forbidden", "not your invoice").into_response();
            }
            (StatusCode::OK, Json(serde_json::json!({
                "invoice_id": invoice_id, "amount": amount, "asset": asset, "status": status,
                "description": desc, "plan_id": plan_id, "user_id": user_id, "seller_id": seller_id,
            }))).into_response()
        }
        None => err(StatusCode::NOT_FOUND, "not_found", "invoice not found").into_response(),
    }
}

#[derive(Deserialize)]
struct InvoiceStatusReq {
    status: String,
    /// Required only for the operator-forced `failed` transition (§3.6).
    reason: Option<String>,
}
async fn invoices_status(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(invoice_id): Path<String>,
    Json(f): Json<InvoiceStatusReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    // ownership check
    if let Some((_a, _as, _st, _d, _p, _u, sid)) = state.db.get_invoice(&invoice_id) {
        if !may_act_on_seller(&session, sid) {
            return err(StatusCode::FORBIDDEN, "forbidden", "not your invoice").into_response();
        }
    } else {
        return err(StatusCode::NOT_FOUND, "not_found", "invoice not found").into_response();
    }
    // 'paid' and 'paid_ungranted' are verified/system states, never manual:
    // they may only be reached through the webhook, claim or recovery flow
    // (§3.2/§3.6). Refuse both explicitly.
    if f.status == "paid" || f.status == "paid_ungranted" {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid_status",
            "paid/paid_ungranted transitions require verified webhook/claim",
        )
        .into_response();
    }
    if f.status == "failed" {
        // §3.6 / D3.6: an operator-forced failure requires the exact Owner
        // role (not Admin/Seller), a non-empty reason, and an audit event.
        // 'failed' is legal only from a pre-payment state — never from a paid
        // or paid_ungranted invoice (§3.1).
        if Role::from_role_str(&session.role) != Role::Owner {
            return err(StatusCode::FORBIDDEN, "owner_required", "owner required").into_response();
        }
        let reason = f.reason.as_deref().unwrap_or("").trim();
        if reason.is_empty() {
            return err(
                StatusCode::BAD_REQUEST,
                "reason_required",
                "a non-empty reason is required to fail an invoice",
            )
            .into_response();
        }
        let previous_status = state
            .db
            .get_invoice(&invoice_id)
            .map(|inv| inv.2)
            .unwrap_or_default();
        if previous_status == "paid" || previous_status == "paid_ungranted" {
            return err(
                StatusCode::CONFLICT,
                "invalid_status",
                "cannot fail a paid or paid_ungranted invoice",
            )
            .into_response();
        }
        state.db.update_invoice_status(&invoice_id, "failed");
        let meta = serde_json::json!({
            "reason": reason, "previous_status": previous_status,
        })
        .to_string();
        state.db.log_action(
            session.account_id,
            &session.role,
            "invoice_manual_failed",
            "invoice",
            &invoice_id,
            Some(&meta),
        );
        return (
            StatusCode::OK,
            Json(serde_json::json!({"ok": true, "status": "failed"})),
        )
            .into_response();
    }
    // All other transitions keep their prior behaviour (seller may e.g. mark
    // an unpaid invoice 'expired'/'cancelled' without a reason).
    if !matches!(f.status.as_str(), "active" | "expired" | "cancelled") {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid_status",
            "status transition not allowed",
        )
        .into_response();
    }
    state.db.update_invoice_status(&invoice_id, &f.status);
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

// ---------- Codes ----------
async fn codes_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let codes = state.db.list_codes(scope, 100);
    let items: Vec<serde_json::Value> = codes.into_iter().map(|c| serde_json::json!({
        "code": c.0, "plan_id": c.1, "value_days": c.2, "uses": c.3, "max_uses": c.4, "seller_id": c.5, "revoked": c.6,
    })).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct CodeGenReq {
    plan_id: Option<i64>,
    value_days: Option<i64>,
    max_uses: Option<i64>,
    expires_at: Option<String>,
    count: Option<i64>,
    length: Option<i64>,
}
async fn codes_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<CodeGenReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let seller_id = match session.seller_id {
        Some(s) => s,
        None => {
            return err(
                StatusCode::FORBIDDEN,
                "not_a_seller",
                "seller profile required",
            )
            .into_response();
        }
    };
    let count = f.count.unwrap_or(1).clamp(1, 100);
    let max_uses = f.max_uses.unwrap_or(1);
    // Clamp to the shared plan-day bound: these days later feed subscription
    // expiry arithmetic when the code is redeemed.
    let value_days = f
        .value_days
        .map(|d| d.clamp(1, crate::constants::MAX_PLAN_DAYS));
    let length = f.length.unwrap_or(12).clamp(1, 32) as usize;
    let mut codes = Vec::new();
    for _ in 0..count {
        let code = state.db.create_code(
            seller_id,
            f.plan_id,
            value_days,
            max_uses,
            f.expires_at.as_deref(),
            length,
        );
        codes.push(code);
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"codes": codes})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct CodeReclaimReq {
    code: String,
    length: Option<i64>,
}

async fn codes_reclaim(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<CodeReclaimReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let length = f.length.unwrap_or(12).clamp(1, 32) as usize;
    match state.db.reclaim_code(&f.code, scope, length) {
        Some(new_code) => {
            (StatusCode::OK, Json(serde_json::json!({"code": new_code}))).into_response()
        }
        None => err(StatusCode::NOT_FOUND, "not_found", "code not found").into_response(),
    }
}

async fn codes_get(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    match state.db.get_code(&code) {
        Some((id, seller_id, plan_id, value_days, max_uses, uses, expires_at)) => {
            if !may_act_on_seller(&session, seller_id) {
                return err(StatusCode::FORBIDDEN, "forbidden", "not your code").into_response();
            }
            (StatusCode::OK, Json(serde_json::json!({
                "id": id, "code": code, "seller_id": seller_id, "plan_id": plan_id,
                "value_days": value_days, "max_uses": max_uses, "uses": uses, "expires_at": expires_at,
            }))).into_response()
        }
        None => err(StatusCode::NOT_FOUND, "not_found", "code not found").into_response(),
    }
}

async fn codes_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let (_id, seller_id, _p, _v, _m, _u, _e) = match state.db.get_code(&code) {
        Some(x) => x,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "code not found").into_response();
        }
    };
    if !may_act_on_seller(&session, seller_id) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your code").into_response();
    }
    // Real delete via the scoped helper (server db.rs addition): sellers delete
    // only their own codes, admin/owner any. `seller_scope` yields None for
    // admin/owner and Some(id) for sellers.
    if !state
        .db
        .delete_code_seller_scoped(&code, seller_scope(&session))
    {
        return err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "delete_failed",
            "code delete failed",
        )
        .into_response();
    }
    state.db.log_action(
        session.account_id,
        &session.role,
        "code_delete",
        "code",
        &code,
        None,
    );
    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "deleted": code})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct RedeemReq {
    user_id: Option<i64>,
}
async fn codes_redeem(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(code): Path<String>,
    Json(f): Json<RedeemReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    // Tier floor: a user-tier caller must not name a beneficiary in the body.
    let user_id = if Role::from_role_str(&session.role) == Role::User {
        session.account_id
    } else {
        f.user_id.unwrap_or(session.account_id)
    };
    // Pre-flight seller-channel check (mirrors /redeem): redeem_code burns a
    // use unconditionally, so a code whose seller has nowhere to grant INTO
    // must be refused untouched, not consumed (P1#7).
    if let Some(c) = state.db.get_code(&code)
        && !state.db.seller_has_channel(c.1)
    {
        return err(
            StatusCode::CONFLICT,
            "seller_channel_missing",
            "seller has no channel configured; the code was not consumed",
        )
        .into_response();
    }
    match state.db.redeem_code(&code, user_id) {
        Ok((seller_id, plan_id, value_days)) => {
            // create subscription if plan/value days
            if let Some(days) = value_days {
                // Clamp defensively (legacy codes may predate bounds) so the
                // subscription expiry math cannot overflow.
                let days = days.clamp(1, crate::constants::MAX_PLAN_DAYS);
                let ch = state.db.seller_channel(seller_id).unwrap_or_default();
                state
                    .db
                    .create_subscription(seller_id, user_id, plan_id, &ch, days, None, None);
            }
            (StatusCode::OK, Json(serde_json::json!({"ok": true, "seller_id": seller_id, "plan_id": plan_id, "value_days": value_days}))).into_response()
        }
        Err(e) => err(StatusCode::BAD_REQUEST, "redeem_failed", &e).into_response(),
    }
}

/// Redemption history for one code. Same lookup + scoping as `codes_get`:
/// 404 for unknown codes, sellers restricted to their own codes (the Db
/// method additionally re-applies the scope in SQL, so the two layers agree).
async fn codes_redemptions(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    match state.db.get_code(&code) {
        Some((_id, seller_id, _p, _v, _m, _u, _e)) => {
            if !may_act_on_seller(&session, seller_id) {
                return err(StatusCode::FORBIDDEN, "forbidden", "not your code").into_response();
            }
        }
        None => return err(StatusCode::NOT_FOUND, "not_found", "code not found").into_response(),
    }
    let rows = state
        .db
        .list_code_redemptions(&code, seller_scope(&session), 200);
    let items: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.0, "code": code.as_str(), "user_id": r.1, "redeemed_at": r.2,
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

// ---------- Posts ----------
async fn posts_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let posts = state.db.list_posts(scope, 100);
    let items: Vec<serde_json::Value> = posts.into_iter().map(|p| serde_json::json!({
        "id": p.0, "seller_id": p.1, "chat_id": p.2, "message_id": p.3, "kind": p.4, "content": p.5, "created_at": p.6,
    })).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct PostCreateReq {
    channel: Option<String>,
    text: String,
}
async fn posts_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<PostCreateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    // Default channel: managed channels table (is_default global row) with
    // legacy-config fallback — see Db::default_channel.
    let ch = f
        .channel
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| state.db.default_channel().unwrap_or_default());
    if ch.is_empty() || f.text.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "channel/text required",
        )
        .into_response();
    }
    // The target is client-supplied and the send is logged against it. Apply
    // the console's composer-target policy (owned rows + global rows) before
    // any moderation check, Bot API call or DB write: a seller must not be able
    // to post into another seller's channel through REST either.
    if !crate::handlers::channel_access::may_post_to_channel(
        &state.db,
        &session.role,
        session.seller_id,
        session.account_id,
        session.selected_bot_id,
        &ch,
    ) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your channel").into_response();
    }
    // Sends go out as the session's effective bot (seller → their bot, owner →
    // selected bot, admin → their bot with primary fallback).
    let bot = crate::web::effective_bot(&state, &session);
    // security outbound gate BEFORE this raw-Bot send. Seller scope comes from the
    // principal (session), never a global 0, so seller rules apply too.
    if let Err(hold) = crate::handlers::moderation::gate_outbound(
        &state.db,
        session
            .seller_namespace()
            .unwrap_or(crate::roles::TenantScope::DENIED_ID),
        Some(bot.id),
        &f.text,
    ) {
        return err(StatusCode::FORBIDDEN, "moderation_hold", &hold.to_string()).into_response();
    }
    let msg_id = match bot
        .bot
        .send_message(crate::util::to_chat_id(&ch), f.text.clone())
        .await
    {
        Ok(m) => m.id.0,
        Err(_) => 0,
    };
    let seller_id = session
        .seller_namespace()
        .unwrap_or(crate::roles::TenantScope::DENIED_ID);
    let _ = state
        .db
        .log_post(seller_id, &ch, msg_id, "text", Some(&f.text), "bot");
    state.db.log_action(
        session.account_id,
        &session.role,
        "post_create",
        "post",
        "new",
        Some(&f.text),
    );
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"ok": true, "message_id": msg_id})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct PostScheduleReq {
    channel: Option<String>,
    text: String,
    scheduled_at: String,
}
async fn posts_schedule(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<PostScheduleReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let ch = f
        .channel
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| state.db.get_config("channel").unwrap_or_default());
    if ch.is_empty() || f.text.trim().is_empty() || f.scheduled_at.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "channel/text/scheduled_at required",
        )
        .into_response();
    }
    // F1: the scheduler sends through the bot bound to the TARGET channel
    // (BotManager::for_channel in main.rs), not the caller's effective bot.
    // Without this gate a seller could queue text into a foreign channel under
    // the victim bot's identity while the moderation gate checked the attacker
    // scope. Authorize before the row write; owner/admin keep the global tier.
    if !api_may_mutate_channel(&state, &session, &ch) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your channel").into_response();
    }
    let seller_id = session
        .seller_namespace()
        .unwrap_or(crate::roles::TenantScope::DENIED_ID);
    let id = state
        .db
        .create_scheduled_post(seller_id, &ch, &f.text, "text", &f.scheduled_at);
    state.db.log_action(
        session.account_id,
        &session.role,
        "post_schedule",
        "scheduled_post",
        id.to_string(),
        Some(&f.text),
    );
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"id": id, "scheduled_at": f.scheduled_at})),
    )
        .into_response()
}

// ---------- Posts (single) ----------
async fn posts_get(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let posts = state.db.list_posts(scope, 100000);
    match posts.into_iter().find(|p| p.0 == id) {
        Some(p) => {
            if !may_act_on_seller(&session, p.1) {
                return err(StatusCode::FORBIDDEN, "forbidden", "not your post").into_response();
            }
            (StatusCode::OK, Json(serde_json::json!({
                "id": p.0, "seller_id": p.1, "chat_id": p.2, "message_id": p.3, "kind": p.4, "content": p.5, "created_at": p.6,
            }))).into_response()
        }
        None => err(StatusCode::NOT_FOUND, "not_found", "post not found").into_response(),
    }
}

#[derive(Deserialize)]
struct PostUpdateReq {
    content: Option<String>,
    text: Option<String>,
}
async fn posts_update(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(f): Json<PostUpdateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let posts = state.db.list_posts(scope, 100000);
    let p = match posts.into_iter().find(|p| p.0 == id) {
        Some(p) => p,
        None => return err(StatusCode::NOT_FOUND, "not_found", "post not found").into_response(),
    };
    if !may_act_on_seller(&session, p.1) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your post").into_response();
    }
    let new_content = f.content.or(f.text);
    if let Some(c) = &new_content {
        // security outbound gate BEFORE the raw-Bot edit. A post edit is a
        // content-bearing send path like any other; without this call it
        // bypasses the moderation floor that the create/broadcast paths
        // already honour. Seller scope comes from the session principal.
        let bot = crate::web::effective_bot(&state, &session);
        if let Err(hold) = crate::handlers::moderation::gate_outbound(
            &state.db,
            session
                .seller_namespace()
                .unwrap_or(crate::roles::TenantScope::DENIED_ID),
            Some(bot.id),
            c,
        ) {
            return err(StatusCode::FORBIDDEN, "moderation_hold", &hold.to_string())
                .into_response();
        }
        if p.3 != 0 {
            let _ = bot
                .bot
                .edit_message_text(crate::util::to_chat_id(&p.2), MessageId(p.3), c.clone())
                .await;
        }
        let _ = state.db.update_post_content(id, c);
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn posts_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let posts = state.db.list_posts(scope, 100000);
    let p = match posts.into_iter().find(|p| p.0 == id) {
        Some(p) => p,
        None => return err(StatusCode::NOT_FOUND, "not_found", "post not found").into_response(),
    };
    if !may_act_on_seller(&session, p.1) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your post").into_response();
    }
    if p.3 != 0 {
        let bot = crate::web::effective_bot(&state, &session);
        let _ = bot
            .bot
            .delete_message(crate::util::to_chat_id(&p.2), MessageId(p.3))
            .await;
    }
    let _ = state.db.delete_post(id);
    state.db.log_action(
        session.account_id,
        &session.role,
        "post_delete",
        "post",
        id.to_string(),
        None,
    );
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn posts_pin(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let posts = state.db.list_posts(scope, 100000);
    let p = match posts.into_iter().find(|p| p.0 == id) {
        Some(p) => p,
        None => return err(StatusCode::NOT_FOUND, "not_found", "post not found").into_response(),
    };
    if !may_act_on_seller(&session, p.1) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your post").into_response();
    }
    if p.3 != 0 {
        let bot = crate::web::effective_bot(&state, &session);
        let _ = bot
            .bot
            .pin_chat_message(crate::util::to_chat_id(&p.2), MessageId(p.3))
            .await;
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn posts_unpin(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let posts = state.db.list_posts(scope, 100000);
    let p = match posts.into_iter().find(|p| p.0 == id) {
        Some(p) => p,
        None => return err(StatusCode::NOT_FOUND, "not_found", "post not found").into_response(),
    };
    if !may_act_on_seller(&session, p.1) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your post").into_response();
    }
    if p.3 != 0 {
        let bot = crate::web::effective_bot(&state, &session);
        let _ = bot
            .bot
            .unpin_chat_message(crate::util::to_chat_id(&p.2))
            .message_id(MessageId(p.3))
            .await;
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

// ---------- Broadcasts ----------
#[derive(Deserialize)]
struct BroadcastReq {
    text: String,
}
async fn broadcasts(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<BroadcastReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Seller) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "seller or admin required",
        )
        .into_response();
    }
    let recipients = if Role::from_role_str(&session.role) == Role::Seller {
        state.db.list_users_for_seller(
            session
                .seller_namespace()
                .unwrap_or(crate::roles::TenantScope::DENIED_ID),
        )
    } else {
        state.db.list_users()
    };
    let mut sent = 0i64;
    // Broadcasts go out as the session's effective bot.
    let bot = crate::web::effective_bot(&state, &session);
    // security outbound gate BEFORE the raw-Bot broadcast fan-out. One decision
    // covers every recipient; seller scope comes from the session principal.
    if let Err(hold) = crate::handlers::moderation::gate_outbound(
        &state.db,
        session
            .seller_namespace()
            .unwrap_or(crate::roles::TenantScope::DENIED_ID),
        Some(bot.id),
        &f.text,
    ) {
        return err(StatusCode::FORBIDDEN, "moderation_hold", &hold.to_string()).into_response();
    }
    for uid in recipients {
        if bot
            .bot
            .send_message(teloxide::types::ChatId(uid), f.text.clone())
            .await
            .is_ok()
        {
            sent += 1;
        }
    }
    state.db.log_action(
        session.account_id,
        &session.role,
        "broadcast",
        "broadcast",
        "all",
        Some(&f.text),
    );
    (StatusCode::OK, Json(serde_json::json!({"sent": sent}))).into_response()
}

// ---------- Channels single ----------
async fn channels_get(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if session.seller_namespace().is_none() {
        return err(
            StatusCode::FORBIDDEN,
            "not_a_seller",
            "seller profile required",
        )
        .into_response();
    }
    (StatusCode::OK, Json(serde_json::json!({"channel": id}))).into_response()
}

#[derive(Deserialize)]
struct ChannelPatchReq {
    title: Option<String>,
    description: Option<String>,
}
async fn channels_patch(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(f): Json<ChannelPatchReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    // H1 tenant gate: a session may only mutate channels inside its own
    // scope. Without this, patching the path id lets one seller deface
    // another seller's channel through this raw-Bot path.
    if !api_may_mutate_channel(&state, &session, &id) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your channel").into_response();
    }
    let chat = crate::util::to_chat_id(&id);
    // Channel metadata edits run as the session's effective bot (the bot that
    // owns the seller's channel).
    let bot = crate::web::effective_bot(&state, &session);
    if let Some(title) = &f.title {
        let _ = bot.bot.set_chat_title(chat.clone(), title.clone()).await;
    }
    if let Some(desc) = &f.description {
        let _ = bot
            .bot
            .set_chat_description(chat.clone())
            .description(desc.clone())
            .await;
    }
    state.db.log_action(
        session.account_id,
        &session.role,
        "channel_update",
        "channel",
        &id,
        None,
    );
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

// ---------- Users single ----------
async fn users_get(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let (uid, username) = state.db.get_user(user_id).unwrap_or((user_id, None));
    let mut resp = serde_json::json!({"user_id": uid, "username": username});
    let subs = state.db.list_subscriptions(seller_scope(&session), 1000);
    let cnt = subs.iter().filter(|s| s.2 == user_id).count();
    resp["subscription_count"] = serde_json::json!(cnt);
    (StatusCode::OK, Json(resp)).into_response()
}

#[derive(Deserialize)]
struct UserUpsertReq {
    user_id: i64,
    username: Option<String>,
}
async fn users_upsert(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<UserUpsertReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if session.seller_namespace().is_none() {
        return err(
            StatusCode::FORBIDDEN,
            "not_a_seller",
            "seller profile required",
        )
        .into_response();
    }
    state.db.upsert_user(f.user_id, f.username.as_deref());
    (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response()
}

// ---------- AI templates single ----------
async fn ai_templates_get(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let Some(seller_id) = session.seller_namespace() else {
        return err(
            StatusCode::FORBIDDEN,
            "not_a_seller",
            "seller profile required",
        )
        .into_response();
    };
    match state.db.get_ai_template(seller_id, &name) {
        Some(t) => (StatusCode::OK, Json(serde_json::json!({
            "name": t.0, "description": t.1, "system_prompt": t.2, "user_prompt": t.3, "variables": t.4,
        }))).into_response(),
        None => err(StatusCode::NOT_FOUND, "not_found", "template not found").into_response(),
    }
}

#[derive(Deserialize)]
struct AiTemplatePatchReq {
    description: Option<String>,
    system_prompt: Option<String>,
    user_prompt: Option<String>,
    variables: Option<String>,
}
async fn ai_templates_update(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(f): Json<AiTemplatePatchReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let Some(seller_id) = session.seller_namespace() else {
        return err(
            StatusCode::FORBIDDEN,
            "not_a_seller",
            "seller profile required",
        )
        .into_response();
    };
    let existing = state.db.get_ai_template(seller_id, &name);
    let description = f
        .description
        .or_else(|| existing.as_ref().and_then(|e| e.1.clone()));
    let system_prompt = f
        .system_prompt
        .or_else(|| existing.as_ref().and_then(|e| e.2.clone()));
    let user_prompt = f
        .user_prompt
        .or_else(|| existing.as_ref().map(|e| e.3.clone()))
        .unwrap_or_default();
    let variables = f
        .variables
        .or_else(|| existing.as_ref().and_then(|e| e.4.clone()));
    state.db.upsert_ai_template(
        seller_id,
        &name,
        description.as_deref(),
        system_prompt.as_deref(),
        &user_prompt,
        variables.as_deref(),
    );
    state.db.log_action(
        session.account_id,
        &session.role,
        "ai_template_update",
        "ai_template",
        &name,
        None,
    );
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn ai_templates_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let Some(seller_id) = session.seller_namespace() else {
        return err(
            StatusCode::FORBIDDEN,
            "not_a_seller",
            "seller profile required",
        )
        .into_response();
    };
    state.db.delete_ai_template(seller_id, &name);
    state.db.log_action(
        session.account_id,
        &session.role,
        "ai_template_delete",
        "ai_template",
        &name,
        None,
    );
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

// ---------- Users ----------
async fn users_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if session.seller_namespace().is_none() {
        return err(
            StatusCode::FORBIDDEN,
            "not_a_seller",
            "seller profile required",
        )
        .into_response();
    }
    let ids = if Role::from_role_str(&session.role) == Role::Seller {
        state
            .db
            .list_users_for_seller(session.seller_id.unwrap_or(0))
    } else {
        state.db.list_users()
    };
    let items: Vec<serde_json::Value> = ids
        .into_iter()
        .map(|u| serde_json::json!({"user_id": u}))
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

// ---------- Stats ----------
/// Money payload shared by the stat/finance endpoints. Per-asset lines are
/// always present. A numeric `revenue_total` is emitted only when every paid
/// invoice uses one denomination; mixed assets get a labeled marker instead
/// of a meaningless summed float.
fn revenue_json(db: &crate::db::Db, scope: Option<i64>) -> serde_json::Value {
    let by_asset: Vec<serde_json::Value> = db
        .revenue_by_asset_scoped(scope)
        .into_iter()
        .map(|(asset, amount)| serde_json::json!({"asset": asset, "amount": amount}))
        .collect();
    let total = match db.revenue_total(scope) {
        Ok(v) => serde_json::json!(v),
        Err(_) => serde_json::json!("mixed assets"),
    };
    serde_json::json!({"revenue_total": total, "revenue_by_asset": by_asset})
}

async fn dashboard_summary(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let plans = state.db.list_plans(scope);
    let subs = state.db.list_subscriptions(scope, 1000);
    let active_subs = subs.iter().filter(|s| s.4 == "active").count();
    let invoices = state.db.list_invoices(scope, 1000);
    let pending_inv = invoices.iter().filter(|i| i.3 == "active").count();
    let codes = state.db.list_codes(scope, 1000);
    let codes_redeemed: i64 = codes.iter().map(|c| c.3).sum();
    let revenue = revenue_json(&state.db, scope);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "plans_active": plans.iter().filter(|p| p.5).count(),
            "subscriptions_active": active_subs,
            "subscriptions_total": subs.len(),
            "invoices_total": invoices.len(),
            "invoices_pending": pending_inv,
            "codes_total": codes.len(),
            "codes_redeemed": codes_redeemed,
            "revenue_total": revenue["revenue_total"],
            "revenue_by_asset": revenue["revenue_by_asset"],
        })),
    )
        .into_response()
}

async fn revenue(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let revenue = revenue_json(&state.db, scope);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "total": revenue["revenue_total"],
            "by_asset": revenue["revenue_by_asset"],
        })),
    )
        .into_response()
}

// ---------- Channels ----------
async fn channels_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let _ = session;
    let mut set = std::collections::BTreeSet::new();
    if let Some(c) = state.db.get_config("channel") {
        set.insert(c);
    }
    for p in state.db.list_plans(seller_scope(&session)) {
        if let Some(c) = p.4 {
            set.insert(c);
        }
    }
    let items: Vec<String> = set.into_iter().collect();
    (StatusCode::OK, Json(serde_json::json!({"items": items}))).into_response()
}

// ---------- Config ----------
async fn config_get_channel(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if session.seller_namespace().is_none() {
        return err(
            StatusCode::FORBIDDEN,
            "not_a_seller",
            "seller profile required",
        )
        .into_response();
    }
    let ch = state.db.get_config("channel").unwrap_or_default();
    (StatusCode::OK, Json(serde_json::json!({"channel": ch}))).into_response()
}

#[derive(Deserialize)]
struct ChannelSetReq {
    channel: String,
}
async fn config_set_channel(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<ChannelSetReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    // The global default channel is owner-only (the web `channels_default`
    // route carries the same gate). A seller writing it would steer every
    // tenant's fallback channel, so sellers are refused outright.
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    state.db.set_config("channel", &f.channel);
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

// ---------- AI ----------
async fn ai_templates_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let Some(seller_id) = session.seller_namespace() else {
        return err(
            StatusCode::FORBIDDEN,
            "not_a_seller",
            "seller profile required",
        )
        .into_response();
    };
    let tpls = state.db.list_ai_templates(seller_id);
    let items: Vec<serde_json::Value> = tpls
        .into_iter()
        .map(|t| serde_json::json!({"name": t.0, "description": t.1}))
        .collect();
    (StatusCode::OK, Json(serde_json::json!({"items": items}))).into_response()
}

#[derive(Deserialize)]
struct AiTemplateReq {
    name: String,
    description: Option<String>,
    system_prompt: Option<String>,
    user_prompt: String,
    variables: Option<String>,
}
async fn ai_templates_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<AiTemplateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let Some(seller_id) = session.seller_namespace() else {
        return err(
            StatusCode::FORBIDDEN,
            "not_a_seller",
            "seller profile required",
        )
        .into_response();
    };
    state.db.upsert_ai_template(
        seller_id,
        &f.name,
        f.description.as_deref(),
        f.system_prompt.as_deref(),
        &f.user_prompt,
        f.variables.as_deref(),
    );
    state.db.log_action(
        session.account_id,
        &session.role,
        "ai_template_create",
        "ai_template",
        &f.name,
        None,
    );
    (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn ai_usage(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let Some(seller_id) = session.seller_namespace() else {
        return err(
            StatusCode::FORBIDDEN,
            "not_a_seller",
            "seller profile required",
        )
        .into_response();
    };
    let summary = state.db.ai_usage_summary(seller_id, 30);
    let items: Vec<serde_json::Value> = summary
        .into_iter()
        .map(|s| serde_json::json!({"date": s.0, "tokens": s.1, "requests": s.2}))
        .collect();
    (StatusCode::OK, Json(serde_json::json!({"items": items}))).into_response()
}

// ---------- Finance ----------
async fn finance_summary(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = seller_scope(&session);
    let revenue = revenue_json(&state.db, scope);
    let invoices = state.db.list_invoices(scope, 1000);
    let paid: f64 = invoices.iter().filter(|i| i.3 == "paid").count() as f64;
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "revenue_total": revenue["revenue_total"],
            "revenue_by_asset": revenue["revenue_by_asset"],
            "invoices_total": invoices.len(),
            "invoices_paid": paid,
        })),
    )
        .into_response()
}

// ---------- Finance (remaining) ----------
#[derive(Deserialize)]
struct FinancePayoutCreateReq {
    /// Owning seller. Sellers may only create payouts for themselves; admins
    /// must name the seller explicitly.
    seller_id: Option<i64>,
    amount: String,
    asset: String,
    destination: Option<String>,
}
#[derive(Deserialize)]
struct FinancePayoutPatchReq {
    /// Target status. Only "failed" is accepted, and only from "approved"
    /// (reverting an approved payout that cannot be fulfilled) — see the
    /// allowlist on `finance_payouts_patch`.
    status: String,
}
#[derive(Deserialize)]
struct FinanceRefundCreateReq {
    invoice_id: String,
    /// Defaults to the invoice's own amount/asset: a refund can never move
    /// money the invoice did not collect. Parsed as f64 (non-numeric is a
    /// 400); every role is clamped to the invoice amount and locked to its
    /// asset (§1.2).
    amount: Option<String>,
    asset: Option<String>,
    reason: Option<String>,
    /// Caller-supplied idempotency key. A replay of the same key for the same
    /// invoice returns the existing row without a second audit money event.
    idem_key: Option<String>,
}

async fn finance_revenue_by_seller(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Admin) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "admin or owner required",
        )
        .into_response();
    }
    let items: Vec<serde_json::Value> = state
        .db
        .revenue_by_seller()
        .into_iter()
        .map(|(sid, by_asset)| {
            serde_json::json!({
                "seller_id": sid,
                "revenue_by_asset": by_asset.into_iter()
                    .map(|(asset, amount)| serde_json::json!({"asset": asset, "amount": amount}))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

async fn finance_payouts_list(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = if Role::from_role_str(&session.role) == Role::Seller {
        match session.seller_id {
            Some(s) => Some(s),
            None => {
                return err(StatusCode::FORBIDDEN, "not_a_seller", "no seller profile")
                    .into_response();
            }
        }
    } else if Role::from_role_str(&session.role).has_level(Role::Admin) {
        None
    } else {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "seller or admin required",
        )
        .into_response();
    };
    let payouts = state.db.list_payouts(scope);
    let items: Vec<serde_json::Value> = payouts.into_iter().map(|p| serde_json::json!({
        "id": p.0, "seller_id": p.1, "amount": p.2, "asset": p.3, "destination": p.4, "status": p.5,
    })).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

async fn finance_payouts_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<FinancePayoutCreateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    // Sellers create payouts for themselves; admins must name the seller.
    let seller_id = if Role::from_role_str(&session.role) == Role::Seller {
        match session.seller_id {
            Some(s) => {
                if f.seller_id.is_some() && f.seller_id != Some(s) {
                    return err(
                        StatusCode::FORBIDDEN,
                        "forbidden",
                        "cannot create payouts for other sellers",
                    )
                    .into_response();
                }
                s
            }
            None => {
                return err(StatusCode::FORBIDDEN, "not_a_seller", "no seller profile")
                    .into_response();
            }
        }
    } else if Role::from_role_str(&session.role).has_level(Role::Admin) {
        match f.seller_id {
            Some(s) => s,
            None => {
                return err(StatusCode::BAD_REQUEST, "bad_request", "seller_id required")
                    .into_response();
            }
        }
    } else {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "seller or admin required",
        )
        .into_response();
    };
    // §3.5: a seller with a paid-but-ungranted invoice older than 72h whose
    // auto-refund has not executed carries an unresolved liability. New
    // payouts are blocked until that liability is resolved (the banner cannot
    // be dismissed by the seller). Internal operational target, not an SLA.
    let escalation_cutoff =
        crate::handlers::money::paid_ungranted_escalation_cutoff(chrono::Utc::now());
    if !state
        .db
        .unresolved_ungranted_invoices(seller_id, &escalation_cutoff)
        .is_empty()
    {
        return err(
            StatusCode::CONFLICT,
            "unresolved_ungranted_liability",
            "unresolved ungranted liability: resolve the outstanding refund before creating a payout",
        )
        .into_response();
    }
    if f.amount.trim().is_empty() || f.asset.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "amount/asset required",
        )
        .into_response();
    }
    let id = state
        .db
        .create_payout(seller_id, &f.amount, &f.asset, f.destination.as_deref());
    let meta = serde_json::json!({
        "seller_id": seller_id, "amount": f.amount, "asset": f.asset,
        "destination": f.destination,
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
    (StatusCode::CREATED, Json(serde_json::json!({"id": id}))).into_response()
}

/// Payout approval: pending -> approved. Owner only (two-person money rule:
/// the seller who benefits never approves their own payout).
async fn finance_payouts_approve(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    let payout = match state.db.get_payout(id.to_string()) {
        Some(p) => p,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "payout not found").into_response();
        }
    };
    let (_pid, _seller, ref amount, ref asset, ref destination, status, ..) = payout;
    if status == "paid" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            "payout already executed",
        )
        .into_response();
    }
    if status != "pending" && status != "approved" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            &format!("cannot approve payout in status '{}'", status),
        )
        .into_response();
    }
    state.db.set_payout_status(id, "approved");
    let meta = serde_json::json!({
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
    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "status": "approved"})),
    )
        .into_response()
}

// The REST surface uses the single shared fail-closed classifier in
// `handlers::money`; the former API-local `transfer_outcome_ambiguous` was
// deleted because its "unknown ⇒ not ambiguous" default was fail-open.

/// Payout execution: approved -> paid by moving real money through Crypto Pay
/// `transfer` to the destination Telegram user. Owner only — the execution
/// half of the two-person money rule (the seller who benefits never approves
/// or executes their own payout). The transition is guarded by the DB CAS:
/// the row is FIRST atomically claimed 'approved' -> 'executing' via
/// `begin_payout_execution` (only one executor can win the claim, so two
/// concurrent executors can never both transfer), and only then is the
/// transfer attempted. A DEFINITE API rejection reverts the claim to
/// 'approved' so execution can be retried; an AMBIGUOUS failure (timeout /
/// network, money state unknown) deliberately leaves the row 'executing'
/// with an audit trail for the operator to reconcile on the Finance page
/// before any retry.
async fn finance_payouts_execute(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    let payout = match state.db.get_payout(id.to_string()) {
        Some(p) => p,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "payout not found").into_response();
        }
    };
    let (_pid, seller_id, amount, asset, destination, status, ..) = payout;
    if status == "paid" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            "payout already executed",
        )
        .into_response();
    }
    if status != "approved" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            "payout must be approved before execution",
        )
        .into_response();
    }
    // Crypto Pay transfers go to a Telegram user id: the destination must be
    // exactly that.
    let Some(dest) = destination
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    else {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "payout has no destination",
        )
        .into_response();
    };
    let Ok(dest_user) = dest.parse::<i64>() else {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_destination",
            "destination must be a Telegram user id",
        )
        .into_response();
    };
    // Resolve a crypto token exactly like the webhook does.
    let token = state
        .cfg
        .crypto_token
        .clone()
        .or_else(|| state.db.get_secret_config("crypto_token").ok().flatten());
    let tok = match token {
        Some(t) => t,
        None => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "transfer_unavailable",
                "crypto pay not configured",
            )
            .into_response();
        }
    };
    let cp = std::sync::Arc::new(crate::crypto::CryptoPay::new(tok));
    // Audit the attempt BEFORE claiming the row: an execution attempt is a
    // money event that deserves a trail even when the CAS claim loses a race.
    let meta = serde_json::json!({
        "seller_id": seller_id, "amount": amount, "asset": asset,
        "destination": dest_user,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "payout_execute_attempt",
        "payout",
        id.to_string(),
        Some(&meta),
    );
    // CAS reserve: 'approved' -> 'executing'. The loser of a race must not
    // transfer — this is the double-transfer guard.
    if !state.db.begin_payout_execution(id) {
        let msg = if state.db.get_payout_status(id).as_deref() == Some("executing") {
            "execution already in progress"
        } else {
            "already executing or not approved"
        };
        return err(StatusCode::CONFLICT, "invalid_state", msg).into_response();
    }
    // §4.1: the guarded helper persists a fresh spend_id BEFORE the transfer
    // and passes it to transfer_with_spend_id. No raw transfer() call may be
    // made on this path. A missing spend_id record is a hard error: no money
    // leaves the app unless its idempotency key is durable first.
    let outcome =
        match execute_guarded_transfer(&state.db, "payout", id, seller_id, None, |spend_id| {
            let cp = cp.clone();
            let asset = asset.clone();
            let amount = amount.clone();
            async move {
                cp.transfer_with_spend_id(dest_user, &asset, &amount, &spend_id)
                    .await
            }
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "transfer_not_attempted",
                    &format!("transfer aborted before dispatch: {e}"),
                )
                .into_response();
            }
        };
    match outcome {
        GuardedTransferOutcome::Succeeded { spend_id, receipt } => {
            // Settle the claim: 'executing' -> 'paid' (we own the claim, so
            // this is always the 1-row transition).
            state.db.finish_payout_execution(id, "paid");
            let meta = serde_json::json!({
                "seller_id": seller_id, "amount": amount, "asset": asset,
                "destination": dest_user, "spend_id": spend_id, "receipt": receipt,
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
            (
                StatusCode::OK,
                Json(serde_json::json!({"ok": true, "status": "paid"})),
            )
                .into_response()
        }
        GuardedTransferOutcome::Ambiguous { spend_id, error } => {
            // AMBIGUOUS: the transfer may or may not have been applied. The
            // attempt keeps its spend_id and outcome for the §5 reconciliation
            // flow; the row stays 'executing' (no fresh-key retry, no guessed
            // settlement).
            let meta = serde_json::json!({
                "seller_id": seller_id, "amount": amount, "asset": asset,
                "destination": dest_user, "spend_id": spend_id, "error": error,
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
            err(
                StatusCode::SERVICE_UNAVAILABLE,
                "execution_ambiguous",
                "transfer outcome unknown; the payout stays 'executing' for reconciliation",
            )
            .into_response()
        }
        GuardedTransferOutcome::DefiniteRejected { spend_id, error } => {
            // DEFINITE rejection: the API refused the transfer, no money
            // moved. Revert the claim to 'approved'; a subsequent retry mints
            // a NEW spend_id/attempt (§4.1 definite retry is fresh).
            state.db.finish_payout_execution(id, "approved");
            let meta = serde_json::json!({
                "seller_id": seller_id, "amount": amount, "asset": asset,
                "destination": dest_user, "spend_id": spend_id, "error": error,
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
            err(
                StatusCode::BAD_GATEWAY,
                "transfer_failed",
                &format!("transfer failed: {error}"),
            )
            .into_response()
        }
    }
}

/// Payout status patch. Owner only (mirrors approve/execute/process), and the
/// ONLY transition permitted is 'approved' -> 'failed' — reverting an approved
/// payout that cannot be fulfilled. Every other current or target status is a
/// 409, so this surface can never skip approval (pending -> paid), re-arm an
/// executed payout (paid -> pending) or otherwise route around the create ->
/// approve -> process/execute two-person money rule.
async fn finance_payouts_patch(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(f): Json<FinancePayoutPatchReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    let payout = match state.db.get_payout(id.to_string()) {
        Some(p) => p,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "payout not found").into_response();
        }
    };
    let status = payout.5;
    // Transition allowlist: an approved payout that cannot be fulfilled may be
    // marked failed; anything else is refused.
    if status != "approved" || f.status != "failed" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            &format!("cannot patch payout from '{}' to '{}'", status, f.status),
        )
        .into_response();
    }
    state
        .db
        .update_payout_status_by(id, "failed", Some(session.account_id));
    state.db.log_action(
        session.account_id,
        &session.role,
        "payout_update",
        "payout",
        id.to_string(),
        Some("failed"),
    );
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

/// Payout processing (manual mark-as-paid outside Crypto Pay): approved ->
/// paid. Owner only, and only after approval — money movement follows the
/// create -> approve -> process state machine.
async fn finance_payouts_process(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    let payout = match state.db.get_payout(id.to_string()) {
        Some(p) => p,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "payout not found").into_response();
        }
    };
    let (_pid, _seller, _amount, _asset, _destination, status, ..) = payout;
    if status == "paid" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            "payout already executed",
        )
        .into_response();
    }
    if status != "approved" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            "payout must be approved before processing",
        )
        .into_response();
    }
    state
        .db
        .update_payout_status_by(id, "paid", Some(session.account_id));
    state.db.log_action(
        session.account_id,
        &session.role,
        "payout_process",
        "payout",
        id.to_string(),
        Some("paid"),
    );
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

/// Refund processing (manual mark-as-settled without a Crypto Pay transfer):
/// approved -> executed, applying the SAME full/partial settlement decision as
/// the transfer-backed execute and reconciliation paths (§1.3). The legacy
/// `"processed"` status is forbidden: it bypassed the `is_full_refund` guard
/// and expired the subscription / kicked the buyer even on a PARTIAL refund
/// (security F1). This finalizes money, so it is Owner-only with creator separation
/// just like `finance_refunds_execute` (§2) — an Admin must not be able to
/// bypass the two-person rule through this path.
async fn finance_refunds_process(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = owner_gate(&session) {
        return e.into_response();
    }
    if let Err(e) = creator_gate(&state.db, id, &session) {
        return e.into_response();
    }
    let refund = match state.db.get_refund(id.to_string()) {
        Some(r) => r,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "refund not found").into_response();
        }
    };
    let (
        _rid,
        refund_seller,
        invoice_id,
        refund_user,
        _amount,
        _asset,
        _reason,
        status,
        _created,
        _updated,
        _pa,
        _pb,
    ) = refund;
    if status == "executed" || status == "processed" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            "refund already executed",
        )
        .into_response();
    }
    if status != "approved" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            "refund must be approved before processing",
        )
        .into_response();
    }
    // Finalize the money row as EXECUTED — never the forbidden legacy
    // "processed" status. Side-effects go through the single shared
    // settlement helper so a PARTIAL refund leaves the invoice paid with no
    // subscription expiry and no channel kick (security F1).
    state
        .db
        .update_refund_status_by(id, "executed", Some(session.account_id));
    let subscription_revoked = apply_refund_settlement(
        &state,
        invoice_id.as_deref(),
        refund_user.unwrap_or(0),
        refund_seller,
    )
    .await;
    let meta = serde_json::json!({
        "invoice_id": invoice_id,
        "subscription_revoked": subscription_revoked,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "refund_process",
        "refund",
        id.to_string(),
        Some(&meta),
    );
    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "status": "executed"})),
    )
        .into_response()
}

async fn finance_refunds_list(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let scope = if Role::from_role_str(&session.role) == Role::Seller {
        match session.seller_id {
            Some(s) => Some(s),
            None => {
                return err(StatusCode::FORBIDDEN, "not_a_seller", "no seller profile")
                    .into_response();
            }
        }
    } else if Role::from_role_str(&session.role).has_level(Role::Admin) {
        None
    } else {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "seller or admin required",
        )
        .into_response();
    };
    let refunds = state.db.list_refunds(scope);
    let items: Vec<serde_json::Value> = refunds.into_iter().map(|r| serde_json::json!({
        "id": r.0, "seller_id": r.1, "invoice_id": r.2, "amount": r.3, "asset": r.4, "status": r.5,
    })).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

/// Refund creation: seller-scoped, always rooted in one of the caller's own
/// paid invoices. Amount/asset default to the invoice's, so a refund can
/// never move money the invoice did not collect. The §1.2 cap and asset lock
/// apply to EVERY role (seller, admin and owner) — no unclamped approver
/// shortcut. Creation goes through `create_refund_guarded` with the real
/// actor and an idempotency key; the legacy id-0 audit wrapper is never used.
async fn finance_refunds_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<FinanceRefundCreateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Seller) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "seller or admin required",
        )
        .into_response();
    }
    if f.invoice_id.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "invoice_id required",
        )
        .into_response();
    }
    // Seller scoping: the invoice must belong to the caller (admins may
    // refund any seller's invoice, inheriting its seller).
    let inv = match state.db.get_invoice(&f.invoice_id) {
        Some(i) => i,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "invoice not found").into_response();
        }
    };
    let (inv_amount, inv_asset, inv_status, _desc, _plan, inv_user, inv_seller) = inv;
    if !may_act_on_seller(&session, inv_seller) {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your invoice").into_response();
    }
    // Only settled money can be returned. `paid_ungranted` is still paid
    // money; it is a non-terminal recovery state whose auto-refund uses the
    // same guarded creation path.
    if inv_status != "paid" && inv_status != "paid_ungranted" {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid_state",
            "only paid invoices can be refunded",
        )
        .into_response();
    }
    // §1.2: the asset lock applies to ALL roles, including admin/owner.
    if f.asset
        .as_deref()
        .map(str::trim)
        .is_some_and(|a| a != inv_asset)
    {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid_asset",
            "refund asset must match the invoice asset",
        )
        .into_response();
    }
    let inv_amount_f: f64 = inv_amount.parse().unwrap_or(f64::NAN);
    let amount = match f.amount.as_deref().map(str::trim) {
        None | Some("") => inv_amount.clone(),
        Some(raw) => {
            let Ok(requested) = raw.parse::<f64>() else {
                return err(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "amount must be a number",
                )
                .into_response();
            };
            if requested <= 0.0 {
                return err(
                    StatusCode::BAD_REQUEST,
                    "bad_request",
                    "amount must be positive",
                )
                .into_response();
            }
            // §1.2: clamp for ALL roles — the cumulative cap in
            // create_refund_guarded is the authoritative guard; this clamp
            // keeps the common single-refund case at the documented amount.
            if inv_amount_f.is_nan() || requested > inv_amount_f {
                inv_amount.clone()
            } else {
                raw.to_string()
            }
        }
    };
    let asset = f
        .asset
        .map(|a| a.trim().to_string())
        .filter(|a| !a.is_empty())
        .unwrap_or(inv_asset);
    let idem_key = match f.idem_key.as_deref().map(str::trim) {
        Some(k) if !k.is_empty() => k.to_string(),
        _ => match crate::handlers::money::fresh_idem_key("api") {
            Ok(k) => k,
            Err(e) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    &format!("could not mint idempotency key: {e}"),
                )
                .into_response();
            }
        },
    };
    let role_str = session.role.clone();
    let creation = match state.db.create_refund_guarded(crate::db::RefundInput {
        seller_id: inv_seller,
        invoice_id: Some(f.invoice_id.trim()),
        user_id: inv_user,
        amount: &amount,
        asset: &asset,
        reason: f.reason.as_deref(),
        idem_key: &idem_key,
        created_by: session.account_id,
        created_role: &role_str,
    }) {
        Ok(creation) => creation,
        Err(e) => {
            use crate::db::RefundCreateError;
            let (status, code, msg) = match e {
                RefundCreateError::InvoiceNotFound => {
                    (StatusCode::NOT_FOUND, "not_found", "invoice not found")
                }
                RefundCreateError::InvoiceNotPaid => (
                    StatusCode::BAD_REQUEST,
                    "invalid_state",
                    "only paid invoices can be refunded",
                ),
                RefundCreateError::AssetMismatch => (
                    StatusCode::BAD_REQUEST,
                    "invalid_asset",
                    "refund asset must match the invoice asset",
                ),
                RefundCreateError::UnquantizableLegacy => (
                    StatusCode::CONFLICT,
                    "unquantifiable_legacy_refund",
                    "stored money amount cannot be quantized; owner must resolve it",
                ),
                RefundCreateError::CapExceeded => (
                    StatusCode::CONFLICT,
                    "refund_cap_exceeded",
                    "cumulative refunds would exceed the invoiced amount",
                ),
                RefundCreateError::InFlight => (
                    StatusCode::CONFLICT,
                    "refund_in_flight",
                    "another refund is already in flight for this invoice",
                ),
                RefundCreateError::Database(err) => {
                    tracing::error!("refund creation failed: {}", err);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        "refund creation failed",
                    )
                }
            };
            return err(status, code, msg).into_response();
        }
    };
    // An idempotent replay is the SAME money movement: never emit a second
    // audit event (§1.2 step 5).
    if creation.created_new {
        let meta = serde_json::json!({
            "invoice_id": f.invoice_id, "amount": amount, "asset": asset,
            "user_id": inv_user, "reason": f.reason, "idem_key": idem_key,
        })
        .to_string();
        state.db.log_action(
            session.account_id,
            &session.role,
            "refund_create",
            "refund",
            creation.id.to_string(),
            Some(&meta),
        );
    }
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"id": creation.id, "created": creation.created_new})),
    )
        .into_response()
}

/// Refund approval: pending -> approved. Owner only (§2) — the creator of a
/// refund (seller, admin or owner) may never approve their own money row.
async fn finance_refunds_approve(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = owner_gate(&session) {
        return e.into_response();
    }
    if let Err(e) = creator_gate(&state.db, id, &session) {
        return e.into_response();
    }
    let refund = match state.db.get_refund(id.to_string()) {
        Some(r) => r,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "refund not found").into_response();
        }
    };
    let (_rid, _seller, ref invoice_id, _user, ref amount, ref asset, _reason, ref status, ..) =
        refund;
    if status == "executed" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            "refund already executed",
        )
        .into_response();
    }
    if status != "pending" && status != "approved" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            &format!("cannot approve refund in status '{}'", status),
        )
        .into_response();
    }
    state.db.set_refund_status(id, "approved");
    let meta = serde_json::json!({
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
    (
        StatusCode::OK,
        Json(serde_json::json!({"ok": true, "status": "approved"})),
    )
        .into_response()
}

/// §1.3: apply the money-success effects of a refund. A full refund flips the
/// invoice to `refunded` and revokes the subscription (+ channel kick); a
/// partial refund leaves the invoice `paid` with no kick. Called only after a
/// transfer receipts, or after the owner proves one landed in reconciliation.
async fn apply_refund_settlement(
    state: &WebState,
    invoice_id: Option<&str>,
    user_id: i64,
    seller_id: i64,
) -> bool {
    let Some(inv) = invoice_id else {
        return false;
    };
    if !crate::handlers::money::is_full_refund(&state.db, inv) {
        return false;
    }
    state.db.update_invoice_status(inv, "refunded");
    let Some((sub_id, channel)) = state.db.subscription_by_invoice(inv) else {
        return false;
    };
    state.db.set_subscription_status(sub_id, "refunded");
    if !channel.is_empty() {
        let cid = crate::util::to_chat_id(&channel);
        let kick_bot = state.bots.for_seller(seller_id);
        let _ = kick_bot
            .bot
            .ban_chat_member(cid.clone(), teloxide::types::UserId(user_id as u64))
            .await;
        let _ = kick_bot
            .bot
            .unban_chat_member(cid, teloxide::types::UserId(user_id as u64))
            .await;
    }
    // §3: the buyer hears "refund sent" only on this terminal outcome (never
    // at refund creation or at the 24h "queued" alert). Best-effort DM through
    // the seller's bot; gate_outbound keeps held surfaces silent.
    let notice = format!("Your refund for invoice {inv} has been sent.");
    let kick_bot = state.bots.for_seller(seller_id);
    if crate::handlers::moderation::gate_outbound(&state.db, seller_id, Some(kick_bot.id), &notice)
        .is_ok()
    {
        let _ = kick_bot
            .bot
            .send_message(teloxide::types::ChatId(user_id), notice)
            .await;
    }
    true
}

/// Refund execution: approved -> executed by transferring real money back to
/// the buyer through Crypto Pay `transfer`. The row is FIRST atomically
/// claimed 'approved' -> 'executing' via the DB CAS `begin_refund_execution`
/// (only one executor can win the claim, so two concurrent executors can
/// never both transfer), and only then is the transfer attempted. On success
/// the invoice is marked refunded, any subscription the invoice granted is
/// revoked (status 'refunded' + channel kick via ban+unban), and the whole
/// transition is audited. A DEFINITE API rejection reverts the claim to
/// 'approved' for retry; an AMBIGUOUS failure (timeout / network, money
/// state unknown) deliberately leaves the row 'executing' with an audit
/// trail for the operator to reconcile on the Finance page before any retry.
/// Owner only, with creator separation (§2).
async fn finance_refunds_execute(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = owner_gate(&session) {
        return e.into_response();
    }
    if let Err(e) = creator_gate(&state.db, id, &session) {
        return e.into_response();
    }
    let refund = match state.db.get_refund(id.to_string()) {
        Some(r) => r,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "refund not found").into_response();
        }
    };
    let (
        _rid,
        refund_seller,
        invoice_id,
        refund_user,
        amount,
        asset,
        _reason,
        status,
        _created,
        _updated,
        _pa,
        _pb,
    ) = refund;
    if status == "executed" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            "refund already executed",
        )
        .into_response();
    }
    if status != "approved" {
        return err(
            StatusCode::CONFLICT,
            "invalid_state",
            "refund must be approved before execution",
        )
        .into_response();
    }
    // Crypto Pay transfers target a Telegram user: the buyer the invoice was
    // bound to.
    let Some(refund_user) = refund_user else {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "refund has no bound user; cannot transfer",
        )
        .into_response();
    };
    // Resolve a crypto token exactly like the webhook does.
    let token = state
        .cfg
        .crypto_token
        .clone()
        .or_else(|| state.db.get_secret_config("crypto_token").ok().flatten());
    let tok = match token {
        Some(t) => t,
        None => {
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "transfer_unavailable",
                "crypto pay not configured",
            )
            .into_response();
        }
    };
    let cp = std::sync::Arc::new(crate::crypto::CryptoPay::new(tok));
    // Audit the attempt BEFORE claiming the row: an execution attempt is a
    // money event that deserves a trail even when the CAS claim loses a race.
    let meta = serde_json::json!({
        "invoice_id": invoice_id, "amount": amount, "asset": asset,
        "user_id": refund_user,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "refund_execute_attempt",
        "refund",
        id.to_string(),
        Some(&meta),
    );
    // CAS reserve: 'approved' -> 'executing'. The loser of a race must not
    // transfer — this is the double-transfer guard.
    if !state.db.begin_refund_execution(id) {
        // No dedicated refund status getter exists; re-read the row and look
        // at its status field to distinguish "someone else is executing"
        // from "not approved".
        let executing = state
            .db
            .get_refund(id.to_string())
            .map(|r| r.7 == "executing")
            .unwrap_or(false);
        let msg = if executing {
            "execution already in progress"
        } else {
            "already executing or not approved"
        };
        return err(StatusCode::CONFLICT, "invalid_state", msg).into_response();
    }
    // §4.1: persist a fresh spend_id before dispatch; no raw transfer() call.
    let outcome =
        match execute_guarded_transfer(&state.db, "refund", id, refund_seller, None, |spend_id| {
            let cp = cp.clone();
            let asset = asset.clone();
            let amount = amount.clone();
            async move {
                cp.transfer_with_spend_id(refund_user, &asset, &amount, &spend_id)
                    .await
            }
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                return err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "transfer_not_attempted",
                    &format!("transfer aborted before dispatch: {e}"),
                )
                .into_response();
            }
        };
    match outcome {
        GuardedTransferOutcome::Succeeded { spend_id, receipt } => {
            // Settle the claim: 'executing' -> 'executed' (we own the claim,
            // so this is always the 1-row transition). Invoice/subscription
            // side-effects stay below, best-effort after the status flip.
            state.db.finish_refund_execution(id, "executed");
            let subscription_revoked =
                apply_refund_settlement(&state, invoice_id.as_deref(), refund_user, refund_seller)
                    .await;
            let meta = serde_json::json!({
                "invoice_id": invoice_id, "amount": amount, "asset": asset,
                "user_id": refund_user, "subscription_revoked": subscription_revoked,
                "spend_id": spend_id, "receipt": receipt,
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
            (
                StatusCode::OK,
                Json(serde_json::json!({"ok": true, "status": "executed"})),
            )
                .into_response()
        }
        GuardedTransferOutcome::Ambiguous { spend_id, error } => {
            // AMBIGUOUS: keep the row 'executing' with its persisted spend_id
            // and outcome for operator reconciliation; no settlement effects.
            let meta = serde_json::json!({
                "invoice_id": invoice_id, "amount": amount, "asset": asset,
                "user_id": refund_user, "spend_id": spend_id, "error": error,
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
            err(
                StatusCode::SERVICE_UNAVAILABLE,
                "execution_ambiguous",
                "transfer outcome unknown; the refund stays 'executing' for reconciliation",
            )
            .into_response()
        }
        GuardedTransferOutcome::DefiniteRejected { spend_id, error } => {
            // DEFINITE rejection: no money moved, revert to 'approved'. A
            // retry mints a fresh spend_id/attempt (§4.1).
            state.db.finish_refund_execution(id, "approved");
            let meta = serde_json::json!({
                "invoice_id": invoice_id, "amount": amount, "asset": asset,
                "user_id": refund_user, "spend_id": spend_id, "error": error,
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
            err(
                StatusCode::BAD_GATEWAY,
                "transfer_failed",
                &format!("transfer failed: {error}"),
            )
            .into_response()
        }
    }
}

// ---------- Decision 5: reconciliation (owner-only) ----------

#[derive(Deserialize)]
struct ReconciliationResolveReq {
    /// `executed`, `reverted_to_approved` or `void` (§5.3).
    outcome: String,
    /// Human reason — required and persisted in the audit trail.
    reason: String,
}

/// §5.4 owner view of every executing/needs-reconciliation row with its
/// spend_id and overdue flag. Read-only; no state is guessed or mutated.
async fn finance_reconciliation_list(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = owner_gate(&session) {
        return e.into_response();
    }
    let (due, overdue) = reconciliation_cutoffs(chrono::Utc::now());
    let items: Vec<serde_json::Value> =
        crate::handlers::money::all_reconciliation_rows(&state.db, &due, &overdue)
            .into_iter()
            .map(|r| {
                serde_json::json!({
                    "row_type": r.row_type, "id": r.id, "seller_id": r.seller_id,
                    "amount": r.amount, "asset": r.asset, "status": r.status,
                    "since": r.since, "spend_id": r.spend_id, "overdue": r.overdue,
                })
            })
            .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

/// §5.3 owner resolution of one `needs_reconciliation` row. Exactly three
/// outcomes, each with a non-empty audited reason. The DB CAS makes
/// resolution append-only: a second attempt is rejected (409).
async fn finance_reconciliation_resolve(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path((row_type, id)): Path<(String, i64)>,
    Json(body): Json<ReconciliationResolveReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = owner_gate(&session) {
        return e.into_response();
    }
    if !matches!(row_type.as_str(), "refund" | "payout") {
        return err(StatusCode::NOT_FOUND, "not_found", "unknown money row type").into_response();
    }
    let outcome = match body.outcome.as_str() {
        "executed" => crate::db::ReconciliationOutcome::Executed,
        "reverted_to_approved" => crate::db::ReconciliationOutcome::RevertedToApproved,
        "void" => crate::db::ReconciliationOutcome::Void,
        _ => {
            return err(
                StatusCode::BAD_REQUEST,
                "bad_request",
                "outcome must be executed, reverted_to_approved or void",
            )
            .into_response();
        }
    };
    // security F2: an "executed" resolution of a refund is a manual settlement, so
    // the human creator may never resolve their own money row — exactly the
    // gate the web form applies. Both surfaces call the same shared helper in
    // `handlers::money` so they cannot drift.
    if row_type == "refund"
        && outcome == crate::db::ReconciliationOutcome::Executed
        && crate::handlers::money::creator_owns_refund(&state.db, session.account_id, id)
    {
        return err(
            StatusCode::FORBIDDEN,
            "creator_forbidden",
            "creator cannot settle their own refund",
        )
        .into_response();
    }
    if body.reason.trim().is_empty() {
        return err(
            StatusCode::BAD_REQUEST,
            "reason_required",
            "a non-empty resolution reason is required",
        )
        .into_response();
    }
    // Attach the persisted spend_id before the CAS so an append-only loser
    // cannot emit a second money event.
    let spend_id = state
        .db
        .latest_transfer_attempt(&row_type, id)
        .map(|a| a.spend_id);
    let result =
        state
            .db
            .resolve_reconciliation(&row_type, id, outcome, session.account_id, &body.reason);
    match result {
        Ok(()) => {
            if let Some(ref sid) = spend_id {
                let outcome_token = match outcome {
                    crate::db::ReconciliationOutcome::Executed => "executed",
                    crate::db::ReconciliationOutcome::RevertedToApproved => "reverted_to_approved",
                    crate::db::ReconciliationOutcome::Void => "void",
                };
                state.db.set_transfer_attempt_outcome(sid, outcome_token);
            }
            // An "executed" resolution is an operator proof that the transfer
            // landed: apply the same settlement effects as a receipt.
            if outcome == crate::db::ReconciliationOutcome::Executed
                && row_type == "refund"
                && let Some(r) = state.db.get_refund(id)
            {
                apply_refund_settlement(&state, r.2.as_deref(), r.3.unwrap_or(0), r.1).await;
            }
            let meta = serde_json::json!({
                "row_type": row_type, "outcome": body.outcome,
                "reason": body.reason, "spend_id": spend_id,
                "actor_id": session.account_id,
            })
            .to_string();
            state.db.log_action(
                session.account_id,
                &session.role,
                "transfer_reconciliation_resolved",
                &row_type,
                id.to_string(),
                Some(&meta),
            );
            (
                StatusCode::OK,
                Json(serde_json::json!({"ok": true, "outcome": body.outcome})),
            )
                .into_response()
        }
        Err(e) => {
            use crate::db::ReconcileError;
            let (status, code, msg) = match e {
                ReconcileError::NotFound => {
                    (StatusCode::NOT_FOUND, "not_found", "money row not found")
                }
                ReconcileError::NotDue => (
                    StatusCode::CONFLICT,
                    "not_due",
                    "row is not in needs_reconciliation state",
                ),
                ReconcileError::AlreadyResolved => (
                    StatusCode::CONFLICT,
                    "already_resolved",
                    "row was already resolved",
                ),
                ReconcileError::ReasonRequired => (
                    StatusCode::BAD_REQUEST,
                    "reason_required",
                    "a non-empty resolution reason is required",
                ),
                ReconcileError::Database(err) => {
                    tracing::error!("reconciliation resolution failed: {}", err);
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "internal_error",
                        "resolution failed",
                    )
                }
            };
            err(status, code, msg).into_response()
        }
    }
}

async fn finance_export(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Seller) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "seller or admin required",
        )
        .into_response();
    }
    let scope = seller_scope(&session);
    let invs = state.db.list_invoices(scope, 10000);
    let mut csv = String::from("invoice_id,amount,asset,status,pay_url\n");
    for i in invs {
        csv.push_str(&format!("{},{},{},{},{}\n", i.0, i.1, i.2, i.3, i.4));
    }
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/csv")],
        csv,
    )
        .into_response()
}

// ---------- Products & shop ----------
const PRODUCT_KINDS: [&str; 3] = ["digital_link", "digital_file", "physical"];

#[derive(Deserialize)]
struct ProductCreateReq {
    name: String,
    price: String,
    asset: String,
    kind: String,
    delivery: Option<String>,
    stock: Option<i64>,
}

async fn products_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    // Sellers see their own catalogue; admins see everything.
    let scope = seller_scope(&session);
    let products = state.db.list_products(scope);
    let items: Vec<serde_json::Value> = products
        .into_iter()
        .map(|p| {
            serde_json::json!({
                "id": p.0, "seller_id": p.1, "name": p.2, "price": p.3, "asset": p.4,
                "kind": p.5, "delivery": p.6, "stock": p.7, "active": p.8,
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

async fn products_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<ProductCreateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let seller_id = match session.seller_id {
        Some(s) => s,
        None => {
            return err(
                StatusCode::FORBIDDEN,
                "not_a_seller",
                "seller profile required",
            )
            .into_response();
        }
    };
    if f.name.trim().is_empty()
        || f.price.trim().is_empty()
        || f.asset.trim().is_empty()
        || f.kind.trim().is_empty()
    {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "name/price/asset/kind required",
        )
        .into_response();
    }
    if !PRODUCT_KINDS.contains(&f.kind.as_str()) {
        return err(
            StatusCode::BAD_REQUEST,
            "invalid_kind",
            "kind must be digital_link, digital_file or physical",
        )
        .into_response();
    }
    // A digital product without a payload would take money and fail at
    // delivery; reject it at creation.
    if f.delivery
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
        && f.kind != "physical"
    {
        return err(
            StatusCode::BAD_REQUEST,
            "bad_request",
            "digital products require a delivery payload",
        )
        .into_response();
    }
    match state.db.add_product(
        seller_id,
        f.name.trim(),
        f.price.trim(),
        f.asset.trim(),
        f.kind.trim(),
        f.delivery.as_deref(),
        f.stock,
    ) {
        Ok(id) => {
            let meta = serde_json::json!({
                "name": f.name, "kind": f.kind, "price": f.price, "asset": f.asset,
                "stock": f.stock,
            })
            .to_string();
            state.db.log_action(
                session.account_id,
                &session.role,
                "product_create",
                "product",
                id.to_string(),
                Some(&meta),
            );
            (StatusCode::CREATED, Json(serde_json::json!({"id": id}))).into_response()
        }
        // e.g. UNIQUE(seller_id, name) collision.
        Err(e) => err(StatusCode::CONFLICT, "conflict", &e).into_response(),
    }
}

/// Product deactivation (soft delete): /products/{name}. Seller-scoped;
/// admins may deactivate by name across sellers (ambiguous names are
/// rejected instead of guessing).
async fn products_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let seller_id = if Role::from_role_str(&session.role) == Role::Seller {
        match session.seller_id {
            Some(s) => s,
            None => {
                return err(StatusCode::FORBIDDEN, "not_a_seller", "no seller profile")
                    .into_response();
            }
        }
    } else if Role::from_role_str(&session.role).has_level(Role::Admin) {
        // Resolve the owning seller by name; products are unique per
        // (seller, name), not globally.
        let owners: Vec<i64> = state
            .db
            .list_products(None)
            .into_iter()
            .filter(|p| p.2 == name)
            .map(|p| p.1)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        match owners[..] {
            [s] => s,
            [] => {
                return err(StatusCode::NOT_FOUND, "not_found", "product not found")
                    .into_response();
            }
            _ => {
                return err(
                    StatusCode::BAD_REQUEST,
                    "ambiguous",
                    "product name exists for multiple sellers",
                )
                .into_response();
            }
        }
    } else {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "seller or admin required",
        )
        .into_response();
    };
    if state.db.get_product(seller_id, &name).is_none() {
        return err(StatusCode::NOT_FOUND, "not_found", "product not found").into_response();
    }
    state.db.set_product_active(seller_id, &name, false);
    state.db.log_action(
        session.account_id,
        &session.role,
        "product_deactivate",
        "product",
        name.clone(),
        None,
    );
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

/// Public shop listing: active products across all sellers. No
/// authentication; delivery payloads (the paid content) are never included.
async fn shop_list(State(state): State<WebState>) -> impl IntoResponse {
    let items: Vec<serde_json::Value> = state
        .db
        .list_products(None)
        .into_iter()
        .filter(|p| p.8 != 0)
        .map(|p| {
            serde_json::json!({
                "name": p.2, "price": p.3, "asset": p.4,
                "kind": match p.5.as_str() {
                    "physical" => "product",
                    _ => "digital",
                },
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

async fn admin_sellers(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Admin) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "admin or owner required",
        )
        .into_response();
    }
    let ids = state.db.list_sellers();
    let items: Vec<serde_json::Value> = ids
        .into_iter()
        .map(|id| {
            serde_json::json!({
                "user_id": id, "default_channel": state.db.seller_channel(id),
            })
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

async fn admin_stats(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Admin) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "admin or owner required",
        )
        .into_response();
    }
    let sellers = state.db.count_sellers();
    let users = state.db.count_users();
    let plans = state.db.count_plans(None);
    let subs = state.db.count_subscriptions(None, None);
    let active_subs = state.db.count_subscriptions(None, Some("active"));
    let invoices = state.db.count_invoices(None, None);
    let revenue = revenue_json(&state.db, None);
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "sellers_total": sellers, "users_total": users, "plans_total": plans,
            "subscriptions_total": subs, "subscriptions_active": active_subs,
            "invoices_total": invoices,
            "revenue_total": revenue["revenue_total"],
            "revenue_by_asset": revenue["revenue_by_asset"],
        })),
    )
        .into_response()
}

async fn admin_web_accounts(
    State(state): State<WebState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    let accounts = state.db.list_web_accounts_full();
    let items: Vec<serde_json::Value> = accounts
        .into_iter()
        .map(|a| serde_json::json!({"id": a.0, "username": a.1, "role": a.2, "seller_id": a.3}))
        .collect();
    (StatusCode::OK, Json(serde_json::json!({"items": items}))).into_response()
}

// ---------- Admin (remaining) ----------
#[derive(Deserialize)]
struct AdminSellerPatchReq {
    default_channel: Option<String>,
}
#[derive(Deserialize)]
struct AdminAdminCreateReq {
    user_id: i64,
    username: Option<String>,
}
#[derive(Deserialize)]
struct WebAccountCreateReq {
    username: String,
    password: String,
    role: String,
    seller_id: Option<i64>,
}
#[derive(Deserialize)]
struct WebAccountPasswordReq {
    password: String,
}
#[derive(Deserialize)]
struct AuditQuery {
    limit: Option<i64>,
}

async fn admin_seller_get(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Admin) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "admin or owner required",
        )
        .into_response();
    }
    match state.db.get_seller(user_id) {
        Some((uid, username, channel)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "user_id": uid, "username": username, "default_channel": channel,
            })),
        )
            .into_response(),
        None => err(StatusCode::NOT_FOUND, "not_found", "seller not found").into_response(),
    }
}

async fn admin_seller_patch(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
    Json(f): Json<AdminSellerPatchReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Admin) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "admin or owner required",
        )
        .into_response();
    }
    if let Some(ch) = &f.default_channel {
        state.db.set_seller_channel(user_id, ch);
    }
    state.db.log_action(
        session.account_id,
        &session.role,
        "seller_update",
        "seller",
        user_id.to_string(),
        f.default_channel.as_deref(),
    );
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn admin_admins_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    let admins = state.db.list_admins_detail();
    let items: Vec<serde_json::Value> = admins
        .into_iter()
        .map(|(uid, username)| serde_json::json!({"user_id": uid, "username": username}))
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

async fn admin_admins_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<AdminAdminCreateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    state.db.add_admin(f.user_id, f.username.as_deref());
    state.db.log_action(
        session.account_id,
        &session.role,
        "admin_add",
        "admin",
        f.user_id.to_string(),
        None,
    );
    (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn admin_admins_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(user_id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    state.db.remove_admin(user_id);
    state.db.log_action(
        session.account_id,
        &session.role,
        "admin_remove",
        "admin",
        user_id.to_string(),
        None,
    );
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn admin_web_accounts_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<WebAccountCreateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    match state
        .db
        .create_web_account(&f.username, &f.password, &f.role, f.seller_id)
        .await
    {
        Ok(()) => {
            state.db.log_action(
                session.account_id,
                &session.role,
                "web_account_create",
                "web_account",
                f.username.clone(),
                None,
            );
            (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response()
        }
        Err(e) => err(StatusCode::BAD_REQUEST, "create_failed", &e).into_response(),
    }
}

async fn admin_web_accounts_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    state.db.delete_web_account(id);
    state.db.log_action(
        session.account_id,
        &session.role,
        "web_account_delete",
        "web_account",
        id.to_string(),
        None,
    );
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn admin_web_accounts_password(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Json(f): Json<WebAccountPasswordReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return err(StatusCode::FORBIDDEN, "forbidden", "owner required").into_response();
    }
    if f.password.len() < 6 {
        return err(
            StatusCode::BAD_REQUEST,
            "weak_password",
            "password must be at least 6 chars",
        )
        .into_response();
    }
    match state.db.update_web_password_by_id(id, &f.password).await {
        Ok(()) => {
            state.db.log_action(
                session.account_id,
                &session.role,
                "web_account_password",
                "web_account",
                id.to_string(),
                None,
            );
            (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
        }
        Err(e) => err(StatusCode::BAD_REQUEST, "update_failed", &e).into_response(),
    }
}

async fn admin_audit_log(
    State(state): State<WebState>,
    headers: HeaderMap,
    Query(q): Query<AuditQuery>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Admin) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "admin or owner required",
        )
        .into_response();
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    let logs = state.db.list_audit_log(limit);
    let items: Vec<serde_json::Value> = logs.into_iter().map(|l| serde_json::json!({
        "id": l.0, "actor_id": l.1, "actor_role": l.2, "action": l.3, "target_type": l.4, "target_id": l.5, "metadata": l.6, "created_at": l.7,
    })).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

// ---------- API keys (P3) ----------
#[derive(Deserialize)]
struct ApiKeyCreateReq {
    name: Option<String>,
    scopes: Option<String>,
}

async fn api_keys_list(State(state): State<WebState>, headers: HeaderMap) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Seller) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "seller or admin required",
        )
        .into_response();
    }
    let keys = state.db.list_api_keys_detail(session.account_id);
    let items: Vec<serde_json::Value> = keys.into_iter().map(|k| serde_json::json!({
        "id": k.0, "account_id": k.1, "name": k.2, "key_prefix": k.3, "scopes": k.4, "revoked": k.5, "created_at": k.6,
    })).collect();
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "total": items.len()})),
    )
        .into_response()
}

async fn api_keys_create(
    State(state): State<WebState>,
    headers: HeaderMap,
    Json(f): Json<ApiKeyCreateReq>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Seller) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "seller or admin required",
        )
        .into_response();
    }
    let (id, raw) =
        state
            .db
            .create_api_key_full(session.account_id, f.name.as_deref(), f.scopes.as_deref());
    state.db.log_action(
        session.account_id,
        &session.role,
        "api_key_create",
        "api_key",
        id.to_string(),
        None,
    );
    (
        StatusCode::CREATED,
        Json(serde_json::json!({"id": id, "key": raw, "api_key": raw})),
    )
        .into_response()
}

async fn api_keys_delete(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Seller) {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "seller or admin required",
        )
        .into_response();
    }
    // ownership check for non-admin
    if !Role::from_role_str(&session.role).has_level(Role::Admin) {
        let own = state
            .db
            .list_api_keys(session.account_id)
            .into_iter()
            .any(|k| k.0 == id);
        if !own {
            return err(StatusCode::FORBIDDEN, "forbidden", "not your api key").into_response();
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
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests;
// ---------- Invoices (claim) ----------
async fn invoices_claim(
    State(state): State<WebState>,
    headers: HeaderMap,
    Path(invoice_id): Path<String>,
) -> impl IntoResponse {
    let session = match session_or_401(&state, &headers) {
        Ok(s) => s,
        Err(e) => return e.into_response(),
    };
    let inv = match state.db.get_invoice(&invoice_id) {
        Some(x) => x,
        None => {
            return err(StatusCode::NOT_FOUND, "not_found", "invoice not found").into_response();
        }
    };
    let (_amount, _asset, status, _desc, plan_id, user_id, seller_id) = inv;
    let admin = Role::from_role_str(&session.role).has_level(Role::Admin);
    if !admin
        && Role::from_role_str(&session.role) == Role::Seller
        && session.seller_id != Some(seller_id)
    {
        return err(StatusCode::FORBIDDEN, "forbidden", "not your invoice").into_response();
    }
    // Invoice user binding (identical to /paid): an invoice bound to a user
    // may only be completed by that user (web seller accounts carry their
    // Telegram id as seller_id); unbound invoices only by Admin/Owner.
    let binding_ok = match user_id {
        None => admin,
        Some(u) => admin || session.seller_id == Some(u),
    };
    if !binding_ok {
        return err(
            StatusCode::FORBIDDEN,
            "forbidden",
            "invoice is bound to another user",
        )
        .into_response();
    }
    if status == "paid" {
        // Legacy short-circuit: a paid invoice with its grant already on file
        // (subscription or product grant) is simply done. A paid invoice with
        // NO grant — a pre-claim-protocol row finalized without ever being
        // granted — is requeued ('active') and falls through to the verified
        // claim/grant path below instead of answering already_paid forever.
        let already_granted = state.db.subscription_exists_for_invoice(&invoice_id)
            || state.db.product_grant_exists(&invoice_id);
        if already_granted {
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "granted": false, "invoice_id": invoice_id, "status": "already_paid",
                })),
            )
                .into_response();
        }
        tracing::warn!(
            "invoice {}: legacy paid row without a grant; requeueing for the verified grant path",
            invoice_id
        );
        state.db.update_invoice_status(&invoice_id, "active");
    }
    // Claim (active -> granting) so the background granter and /paid cannot
    // double-grant while we verify.
    if !state.db.claim_invoice_for_grant(&invoice_id) {
        return err(
            StatusCode::CONFLICT,
            "claim_conflict",
            "invoice is already being processed",
        )
        .into_response();
    }
    // Fail-closed: without a crypto token the payment cannot be verified, so
    // nothing is granted. Release the claim for a later attempt.
    let token = state
        .cfg
        .crypto_token
        .clone()
        .or_else(|| state.db.get_secret_config("crypto_token").ok().flatten());
    let tok = match token {
        Some(t) => t,
        None => {
            state.db.release_invoice_claim(&invoice_id);
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "verification_unavailable",
                "payment verification unavailable: crypto pay not configured",
            )
            .into_response();
        }
    };
    // The optional base URL is test-only; production keeps the canonical
    // endpoint via `CryptoPay::new`.
    let cp = match state.cfg.crypto_api_base.as_deref() {
        Some(base) => crate::crypto::CryptoPay::with_base_url(tok, base),
        None => crate::crypto::CryptoPay::new(tok),
    };
    let paid = match crate::services::grant::verify_paid(&cp, &invoice_id).await {
        Ok(paid) => paid,
        Err(e) => {
            // Verification error: release the claim; the background granter
            // retries. Never grant on doubt.
            state.db.release_invoice_claim(&invoice_id);
            return err(
                StatusCode::BAD_GATEWAY,
                "verification_failed",
                &format!("could not verify payment: {}", e),
            )
            .into_response();
        }
    };
    if !paid {
        state.db.release_invoice_claim(&invoice_id);
        return err(StatusCode::BAD_REQUEST, "not_paid", "invoice not paid").into_response();
    }
    // Verified paid. Grant once (idempotent), then finalize.
    // Every Telegram side-effect below (product delivery DM, plan invite link,
    // grant DM) belongs to the invoice's seller, so it runs on that seller's
    // bot — the same resolution the background granter uses. Seller 0
    // (legacy/global invoices) resolves to the primary bot.
    let invoice_bot = state.bots.for_seller(seller_id);
    let mut granted = false;
    let target_user = user_id.unwrap_or(0);
    // Product invoices take the delivery path; plan invoices the
    // subscription path below. The branch is selected by invoices.product_id.
    if let Some((pid, kind, pname, delivery)) = state.db.invoice_product_delivery(&invoice_id) {
        if target_user != 0 && !state.db.product_grant_exists(&invoice_id) {
            match state
                .db
                .grant_product(seller_id, target_user, pid, &invoice_id)
            {
                Ok(true) => {
                    granted = true;
                    if let Err(e) = crate::services::deliver::deliver(
                        &invoice_bot.bot,
                        target_user,
                        &kind,
                        &pname,
                        delivery.as_deref(),
                        &state.db,
                        seller_id,
                        Some(invoice_bot.id),
                    )
                    .await
                    {
                        tracing::warn!(
                            "invoice {}: product '{}' granted but delivery DM failed: {}",
                            invoice_id,
                            pname,
                            e
                        );
                    }
                }
                // A concurrent granter won the UNIQUE(invoice_id) race.
                Ok(false) => {}
                // e.g. out of stock: money settled, grant not — surface and
                // finalize so the invoice stops re-queueing.
                Err(e) => {
                    tracing::warn!(
                        "invoice {}: product grant failed ({}); finalizing without delivery",
                        invoice_id,
                        e
                    );
                }
            }
        }
        state.db.finalize_invoice_paid(&invoice_id);
        let meta = serde_json::json!({
            "product_id": pid, "kind": kind, "name": pname, "granted": granted,
        })
        .to_string();
        state.db.log_action(
            session.account_id,
            &session.role,
            "invoice_claim",
            "invoice",
            &invoice_id,
            Some(&meta),
        );
        return (
            StatusCode::OK,
            Json(serde_json::json!({"granted": granted, "invoice_id": invoice_id})),
        )
            .into_response();
    }
    if !state.db.subscription_exists_for_invoice(&invoice_id) && target_user != 0 {
        // Seller-scoped plan lookup: the invoice's plan is resolved under the
        // invoice's own seller, never a global same-name plan owned by
        // someone else. Tuple: (id, price, asset, name, days, channel).
        let plan_info = plan_id
            .as_ref()
            .and_then(|pn| state.db.get_plan(seller_id, pn));
        let days = plan_info
            .as_ref()
            .map(|p| p.4)
            .unwrap_or(30)
            .clamp(1, crate::constants::MAX_PLAN_DAYS);
        let sid = seller_id;
        // The channel must belong to the invoice's seller (via its plan) —
        // the global default fallback stays reserved for Admin/Owner.
        let ch = plan_info
            .as_ref()
            .and_then(|p| p.5.clone())
            .or_else(|| state.db.seller_channel(sid))
            .or_else(|| {
                if admin {
                    state.db.get_config("channel")
                } else {
                    None
                }
            })
            .unwrap_or_default();
        if ch.is_empty() {
            // Paid but ungrantable: release the claim so the background
            // granter retries once the seller configures a channel — never
            // finalize into a paid-without-grant wedge (mirrors /paid).
            state.db.release_invoice_claim(&invoice_id);
            return err(
                StatusCode::SERVICE_UNAVAILABLE,
                "seller_channel_missing",
                "seller has no channel configured; the payment will be granted automatically once setup completes",
            )
            .into_response();
        }
        {
            let plan_db_id = plan_info.as_ref().map(|p| p.0);
            state.db.create_subscription(
                sid,
                target_user,
                plan_db_id,
                &ch,
                days,
                Some(&invoice_id),
                None,
            );
            // Checked expiry math; defensive even behind the clamp above.
            let expire = chrono::Duration::try_days(days)
                .and_then(|d| chrono::Utc::now().checked_add_signed(d))
                .unwrap_or_else(chrono::Utc::now);
            let link = invoice_bot
                .bot
                .create_chat_invite_link(crate::util::to_chat_id(&ch))
                .member_limit(1)
                .expire_date(expire)
                .await
                .ok()
                .map(|l| l.invite_link);
            if let Some(l) = link {
                let grant_text = format!("Payment confirmed. Access granted.\nInvite: {}", l);
                // security outbound gate before this raw-Bot DM (the grant row is
                // already durable; only the notice is skipped while held).
                if crate::handlers::moderation::gate_outbound(
                    &state.db,
                    seller_id,
                    Some(invoice_bot.id),
                    &grant_text,
                )
                .is_ok()
                {
                    let _ = invoice_bot
                        .bot
                        .send_message(teloxide::types::ChatId(target_user), grant_text)
                        .await;
                }
            }
            granted = true;
        }
    }
    state.db.finalize_invoice_paid(&invoice_id);
    state.db.log_action(
        session.account_id,
        &session.role,
        "invoice_claim",
        "invoice",
        &invoice_id,
        None,
    );
    (
        StatusCode::OK,
        Json(serde_json::json!({"granted": granted, "invoice_id": invoice_id})),
    )
        .into_response()
}

// ---------- Webhook ----------
async fn webhook_cryptopay(
    State(state): State<WebState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    let token = state
        .cfg
        .crypto_token
        .clone()
        .or_else(|| state.db.get_secret_config("crypto_token").ok().flatten());
    // Fail closed when crypto pay is not configured: there is no secret to
    // authenticate the webhook against.
    let Some(tok) = token else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"crypto pay not configured"})),
        )
            .into_response();
    };
    // The signature header is mandatory whenever a token is configured.
    let Some(sig) = headers
        .get("x-crypto-pay-api-signature")
        .and_then(|v| v.to_str().ok())
    else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"missing signature"})),
        )
            .into_response();
    };
    let computed = crate::util::hmac_sha256(tok.as_bytes(), &body);
    let hex: String = computed.iter().map(|b| format!("{:02x}", b)).collect();
    // Constant-time compare: never short-circuit on the first mismatching
    // byte.
    if !crate::util::ct_eq(&hex, sig) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error":"bad signature"})),
        )
            .into_response();
    }
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(x) => x,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error":"bad json"})),
            )
                .into_response();
        }
    };
    let payload = v.get("payload").cloned().unwrap_or(v.clone());
    let inv_id_str = if let Some(s) = payload.get("invoice_id") {
        if let Some(n) = s.as_i64() {
            n.to_string()
        } else {
            s.as_str().unwrap_or("").to_string()
        }
    } else {
        String::new()
    };
    let status = payload.get("status").and_then(|s| s.as_str()).unwrap_or("");
    if !inv_id_str.is_empty() && status == "paid" {
        // The webhook only ENQUEUES the invoice ('active' = the grant queue);
        // granting is left to the claiming flows (background granter, /paid,
        // claim API), which re-verify against the provider and grant
        // idempotently. Marking 'paid' here would deadlock the money path —
        // pending_invoices() feeds 'active' rows to the granter, never 'paid'
        // ones. Rows already in a claimer- or operator-owned state are left
        // alone: 'paid' means the grant already finalized, 'failed' is
        // operator-reversible only, a live 'granting' claim belongs to its
        // claimer (staleness recovery handles a dead one), and
        // 'refunded'/'processed' are refund outcomes — a replayed webhook must
        // never resurrect a refunded invoice into the grant queue only for the
        // granter to flip it straight back to paid.
        let current = state.db.get_invoice(&inv_id_str).map(|i| i.2);
        if !matches!(
            current.as_deref(),
            Some("paid" | "failed" | "granting" | "refunded" | "processed")
        ) {
            state.db.update_invoice_status(&inv_id_str, "active");
        }
        state
            .db
            .log_action(0, "system", "webhook_paid", "invoice", &inv_id_str, None);
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/health", get(health))
        // Prometheus scrape endpoint. No auth by design: the default bind is
        // 127.0.0.1 and this MUST NOT be exposed publicly. Labels are fixed
        // route categories only — see src/metrics.rs for the invariant.
        .route("/metrics", get(crate::metrics::render_metrics))
        // auth
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/auth/me", get(auth_me))
        .route("/api/v1/auth/password", post(auth_password))
        .route("/api/v1/auth/mfa/verify", post(auth_mfa_verify))
        // sellers
        .route("/api/v1/sellers", get(sellers_list).post(sellers_create))
        .route("/api/v1/sellers/me", get(sellers_me))
        .route("/api/v1/sellers/{user_id}", get(seller_get))
        .route(
            "/api/v1/sellers/{user_id}/channel",
            post(seller_set_channel),
        )
        .route(
            "/api/v1/sellers/{user_id}/ai-settings",
            get(seller_get_ai_settings).post(seller_set_ai_settings),
        )
        // plans
        .route("/api/v1/plans", get(plans_list).post(plans_create))
        .route(
            "/api/v1/plans/{id}",
            get(plans_get).patch(plans_update).delete(plans_delete),
        )
        .route("/api/v1/plans/{id}/active", post(plans_active))
        // subscriptions
        .route("/api/v1/subscriptions", get(subs_list).post(subs_create))
        .route(
            "/api/v1/subscriptions/{id}",
            get(subs_get).patch(subs_patch),
        )
        .route(
            "/api/v1/subscriptions/expire-check",
            post(subs_expire_check),
        )
        .route("/api/v1/users/{user_id}/subscriptions", get(user_subs))
        // invoices
        .route("/api/v1/invoices", get(invoices_list).post(invoices_create))
        .route("/api/v1/invoices/{invoice_id}", get(invoices_get))
        .route(
            "/api/v1/invoices/{invoice_id}/status",
            post(invoices_status),
        )
        .route("/api/v1/invoices/{invoice_id}/claim", post(invoices_claim))
        .route("/api/v1/webhooks/cryptopay", post(webhook_cryptopay))
        // codes
        .route("/api/v1/codes", get(codes_list).post(codes_create))
        .route("/api/v1/codes/reclaim", post(codes_reclaim))
        .route("/api/v1/codes/{code}", get(codes_get).delete(codes_delete))
        .route("/api/v1/codes/{code}/redeem", post(codes_redeem))
        .route("/api/v1/codes/{code}/redemptions", get(codes_redemptions))
        // posts
        .route("/api/v1/posts", get(posts_list).post(posts_create))
        .route("/api/v1/posts/schedule", post(posts_schedule))
        .route(
            "/api/v1/posts/{id}",
            get(posts_get).patch(posts_update).delete(posts_delete),
        )
        .route("/api/v1/posts/{id}/pin", post(posts_pin))
        .route("/api/v1/posts/{id}/unpin", post(posts_unpin))
        // broadcasts
        .route("/api/v1/broadcasts", post(broadcasts))
        // users
        .route("/api/v1/users", get(users_list))
        .route("/api/v1/users/upsert", post(users_upsert))
        .route("/api/v1/users/{user_id}", get(users_get))
        // stats
        .route("/api/v1/dashboard/summary", get(dashboard_summary))
        .route("/api/v1/revenue", get(revenue))
        // channels
        .route("/api/v1/channels", get(channels_list))
        .route(
            "/api/v1/channels/{id}",
            get(channels_get).patch(channels_patch),
        )
        .route(
            "/api/v1/config/channel",
            get(config_get_channel).post(config_set_channel),
        )
        // ai
        .route(
            "/api/v1/ai/templates",
            get(ai_templates_list).post(ai_templates_create),
        )
        .route(
            "/api/v1/ai/templates/{name}",
            get(ai_templates_get)
                .patch(ai_templates_update)
                .delete(ai_templates_delete),
        )
        .route("/api/v1/ai/usage", get(ai_usage))
        // products (seller scope) + public shop
        .route("/api/v1/products", get(products_list).post(products_create))
        .route("/api/v1/products/{name}", delete(products_delete))
        .route("/api/v1/shop", get(shop_list))
        // finance
        .route("/api/v1/finance/summary", get(finance_summary))
        .route(
            "/api/v1/finance/revenue-by-seller",
            get(finance_revenue_by_seller),
        )
        .route(
            "/api/v1/finance/payouts",
            get(finance_payouts_list).post(finance_payouts_create),
        )
        .route("/api/v1/finance/payouts/{id}", patch(finance_payouts_patch))
        .route(
            "/api/v1/finance/payouts/{id}/process",
            post(finance_payouts_process),
        )
        .route(
            "/api/v1/finance/payouts/{id}/approve",
            post(finance_payouts_approve),
        )
        .route(
            "/api/v1/finance/payouts/{id}/execute",
            post(finance_payouts_execute),
        )
        .route(
            "/api/v1/finance/refunds",
            get(finance_refunds_list).post(finance_refunds_create),
        )
        .route(
            "/api/v1/finance/refunds/{id}/process",
            post(finance_refunds_process),
        )
        .route(
            "/api/v1/finance/refunds/{id}/approve",
            post(finance_refunds_approve),
        )
        .route(
            "/api/v1/finance/refunds/{id}/execute",
            post(finance_refunds_execute),
        )
        .route("/api/v1/finance/export", get(finance_export))
        // decision 5 — reconciliation (owner-only)
        .route(
            "/api/v1/finance/reconciliation",
            get(finance_reconciliation_list),
        )
        .route(
            "/api/v1/finance/reconciliation/{row_type}/{id}/resolve",
            post(finance_reconciliation_resolve),
        )
        // admin
        .route("/api/v1/admin/sellers", get(admin_sellers))
        .route(
            "/api/v1/admin/sellers/{user_id}",
            get(admin_seller_get).patch(admin_seller_patch),
        )
        .route("/api/v1/admin/stats/global", get(admin_stats))
        .route(
            "/api/v1/admin/admins",
            get(admin_admins_list).post(admin_admins_create),
        )
        .route(
            "/api/v1/admin/admins/{user_id}",
            delete(admin_admins_delete),
        )
        .route(
            "/api/v1/admin/web-accounts",
            get(admin_web_accounts).post(admin_web_accounts_create),
        )
        .route(
            "/api/v1/admin/web-accounts/{id}",
            delete(admin_web_accounts_delete),
        )
        .route(
            "/api/v1/admin/web-accounts/{id}/password",
            patch(admin_web_accounts_password),
        )
        .route("/api/v1/admin/audit-log", get(admin_audit_log))
        // api keys
        .route(
            "/api/v1/auth/api-keys",
            get(api_keys_list).post(api_keys_create),
        )
        .route("/api/v1/auth/api-keys/{id}", delete(api_keys_delete))
        .with_state(state)
}
