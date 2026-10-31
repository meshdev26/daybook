use super::*;
// ---------- API keys (web console, seller+) ----------

/// The API keys page body. `banner` carries the outcome of a create (rendered
/// above the panels so the POST can answer in place). Keys are scoped to the
/// signed-in account exactly like the REST handlers (`api_keys_list` et al. in
/// api.rs): the raw key exists only in the create banner of the one response
/// that issues it — the database stores its hash, and the raw value is never
/// written to the audit log or rendered again.
pub(super) fn keys_body(state: &WebState, session: &Session, banner: Option<&str>) -> String {
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
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Table"><table aria-label="API keys"><thead><tr><th scope="col">Name</th><th scope="col">Key prefix</th><th scope="col">Created</th><th scope="col">Actions</th></tr></thead><tbody>{}</tbody></table></div>"#,
        if rows.is_empty() {
            r#"<tr><td colspan="4" class="empty"><b>No API keys yet — create one to call the REST API</b></td></tr>"#.to_string()
        } else {
            rows
        }
    );
    let form = format!(
        r#"<article class="panel"><h3>Create key</h3>
<form method="post" action="{}">
<label for="name">Name</label><input id="name" type="text" name="name" placeholder="CI deploys">
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
pub(super) struct KeysQuery {
    pub(super) revoked: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn keys_page(
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
pub(super) struct KeysCreateForm {
    pub(super) name: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn keys_create(
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
pub(super) struct KeysDeleteForm {
    pub(super) id: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn keys_delete(
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
pub(super) async fn settings_setupbot(
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
    // Profile changes apply to the effective bot: the selected bot when the
    // owner is scoped, otherwise the primary.
    let bot = effective_bot(&state, &session);
    let commands = bot.bot.set_my_commands(cmds).await;
    let description = bot
        .bot
        .set_my_description()
        .description(
            "Telegram shop: digital products, channel memberships, codes and bookings. Browse /shop — buy with /buy.",
        )
        .await;
    let short_description = bot
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
