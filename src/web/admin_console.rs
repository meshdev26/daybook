use super::*;
// Subs grant / broadcast / crypto / admins / sellers live here; the bots
// management cluster (pages, add wizard, card actions) lives in the sibling
// `bots_admin` module (see mod.rs for the glob import that reunites them).

#[derive(Deserialize)]
pub(super) struct SubsGrantForm {
    pub(super) user_id: String,
    pub(super) days: String,
    pub(super) channel: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn subs_grant(
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

/// One segment value from the GET form's repeated `segment` params (EDIT_FLOW_SPEC
/// E17: the preset select and the custom `<details>` input share the name on
/// purpose — the extraction is `Query<Vec<(String, String)>>` because
/// serde_urlencoded rejects repeated keys into any struct field). DOM order
/// makes the select first and the custom text input second, so an explicit
/// select choice (anything but the `custom` sentinel) wins; the sentinel
/// defers to the typed expression; a bare `custom` with nothing typed stays
/// `custom` (matches nobody — the honest zero audience the preview count
/// then reports).
fn resolve_segment(values: &[String]) -> String {
    let select = values.first().map(|s| s.trim()).unwrap_or("");
    let custom = values
        .iter()
        .skip(1)
        .rev()
        .map(|s| s.trim())
        .find(|s| !s.is_empty())
        .unwrap_or("");
    if select == "custom" {
        if custom.is_empty() {
            "custom".to_string()
        } else {
            custom.to_string()
        }
    } else if !select.is_empty() {
        select.to_string()
    } else if !custom.is_empty() {
        custom.to_string()
    } else {
        "all".to_string()
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn broadcast_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(pairs): Query<Vec<(String, String)>>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let sent = pairs
        .iter()
        .find(|(k, _)| k == "sent")
        .map(|(_, v)| v.clone());
    let segments: Vec<String> = pairs
        .iter()
        .filter(|(k, _)| k == "segment")
        .map(|(_, v)| v.clone())
        .collect();
    // Outcome banner from the ?sent={n} redirect (PRG). The count is what the
    // send actually delivered; 0 is a failure, not a success — nothing went
    // out.
    let banner = match sent.as_deref().and_then(|s| s.parse::<usize>().ok()) {
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
    let segment = resolve_segment(&segments);
    // Recipient scope (review P1-1, fail closed): owner → global (None);
    // a non-owner carries their seller_id; a non-owner with NO seller scope
    // must see and reach zero recipients — passing None from here would
    // mean "all platform users".
    let seller_scope = if session.role == "owner" {
        None
    } else {
        session.seller_id
    };
    let no_seller_scope = session.role != "owner" && seller_scope.is_none();
    let users = if no_seller_scope {
        Vec::new()
    } else {
        crate::services::broadcast::filter_broadcast_users(&state.db, seller_scope, &segment)
    };
    let audience = users.len();
    let audience_note = if no_seller_scope {
        r#"<p class="help">This account has no seller scope — nothing to send.</p>"#.to_string()
    } else if audience == 0 {
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
    let segment_fields = broadcast_segment_fields(&segment);
    let body = format!(
        r#"{banner}<h1>Broadcast <span class="sub">send to customers</span></h1>
<article class="panel"><h3>Compose</h3>
<form method="get" action="{}" class="inline-form">
{segment_fields}
<button type="submit">Preview count</button>
</form>
<form method="post" action="{}" onsubmit="var b=this.querySelector('button[type=submit]');if(b){{b.disabled=true;b.setAttribute('aria-busy','true');}}">
<input type="hidden" name="segment" value="{segment}">
<label>Message <textarea name="text" rows="6" required></textarea></label>
<button type="submit" {confirm}>Send broadcast</button>
</form>
{audience_note}
<p class="help">Sellers broadcast to their own customers; owners broadcast to all users.</p>
</article>"#,
        state.url("/broadcast"),
        state.url("/broadcast/send"),
        banner = banner.as_deref().unwrap_or(""),
        segment_fields = segment_fields,
        segment = esc(&segment),
        confirm = confirm,
        audience_note = audience_note,
    );
    Html(html_page(&state, Some(&session), "Broadcast", &body)).into_response()
}

/// The GET preview form's segment controls (EDIT_FLOW_SPEC E17): one preset
/// select carrying the `custom` sentinel as its last option, plus the ONE
/// custom text input inside a native `<details>` (closed unless the segment
/// is non-preset — server-rendered, no JS). The shared `name="segment"` is
/// deliberate: `resolve_segment` gives the explicit select choice
/// precedence, so both submitting can no longer produce an unpredictable
/// audience.
fn broadcast_segment_fields(segment: &str) -> String {
    let presets = ["all", "product:", "tag:", "active:30", "minspend:0"];
    let preset_selected = presets
        .iter()
        .any(|val| segment == *val || (val.ends_with(':') && segment.starts_with(val)));
    let mut segment_options = presets
        .iter()
        .map(|s| {
            // Render a few presets; selected if segment starts with s prefix
            let val = *s;
            let sel = if segment == val || (val.ends_with(':') && segment.starts_with(val)) {
                " selected"
            } else {
                ""
            };
            format!(r#"<option value="{}"{}>{}</option>"#, val, sel, val)
        })
        .collect::<String>();
    if preset_selected {
        segment_options.push_str(r#"<option value="custom">Custom segment…</option>"#);
    } else {
        segment_options.push_str(r#"<option value="custom" selected>Custom segment…</option>"#);
    }
    let custom_input = if preset_selected {
        String::new()
    } else {
        format!(r#" value="{}""#, esc(segment))
    };
    let details_open = if preset_selected { "" } else { " open" };
    format!(
        r#"<label>Segment <select name="segment">{segment_options}</select></label>
<details{details_open}><summary>Custom segment</summary>
<label>Custom segment expression <input type="text" name="segment"{custom_input} placeholder="product:1 / tag:vip / active:7 / minspend:10"></label>
</details>"#,
        segment_options = segment_options,
        details_open = details_open,
        custom_input = custom_input,
    )
}

#[derive(Deserialize)]
pub(super) struct BroadcastForm {
    pub(super) text: String,
    pub(super) segment: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn broadcast_send(
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
    let segment = f.segment.clone().unwrap_or_else(|| "all".to_string());
    // Recipient scope (review P1-1, fail closed): owner → global (None);
    // a non-owner carries their seller_id; a non-owner with NO seller scope
    // delivers to nobody — None here would mean "all platform users".
    let seller_scope = if session.role == "owner" {
        None
    } else {
        session.seller_id
    };
    let users = if session.role != "owner" && seller_scope.is_none() {
        Vec::new()
    } else {
        crate::services::broadcast::filter_broadcast_users(&state.db, seller_scope, &segment)
    };
    let sent =
        crate::broadcast_to_users(&effective_bot(&state, &session).bot, &users, &f.text).await;
    Redirect::to(&state.url(&format!("/broadcast?sent={}", sent))).into_response()
}

// ---------- Crypto ----------

/// The crypto page's save flash (EDIT_FLOW_SPEC E18) and the "Show me"
/// result slot (E16): both render through one banner parameter.
#[derive(Deserialize)]
pub(super) struct CryptoQuery {
    pub(super) saved: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn crypto_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<CryptoQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Crypto", "Crypto").into_response();
    }
    let banner = if q.saved.as_deref() == Some("1") {
        r#"<div class="alert alert-success" role="status" aria-live="polite">Token saved.</div>"#
            .to_string()
    } else {
        String::new()
    };
    let body = crypto_body(&state, &banner);
    Html(html_page(&state, Some(&session), "Crypto", &body)).into_response()
}

/// The crypto page body, parameterized on the banner slot so `crypto_me` can
/// render its check result in place (keys_setup.rs:149-155 direct-Html
/// precedent — no PRG, no free text in URLs).
fn crypto_body(state: &WebState, banner: &str) -> String {
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
    format!(
        r#"<h1>Crypto <span class="sub">Crypto Pay</span></h1>
{banner}
<article class="panel"><h3>Token {}</h3>
<form method="post" action="{}" onsubmit="var b=this.querySelector('button[type=submit]');if(b){{b.disabled=true;b.setAttribute('aria-busy','true');}}">
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
        banner = banner,
        toggle = SECRET_TOGGLE,
    )
}

#[derive(Deserialize)]
pub(super) struct CryptoSetupForm {
    pub(super) token: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn crypto_setup(
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
        Redirect::to(&state.url("/crypto?saved=1")).into_response()
    } else {
        // Blank submit changed nothing — no success flash (honest no-op).
        Redirect::to(&state.url("/crypto")).into_response()
    }
}

/// Char-boundary-safe truncation for the getMe snippet shown in the banner.
fn truncate_chars(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// The error NAME only (EDIT_FLOW_SPEC E16): Crypto Pay errors are
/// `HTTP <status> - <body>` or an API error word — the response body never
/// reaches the banner.
fn crypto_error_name(e: &str) -> String {
    let head = e.split(" - ").next().unwrap_or(e);
    truncate_chars(head, 120)
}

#[tracing::instrument(skip_all)]
pub(super) async fn crypto_me(
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
    let banner = match cp.get_me().await {
        Ok(v) => {
            // EDIT_FLOW_SPEC E16: the computed result actually displays now —
            // truncated to 200 chars and escaped, rendered here only.
            let me = truncate_chars(&format!("Me: {}", v), 200);
            format!(
                r#"<div class="alert alert-success" role="status" aria-live="polite"><p><strong>Crypto Pay reachable.</strong></p><p class="mono">{}</p></div>"#,
                esc(&me)
            )
        }
        Err(e) => {
            tracing::warn!("crypto get_me failed: {}", e);
            format!(
                r#"<div class="alert alert-error" role="alert">Crypto Pay check failed: {}</div>"#,
                esc(&crypto_error_name(&e))
            )
        }
    };
    let body = crypto_body(&state, &banner);
    Html(html_page(&state, Some(&session), "Crypto", &body)).into_response()
}

// ---------- Admins ----------

/// Admins-page flash keys (EDIT_FLOW_SPEC E14): added/removed outcomes and
/// the uid parse-fail error that used to drop silently.
#[derive(Deserialize)]
pub(super) struct AdminsQuery {
    pub(super) added: Option<String>,
    pub(super) removed: Option<String>,
    pub(super) error: Option<String>,
}

fn admins_banner(q: &AdminsQuery) -> String {
    if q.added.as_deref() == Some("1") {
        return r#"<div class="alert alert-success" role="status" aria-live="polite">Admin added.</div>"#.to_string();
    }
    if q.removed.as_deref() == Some("1") {
        return r#"<div class="alert alert-success" role="status" aria-live="polite">Admin removed.</div>"#.to_string();
    }
    if q.error.as_deref() == Some("uid") {
        return r#"<div class="alert alert-error" role="alert">User ID must be a number.</div>"#
            .to_string();
    }
    String::new()
}

#[tracing::instrument(skip_all)]
pub(super) async fn admins_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<AdminsQuery>,
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
            // Username comes from the add-admin form's free-text field —
            // escaped on render like every other user-supplied cell.
            let name = esc(uname.as_deref().unwrap_or("—"));
            format!(
                r#"<tr><td class="mono">{}</td><td>{}</td><td>
<form method="post" action="{}" class="inline-form"><input type="hidden" name="user_id" value="{}"><button type="submit" class="btn btn-danger btn-sm" onclick="return confirm('Remove admin {}? They lose console admin powers.')">Remove</button></form>
</td></tr>"#,
                uid,
                name,
                state.url("/admins/remove"),
                uid,
                name
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let table = format!(
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Admins"><table aria-label="Admins"><thead><tr><th scope="col">User ID</th><th scope="col">Username</th><th scope="col">Actions</th></tr></thead><tbody>{}</tbody></table></div>"#,
        if rows.is_empty() {
            r#"<tr><td colspan="3" class="empty"><b>No admins.</b></td></tr>"#.to_string()
        } else {
            rows
        }
    );
    let banner = admins_banner(&q);
    let body = format!(
        r#"<h1>Admins <span class="sub">admin management</span></h1>
{banner}
<article class="panel"><h3>Add admin</h3>
<form method="post" action="{}" class="inline-form">
<label for="user_id">User ID</label><input id="user_id" type="number" name="user_id" placeholder="user id" required>
<label for="username">Username</label><input id="username" type="text" name="username" placeholder="username">
<button type="submit">Add</button>
</form></article>
{table}"#,
        state.url("/admins/add"),
        banner = banner,
        table = table
    );
    Html(html_page(&state, Some(&session), "Admins", &body)).into_response()
}

#[derive(Deserialize)]
pub(super) struct AdminForm {
    pub(super) user_id: String,
    pub(super) username: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct AdminRemoveForm {
    pub(super) user_id: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn admins_add(
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
    match f.user_id.trim().parse::<i64>() {
        Ok(uid) => {
            state.db.add_admin(uid, f.username.as_deref());
            Redirect::to(&state.url("/admins?added=1")).into_response()
        }
        Err(_) => Redirect::to(&state.url("/admins?error=uid")).into_response(),
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn admins_remove(
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
    match f.user_id.trim().parse::<i64>() {
        Ok(uid) => {
            state.db.remove_admin(uid);
            Redirect::to(&state.url("/admins?removed=1")).into_response()
        }
        Err(_) => Redirect::to(&state.url("/admins?error=uid")).into_response(),
    }
}

// ---------- Sellers ----------

/// Sellers-page flash keys (EDIT_FLOW_SPEC E15): the save outcome and the
/// uid parse-fail error that used to drop silently.
#[derive(Deserialize)]
pub(super) struct SellersQuery {
    pub(super) saved: Option<String>,
    pub(super) error: Option<String>,
}

fn sellers_banner(q: &SellersQuery) -> String {
    if q.saved.as_deref() == Some("1") {
        return r#"<div class="alert alert-success" role="status" aria-live="polite">Seller saved.</div>"#.to_string();
    }
    if q.error.as_deref() == Some("uid") {
        return r#"<div class="alert alert-error" role="alert">Seller id must be a number.</div>"#
            .to_string();
    }
    String::new()
}

#[tracing::instrument(skip_all)]
pub(super) async fn sellers_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SellersQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Sellers", "Sellers").into_response();
    }
    let sellers = state.db.list_sellers();
    let titles = channel_titles(&state);
    let rows = sellers
        .iter()
        .map(|sid| {
            let ch = state.db.seller_channel(*sid).unwrap_or_default();
            format!(
                r#"<tr><td class="mono">{}</td><td class="mono">{}</td></tr>"#,
                sid,
                channel_label(
                    titles.get(&ch.to_ascii_lowercase()).map(String::as_str),
                    &ch
                )
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let table = format!(
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Sellers"><table aria-label="Sellers"><thead><tr><th scope="col">Seller ID</th><th scope="col">Channel</th></tr></thead><tbody>{}</tbody></table></div>"#,
        if rows.is_empty() {
            r#"<tr><td colspan="2" class="empty"><b>No sellers.</b></td></tr>"#.to_string()
        } else {
            rows
        }
    );
    // EDIT_FLOW_SPEC E15: same verb ("Save") for both forms, one help line
    // each so the ownership of the channel field is unambiguous; "Upsert"
    // (developer vocabulary) is gone.
    let banner = sellers_banner(&q);
    let body = format!(
        r#"<h1>Sellers <span class="sub">seller management</span></h1>
{banner}
<article class="panel"><h3>Add or update seller</h3>
<form method="post" action="{}" class="inline-form">
<label for="upsert-user-id">Seller ID</label><input id="upsert-user-id" type="number" name="user_id" placeholder="user id" required>
<label for="upsert-username">Username</label><input id="upsert-username" type="text" name="username" placeholder="username">
<label for="upsert-channel">Channel</label><input id="upsert-channel" type="text" name="channel" placeholder="@channel">
<button type="submit">Save</button>
</form>
<p class="help">Creates the seller if the id is new, otherwise updates name and channel.</p>
</article>
<article class="panel"><h3>Set seller channel</h3>
<form method="post" action="{}" class="inline-form">
<label for="setchan-user-id">Seller ID</label><input id="setchan-user-id" type="number" name="user_id" placeholder="seller id" required>
<label for="setchan-channel">Channel</label><input id="setchan-channel" type="text" name="channel" placeholder="@channel" required>
<button type="submit">Save</button>
</form>
<p class="help">Updates only the channel.</p>
</article>
{table}"#,
        state.url("/sellers/upsert"),
        state.url("/sellers/channel"),
        banner = banner,
        table = table
    );
    Html(html_page(&state, Some(&session), "Sellers", &body)).into_response()
}

#[derive(Deserialize)]
pub(super) struct SellerUpsertForm {
    pub(super) user_id: String,
    pub(super) username: Option<String>,
    pub(super) channel: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SellerChannelForm {
    pub(super) user_id: String,
    pub(super) channel: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn sellers_upsert(
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
    match f.user_id.trim().parse::<i64>() {
        Ok(uid) => {
            state
                .db
                .upsert_seller(uid, f.username.as_deref(), f.channel.as_deref());
            Redirect::to(&state.url("/sellers?saved=1")).into_response()
        }
        Err(_) => Redirect::to(&state.url("/sellers?error=uid")).into_response(),
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn sellers_channel(
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
    match f.user_id.trim().parse::<i64>() {
        Ok(uid) => {
            state.db.set_seller_channel(uid, &f.channel);
            Redirect::to(&state.url("/sellers?saved=1")).into_response()
        }
        Err(_) => Redirect::to(&state.url("/sellers?error=uid")).into_response(),
    }
}
