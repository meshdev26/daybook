use super::*;
#[derive(Deserialize)]
pub(super) struct PlansQuery {
    pub(super) error: Option<String>,
    pub(super) created: Option<String>,
    pub(super) deleted: Option<String>,
}

/// Plans-page flash banners (EDIT_FLOW_SPEC E1): success keys render the
/// house "{Object} {verb}." copy; error keys map to human sentences — the
/// raw URL key never reaches the screen. The URL keys stay byte-identical
/// (tests/web_base_path.rs pins `{BASE}/plans?error=invalid_duration`); only
/// the rendered copy is mapped. Unknown keys render nothing.
fn plans_banner(q: &PlansQuery) -> String {
    if q.created.as_deref() == Some("1") {
        return flash_success("Plan created.");
    }
    if q.deleted.as_deref() == Some("1") {
        return flash_success("Plan deleted.");
    }
    if let Some(e) = q.error.as_ref() {
        let msg = match e.as_str() {
            "invalid_duration" => "Duration must be a positive number of days.",
            "missing_fields" => "Fill in name, price and asset.",
            _ => return String::new(),
        };
        return format!(r#"<div class="alert alert-error" role="alert">{msg}</div>"#);
    }
    String::new()
}

/// The one PRG success banner shape (subs.rs:63-66 idiom), shared by the
/// console surfaces this pass touched (content, codes, promos, mfa_users).
pub(super) fn flash_success(msg: &str) -> String {
    format!(r#"<div class="alert alert-success" role="status" aria-live="polite">{msg}</div>"#)
}

#[tracing::instrument(skip_all)]
pub(super) async fn plans_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<PlansQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.tenant_scope().is_none() {
        return forbidden_page(&state, "Plans", "Plans").into_response();
    }
    let seller_scope = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id
    } else {
        None
    };
    let bot_scope = bot_scope_sellers(&state, &session);
    let plans = state
        .db
        .list_plans(seller_scope)
        .into_iter()
        .filter(|p| in_bot_scope(&bot_scope, p.6))
        .collect::<Vec<_>>();
    let list_items = if plans.is_empty() {
        "<li class=\"empty\"><b>No plans yet.</b>Create one below — it appears in the shop immediately.</li>"
            .to_string()
    } else {
        plans
            .iter()
            .map(|p| {
                format!(
                    "<li><b>{}</b><span class=\"mono\">{} {}</span><span class=\"help\">/ {} days</span><span class=\"row-action\"><form method=\"post\" action=\"{}\" class=\"inline-form\"><input type=\"hidden\" name=\"name\" value=\"{}\"><button type=\"submit\" class=\"btn btn-danger btn-sm\" aria-label=\"Delete plan {}\" onclick=\"return confirm('Delete plan {}? Buyers can no longer buy it.')\">Delete</button></form></span></li>",
                    esc(&p.0),
                    esc(&p.1),
                    esc(&p.2),
                    p.3,
                    state.url("/plandel"),
                    esc(&p.0),
                    esc(&p.0),
                    esc(&p.0)
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let banner = plans_banner(&q);
    let body = format!(
        r#"{}<h1>Plans <span class="sub">what buyers can purchase</span></h1>
<article class="panel"><h3>Create plan</h3><form method="post" action="{}">
<label for="name">Name</label><input id="name" name="name" required>
<label for="price">Price</label><input id="price" name="price" required>
<label for="asset">Asset</label><input id="asset" name="asset" required>
<label for="duration_days">Duration days</label><input id="duration_days" name="duration_days" type="number" required>
<label for="channel">Channel</label><input id="channel" name="channel" placeholder="@channel (optional)">
<button type="submit">Create plan</button></form></article>
<article class="panel"><ul>{}</ul></article>"#,
        banner,
        state.url("/plan"),
        list_items
    );
    Html(html_page(&state, Some(&session), "Plans", &body)).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn posts_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.tenant_scope().is_none() {
        return forbidden_page(&state, "Posts", "Posts").into_response();
    }
    let scope = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id
    } else {
        None
    };
    let posts = state.db.list_posts(scope, 50);
    let titles = channel_titles(&state);
    let rows = if posts.is_empty() {
        r#"<tr><td colspan="5" class="empty"><b>No posts yet.</b> Use the dashboard form or Telegram commands to post.</td></tr>"#.to_string()
    } else {
        posts
            .iter()
            .map(|p| {
                // p.7 is the row's recorded sender ('bot' | 'user'): the
                // badge marks only the exceptional user-bound rows
                // (IDENTITY_SENDER_UX_SPEC §4 — exception-only marking, same
                // word and badge as the Channels row, directly after the
                // channel so the pair reads as one fact).
                let sender_badge = if p.7 == "user" {
                    " <span class=\"badge badge-warning\">as user</span>"
                } else {
                    ""
                };
                format!(
                    r#"<tr><td class="mono">{}</td><td class="mono">{}{sender_badge}</td><td>{}</td><td>{}</td><td class="mono">{}</td></tr>"#,
                    p.0,
                    channel_label(titles.get(&p.2.to_ascii_lowercase()).map(String::as_str), &p.2),
                    esc(&p.4),
                    esc(p.5.as_deref().unwrap_or("")),
                    esc(&p.6)
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let table = format!(
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Recent posts"><table aria-label="Recent posts"><thead><tr><th scope="col">ID</th><th scope="col">Channel</th><th scope="col">Kind</th><th scope="col">Content</th><th scope="col">Created</th></tr></thead><tbody>{}</tbody></table></div>"#,
        rows
    );
    // M2: the gate refuses held edits before transport; the only feedback is
    // this opaque, content-free status line (no held body, no buyer presence).
    let held_banner = if q.held.as_deref() == Some("1") {
        r#"<div class="alert alert-warn" role="status" aria-live="polite">The edit was held by the content policy. Nothing was changed.</div>"#
    } else {
        ""
    };
    let body = format!(
        r#"<h1>Posts <span class="sub">recent sends</span></h1>
{held_banner}<p class="help">Use the <a href="{}">dashboard form</a> to create a post.</p>
{}"#,
        state.url("/dashboard"),
        table,
        held_banner = held_banner
    );
    Html(html_page(&state, Some(&session), "Posts", &body)).into_response()
}

#[derive(Deserialize)]
pub(super) struct PostForm {
    pub(super) channel: Option<String>,
    pub(super) text: String,
}

/// The i64 chat id the [`crate::identity::ContentSender`] contract takes,
/// from a console channel string: numeric Bot-API ids pass through
/// unchanged; `@username` names resolve through the Bot API's `get_chat`.
/// Resolution is transport only — which identity sends is decided by the
/// channel binding (`BotManager::for_channel_sender`), never here. `None`
/// when the name cannot be resolved: the caller surfaces an honest error and
/// nothing is sent.
///
/// The bot is a parameter, not `state.bot`: `get_chat` only answers for a chat
/// the asking bot can see, so a global owner who selected a bot resolves names
/// through that bot (the caller passes `effective_bot`). Resolution stays
/// transport only — it never decides who sends.
pub(super) async fn channel_chat_id(bot: &Bot, channel: &str) -> Option<i64> {
    channel_chat_info(bot, channel).await.map(|(id, _)| id)
}

/// Extract the path portion of a `t.me` link, if the input is one.
/// Accepts `https://t.me/xxx`, `http://t.me/xxx` and `t.me/xxx`.
pub(super) fn tme_link_path(input: &str) -> Option<String> {
    let s = input.trim();
    let lower = s.to_ascii_lowercase();
    let rest = if lower.starts_with("https://t.me/") {
        &s["https://".len() + "t.me/".len()..]
    } else if lower.starts_with("http://t.me/") {
        &s["http://".len() + "t.me/".len()..]
    } else if lower.starts_with("t.me/") {
        &s["t.me/".len()..]
    } else {
        return None;
    };
    // Strip query / fragment / trailing slash.
    let rest = rest.split('?').next().unwrap_or(rest);
    let rest = rest.split('#').next().unwrap_or(rest);
    let rest = rest.trim_end_matches('/');
    Some(rest.to_string())
}

/// Normalize a channel-ish input to a form `get_chat` can resolve:
/// - numeric id -> as-is (handled by caller)
/// - @handle -> as-is
/// - t.me/<handle> -> @handle
/// - t.me/+hash or t.me/joinchat/<hash> -> the invite link (returned as-is
///   for a best-effort `get_chat`, which is expected to fail)
///
/// Used by the channels-add Public path (CHANNEL_ADD_REDESIGN_SPEC §3.4.3):
/// the public visibility stores the normalized `@handle`, so the shape-
/// derived badge stays truthful (§6).
pub(super) fn normalize_channel_input(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.parse::<i64>().is_ok() {
        return Some(trimmed.to_string());
    }
    if trimmed.starts_with('@') {
        return Some(trimmed.to_string());
    }
    if let Some(path) = tme_link_path(trimmed) {
        if path.starts_with('+') || path.starts_with("joinchat/") {
            // Invite link: keep the original input for a best-effort lookup.
            return Some(trimmed.to_string());
        }
        // Public handle.
        let handle = path.split('/').next().unwrap_or("").trim();
        if handle.is_empty() {
            return None;
        }
        let handle = handle.trim_start_matches('@');
        return Some(format!("@{}", handle));
    }
    // Bare display title or anything else: Telegram cannot resolve by title,
    // so do not attempt a lookup.
    None
}

/// Resolve a channel string to a canonical numeric chat id AND its Telegram
/// title (when the bot can see it), so the console can store and display a
/// human-readable name alongside the id instead of a bare number.
pub(super) async fn channel_chat_info(bot: &Bot, channel: &str) -> Option<(i64, Option<String>)> {
    let trimmed = channel.trim();
    if let Ok(id) = trimmed.parse::<i64>() {
        // Numeric id supplied directly: try to fetch the title for display,
        // but never fail resolution just because the title lookup failed.
        let title = bot
            .get_chat(crate::util::to_chat_id(trimmed))
            .await
            .ok()
            .and_then(|info| info.title().map(str::to_string));
        return Some((id, title));
    }

    // t.me invite link: attempt get_chat with the link as-is; on failure the
    // caller surfaces a helpful error (Telegram cannot resolve invite links
    // through get_chat in general).
    if let Some(path) = tme_link_path(trimmed)
        && (path.starts_with('+') || path.starts_with("joinchat/"))
    {
        match bot.get_chat(trimmed.to_string()).await {
            Ok(info) => return Some((info.id.0, info.title().map(str::to_string))),
            Err(e) => {
                tracing::warn!(channel = %trimmed, "invite link resolution failed: {}", e);
                return None;
            }
        }
    }

    // Resolve @handle or a t.me/<handle> link to a canonical numeric id.
    let resolved_handle = if let Some(path) = tme_link_path(trimmed) {
        let handle = path.split('/').next().unwrap_or("").trim();
        let handle = handle.trim_start_matches('@');
        if handle.is_empty() {
            return None;
        }
        format!("@{}", handle)
    } else if trimmed.starts_with('@') {
        trimmed.to_string()
    } else {
        // Bare display title or unsupported input: do not attempt resolution.
        return None;
    };

    match bot
        .get_chat(crate::util::to_chat_id(&resolved_handle))
        .await
    {
        Ok(info) => Some((info.id.0, info.title().map(str::to_string))),
        Err(e) => {
            tracing::warn!(channel = %trimmed, "chat id resolution failed: {}", e);
            None
        }
    }
}

/// Fixed flash key + optional retry seconds for a `SendError` — the
/// dashboard renders the honest copy server-side from these keys, so no free
/// text rides the URL (IDENTITY_SENDER_UX_SPEC §7.5).
pub(super) fn send_error_flash(e: &crate::identity::SendError) -> (&'static str, Option<u64>) {
    match e {
        crate::identity::SendError::WritesDisabled => ("writes_disabled", None),
        crate::identity::SendError::CapReached {
            retry_after_secs, ..
        } => ("cap", Some(*retry_after_secs)),
        crate::identity::SendError::FloodWait(secs) => ("flood", Some(*secs)),
        crate::identity::SendError::NotAuthorized => ("not_authorized", None),
        crate::identity::SendError::ChatNotFound => ("chat", None),
        crate::identity::SendError::SurfaceHalted => ("halted", None),
        crate::identity::SendError::Transport(_) => ("transport", None),
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn create_post(
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
    // Tenant gate on the resolved target (review H1): the send runs through
    // the bot bound to that channel, so a seller may only address the global
    // set the dropdown offers or a channel they own — never another seller's
    // channel. Checked before any Telegram traffic and before the row write.
    if !may_post_to_channel(&state, &session, &ch) {
        return forbidden_page(&state, "Posts", "Posts");
    }
    // Sender resolution (architecture decision 1, IDENTITY_SENDER_UX_SPEC §1): the
    // channel binding decides who posts — before any Telegram traffic, with
    // no fallback. A user-bound channel that fails fails as a user-bound
    // send (§3 honesty).
    let sender = state.bots.for_channel_sender(&ch);
    // Personal-identity posting is owner-only (security finding 1): a non-owner
    // session whose target resolves to the operator's personal account is
    // refused BEFORE any Telegram traffic — no bot fallback, no send, no
    // log_post row. Fail-closed on every non-owner role.
    if !sender.is_bot() && !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Posts", "Personal-identity posting");
    }
    let sender_word = if sender.is_bot() { "bot" } else { "user" };
    // The ContentSender contract takes an i64 chat id; console channels are
    // `@username` names or numeric ids, so resolve the name first. Resolution
    // runs through the session's effective bot, so an owner-scoped post
    // resolves names with the bot the console is showing.
    let resolution_bot = effective_bot(&state, &session);
    let Some(chat_id) = channel_chat_id(&resolution_bot.bot, &ch).await else {
        tracing::warn!(channel = %ch, "post not sent: chat id could not be resolved");
        return Redirect::to(&state.url("/dashboard?post_error=resolve")).into_response();
    };
    match sender.send_text(chat_id, &f.text).await {
        Ok(sent) => {
            if sender.is_bot() {
                crate::metrics::bot_sent();
            }
            // Record the row with the sender it actually used -- the posts
            // page badge is a fact about the row, not the current binding.
            //
            // A sender with no tenant has no namespace to file this row in:
            // the old `unwrap_or(0)` attributed it to the operator. The
            // message has already been delivered, so report the scope failure.
            let Some(seller_id) = session.seller_namespace() else {
                return Redirect::to(&state.url("/dashboard?post_error=scope")).into_response();
            };
            let message_id = i32::try_from(sent.message_id).unwrap_or(0);
            if let Err(e) = state.db.log_post(
                seller_id,
                &ch,
                message_id,
                "text",
                Some(&f.text),
                sender_word,
            ) {
                tracing::warn!("failed to log post: {}", e);
            }
            Redirect::to(&state.url("/dashboard")).into_response()
        }
        Err(e) => {
            // Honest failure on the composer's own surface (spec §3): the
            // typed reason renders from fixed flash keys, and nothing is
            // logged as if the post had been sent.
            tracing::warn!(channel = %ch, sender = sender_word, "post send failed: {}", e);
            let (key, retry) = send_error_flash(&e);
            let mut target = format!("/dashboard?post_error={key}");
            if let Some(secs) = retry {
                target.push_str(&format!("&post_retry={secs}"));
            }
            Redirect::to(&state.url(&target)).into_response()
        }
    }
}

#[derive(Deserialize)]
pub(super) struct PlanForm {
    pub(super) name: String,
    pub(super) price: String,
    pub(super) asset: String,
    pub(super) duration_days: String,
    pub(super) channel: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn create_plan(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PlanForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    // A seller acts inside its own namespace; owner/admin act globally. A
    // session with no tenant (seller without a binding, or the buyer `user`
    // tier) is refused rather than defaulting to the platform namespace 0.
    let Some(seller_id) = session.seller_namespace() else {
        return forbidden_page(&state, "Plans", "Plans");
    };
    let name = f.name.trim().to_string();
    let price = f.price.trim().to_string();
    let asset = f.asset.trim().to_string();
    let days: i64 = match f.duration_days.trim().parse() {
        Ok(v) if v > 0 => v,
        _ => return Redirect::to(&state.url("/plans?error=invalid_duration")).into_response(),
    };
    if name.is_empty() || price.is_empty() || asset.is_empty() {
        return Redirect::to(&state.url("/plans?error=missing_fields")).into_response();
    }
    let channel = f.channel.filter(|s| !s.trim().is_empty());
    state
        .db
        .add_plan(seller_id, &name, &price, &asset, days, channel.as_deref());
    Redirect::to(&state.url("/plans?created=1")).into_response()
}

#[derive(Deserialize)]
pub(super) struct PlanDeleteForm {
    pub(super) name: Option<String>,
    pub(super) plan: Option<String>,
    pub(super) plan_name: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn delete_plan_by_name(
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
    // Plan names are unique per seller, not globally, so a delete BY NAME must
    // know whose namespace it is in. A seller deletes its own; an operator may
    // delete from any namespace (the console lists every plan, so the name was
    // resolved across all of them, and that authority is kept here). A session
    // with no tenant -- a seller with no binding, or the buyer `user` tier -- has
    // no namespace at all, and the old `else` arm silently treated it as the
    // operator and deleted the *platform* plan of that name.
    match session.tenant_scope() {
        Some(Some(sid)) => state.db.remove_plan(sid, &name),
        Some(None) => {
            let sid = state
                .db
                .list_plans(None)
                .into_iter()
                .find(|p| p.0 == name)
                .map(|p| p.6)
                .unwrap_or(0);
            state.db.remove_plan(sid, &name)
        }
        None => return forbidden_page(&state, "Plans", "Plans"),
    }
    Redirect::to(&state.url("/plans?deleted=1")).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn delete_plan_by_id(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if let Some((sid, price, asset, name, days, channel)) = state.db.get_plan_by_id(id) {
        // The owner comes from the row, never the request. Operators may delete
        // any namespace, sellers only their own, and a session with no tenant
        // has no namespace to delete in.
        match session.tenant_scope() {
            Some(None) => {}
            Some(Some(own)) if own == sid => {}
            Some(Some(_)) => return Redirect::to(&state.url("/plans")).into_response(),
            None => return forbidden_page(&state, "Plans", "Plans").into_response(),
        }
        let _ = (price, asset, days, channel);
        state.db.remove_plan(sid, &name);
        return Redirect::to(&state.url("/plans?deleted=1")).into_response();
    }
    Redirect::to(&state.url("/plans")).into_response()
}

#[derive(Deserialize)]
pub(super) struct ProductsQuery {
    pub(super) created: Option<String>,
    pub(super) activated: Option<String>,
    pub(super) deactivated: Option<String>,
    pub(super) error: Option<String>,
}

/// Products-page flash banners (EDIT_FLOW_SPEC E2/E3): fixed keys mapped to
/// house copy; an unknown key renders nothing rather than leaking a raw code.
fn products_banner(q: &ProductsQuery) -> String {
    if q.created.as_deref() == Some("1") {
        return flash_success("Product created.");
    }
    if q.activated.as_deref() == Some("1") {
        return flash_success("Product activated.");
    }
    if q.deactivated.as_deref() == Some("1") {
        return flash_success("Product deactivated.");
    }
    if let Some(e) = q.error.as_ref() {
        let msg = match e.as_str() {
            "missing_fields" => "Fill in name, price and asset.",
            "bad_kind" => "Choose a valid product kind.",
            "bad_stock" => "Stock must be a number, 0 or more.",
            _ => return String::new(),
        };
        return format!(r#"<div class="alert alert-error" role="alert">{msg}</div>"#);
    }
    String::new()
}

#[tracing::instrument(skip_all)]
pub(super) async fn products_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ProductsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.tenant_scope().is_none() {
        return forbidden_page(&state, "Products", "Products").into_response();
    }
    let banner = products_banner(&q);
    let body = products_body(&state, &session, &banner);
    Html(html_page(&state, Some(&session), "Products", &body)).into_response()
}

/// The products page body, parameterized on the banner slot so the
/// create-product DB-error path can re-render in place (EDIT_FLOW_SPEC E3 —
/// the typed form's context stays instead of a bare dead-end page).
fn products_body(state: &WebState, session: &Session, banner: &str) -> String {
    let seller_scope = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id
    } else {
        None
    };
    let bot_scope = bot_scope_sellers(state, session);
    let products = state
        .db
        .list_products(seller_scope)
        .into_iter()
        .filter(|p| in_bot_scope(&bot_scope, p.1))
        .collect::<Vec<_>>();
    let listing = if products.is_empty() {
        "<p class=\"empty\"><b>No products yet.</b> Create your first one below — buyers see it in /shop.</p>".to_string()
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
                // EDIT_FLOW_SPEC E2: an inactive product is no longer a dead
                // end — the row gets Activate (set_product_active(true)'s
                // first caller anywhere); an active row keeps Deactivate,
                // now confirmed because it is a sell-stop, not a delete.
                let row_action = if p.8 != 0 {
                    format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="name" value="{}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Deactivate {}" onclick="return confirm('Deactivate {}? Buyers can no longer buy it in /shop.')">Deactivate</button></form>"#,
                        state.url("/productdel"),
                        esc(&p.2),
                        esc(&p.2),
                        esc(&p.2)
                    )
                } else {
                    format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="name" value="{}"><button type="submit" class="btn btn-secondary btn-sm" aria-label="Activate {}">Activate</button></form>"#,
                        state.url("/product/activate"),
                        esc(&p.2),
                        esc(&p.2)
                    )
                };
                format!(
                    "<tr><td><b>{}</b></td><td class=\"num\">{}</td><td class=\"mono\">{}</td><td>{}</td><td class=\"num\">{}</td><td>{}</td><td class=\"actions\">{}</td></tr>",
                    esc(&p.2),
                    esc(&p.3),
                    esc(&p.4),
                    esc(&p.5),
                    stock,
                    active,
                    row_action
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!(
            "<div class=\"table-wrap\" tabindex=\"0\" role=\"region\" aria-label=\"Products\"><table aria-label=\"Products\"><thead><tr><th scope=\"col\">Name</th><th scope=\"col\" class=\"num\">Price</th><th scope=\"col\">Asset</th><th scope=\"col\">Kind</th><th scope=\"col\" class=\"num\">Stock</th><th scope=\"col\">Active</th><th scope=\"col\"><span class=\"sr-only\">Actions</span></th></tr></thead><tbody>{}</tbody></table></div>",
            rows
        )
    };
    format!(
        r#"<h1>Products <span class="sub">digital delivery</span></h1>
{banner}
{listing}
<article class="panel"><h3>Create product</h3><form method="post" action="{}">
<label for="name">Name</label><input id="name" name="name" required>
<label for="price">Price</label><input id="price" name="price" required>
<label for="asset">Asset</label><input id="asset" name="asset" required>
<label for="kind">Kind</label><select id="kind" name="kind"><option value="digital_link">Link</option><option value="digital_file">File</option><option value="physical">Physical item (shipped)</option></select>
<label for="delivery">Delivery (link, text or Telegram file_id, optional)</label><input id="delivery" name="delivery">
<label for="stock">Stock (blank = unlimited)</label><input id="stock" name="stock" type="number" min="0">
<button type="submit">Create product</button></form></article>"#,
        state.url("/product"),
        banner = banner,
        listing = listing
    )
}

#[derive(Deserialize)]
pub(super) struct ProductForm {
    pub(super) name: String,
    pub(super) price: String,
    pub(super) asset: String,
    pub(super) kind: String,
    pub(super) delivery: Option<String>,
    pub(super) stock: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn create_product(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ProductForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    // Same tenant rule as create_plan: a seller writes its own namespace, an
    // operator the platform one, and a session with no tenant is refused.
    let Some(seller_id) = session.seller_namespace() else {
        return forbidden_page(&state, "Products", "Products");
    };
    let name = f.name.trim().to_string();
    let price = f.price.trim().to_string();
    let asset = f.asset.trim().to_string();
    let kind = f.kind.trim().to_string();
    // Failures are flash keys with mapped copy (EDIT_FLOW_SPEC E3) — the
    // URL keys are fixed words, never free text; the page renders the
    // honest sentence.
    if name.is_empty() || price.is_empty() || asset.is_empty() {
        return Redirect::to(&state.url("/products?error=missing_fields")).into_response();
    }
    if !matches!(kind.as_str(), "digital_link" | "digital_file" | "physical") {
        return Redirect::to(&state.url("/products?error=bad_kind")).into_response();
    }
    let delivery = f
        .delivery
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let stock: Option<i64> = match f.stock.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => match s.parse() {
            Ok(v) if v >= 0 => Some(v),
            _ => return Redirect::to(&state.url("/products?error=bad_stock")).into_response(),
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
        // DB failure re-renders the products page in place with the reason
        // in a focused alert (identity.rs:241-244 idiom) instead of the
        // former bare dead-end page.
        tracing::warn!("product create failed: {}", e);
        let alert = focused_error_alert(&esc(&e.to_string()));
        let body = products_body(&state, &session, &alert);
        return Html(html_page(&state, Some(&session), "Products", &body)).into_response();
    }
    Redirect::to(&state.url("/products?created=1")).into_response()
}

#[derive(Deserialize)]
pub(super) struct ProductDeleteForm {
    pub(super) name: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn delete_product(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ProductDeleteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let Some(seller) = session.seller_namespace() else {
        return forbidden_page(&state, "Products", "Products");
    };
    let name = f.name.trim().to_string();
    if !name.is_empty() {
        state.db.set_product_active(seller, &name, false);
    }
    Redirect::to(&state.url("/products?deactivated=1")).into_response()
}

/// The one new route this pass adds (EDIT_FLOW_SPEC E2, `POST
/// /product/activate`): reactivation removes the one-way dead end where a
/// mis-clicked Deactivate made a product unsellable forever. Reuses the
/// existing [`Db::set_product_active`] `true` arm — its first caller — with
/// the same seller-namespace scope rule as `delete_product`: a seller flips
/// rows in their own seller_id namespace, an owner the global one. No DB
/// change.
#[tracing::instrument(skip_all)]
pub(super) async fn product_activate(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ProductDeleteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let Some(seller) = session.seller_namespace() else {
        return forbidden_page(&state, "Products", "Products");
    };
    let name = f.name.trim().to_string();
    if !name.is_empty() {
        state.db.set_product_active(seller, &name, true);
    }
    Redirect::to(&state.url("/products?activated=1")).into_response()
}
