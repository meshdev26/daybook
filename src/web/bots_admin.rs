use super::*;
// ---------- Bots (multi-bot console surface) ----------
//
// UX contract (SETUP_UX_POLISH_SPEC F3 + §2 cross-cutting rules): tokens are
// write-only and boot-loaded, so every mutation here states the restart in
// its banner and sets the `restart_required` flag the dashboard/settings
// banners read; banners and confirms name bots by their key, never the
// numeric id; errors re-render in place with the typed (non-secret) values.
// A bot's display name is card-title/rail-label sugar only (names are
// display-only, never identity).

#[derive(Deserialize)]
pub(super) struct BotSelectForm {
    pub(super) bot_id: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct BotSettingsForm {
    pub(super) bot_id: String,
    /// Console display name; absent/blank means "show the key instead".
    pub(super) name: Option<String>,
    pub(super) owner_user_id: String,
    pub(super) default_channel_id: Option<String>,
    pub(super) enabled: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct BotsQuery {
    pub(super) added: Option<String>,
    pub(super) disabled: Option<String>,
    pub(super) deleted: Option<String>,
    /// Fallback flag from the add POST (`?snippet=1`): the config file was
    /// not writable, so the page renders the copy-paste snippet panel
    /// instead of a dead-end raw download (F3.3).
    pub(super) snippet: Option<String>,
    /// Success flag from the card Save POST (`?saved=1`).
    pub(super) saved: Option<String>,
    /// Row id of the card to expand (`?saved=1&bot=N`, MANAGEMENT_DISCLOSURE_SPEC
    /// §2b): the page opens just that card's `<details>`. Inert on /bots/add.
    pub(super) bot: Option<String>,
}

/// Raw session token from cookie or Bearer header. `Session` itself carries
/// no token material, but `selected_bot_id` persists against the token hash,
/// so the select handler re-derives it here.
pub(super) fn session_token_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(tok) = crate::auth::get_cookie(headers) {
        return Some(tok);
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer ").map(str::to_string))
}

/// Path component of the Referer header for PRG returns. Only the path is
/// ever honored — scheme and host from the client are discarded — and the
/// path must live under the configured base path.
///
/// The path is always emitted with exactly one leading slash (review I1): a
/// double-slash Referer (`http:////evil.example/x`) splits to `//evil.example/x`,
/// which a browser would follow as a protocol-relative URL to another origin.
/// Control characters and backslashes (some agents read `\` as `/`) are
/// rejected outright, and an empty path falls back to the caller's default.
pub(super) fn referer_path(state: &WebState, headers: &axum::http::HeaderMap) -> Option<String> {
    let r = headers.get(axum::http::header::REFERER)?.to_str().ok()?;
    let rest = r.split_once("://")?.1;
    let path = rest.split_once('/')?.1;
    if path.len() > 512 {
        return None;
    }
    // Collapse every leading slash: the PRG target must stay same-origin.
    let path = format!("/{}", path.trim_start_matches('/'));
    if path.len() < 2 || path.contains('\\') || path.chars().any(|c| c.is_control()) {
        return None;
    }
    if state.base_path.is_empty() || path.starts_with(&state.base_path) {
        Some(path)
    } else {
        None
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn bot_select(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<BotSelectForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        // Sellers and admins are pinned to their resolved bot; a crafted
        // switch form must never widen their view.
        return forbidden_page(&state, "Bots", "Bot switching").into_response();
    }
    let Some(token) = session_token_from_headers(&headers) else {
        return Redirect::to(&state.url("/login")).into_response();
    };
    let selected = f
        .bot_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|id| state.db.get_bot(*id).is_some_and(|b| b.enabled));
    state
        .db
        .set_web_session_selected_bot(&sha256_hex(&token), selected);
    let back = referer_path(&state, &headers).unwrap_or_else(|| state.url("/dashboard"));
    Redirect::to(&back).into_response()
}

/// Banners ride the redirect URL as `disabled=`/`deleted=` params. Console
/// bots carry slug keys, which are URL-safe; a pathological config key falls
/// back to the numeric id rather than risking an invalid redirect URI. The
/// page resolves an id param back to the live key (F3.5: key-addressed copy).
fn bot_key_url_param(key: &str, id: i64) -> String {
    let url_safe = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if url_safe {
        key.to_string()
    } else {
        id.to_string()
    }
}

/// The display key for a `disabled=`/`deleted=` banner param: a live bot id
/// resolves to its key; a plain (URL-safe) key passes through; anything else
/// renders as the escaped param.
fn banner_bot_key(bots: &[crate::db::BotRecord], param: &str) -> String {
    if let Ok(id) = param.trim().parse::<i64>() {
        if let Some(b) = bots.iter().find(|b| b.id == id) {
            return b.key.clone();
        }
        return format!("#{}", id);
    }
    param.to_string()
}

/// Outcome banners for the Bots page (PRG results). `extra_error` carries an
/// in-place re-render error (bots_save failure) as a focused alert.
fn bots_page_banners(bots: &[crate::db::BotRecord], q: &BotsQuery) -> String {
    let mut banner = String::new();
    if let Some(k) = q.added.as_deref().filter(|s| !s.trim().is_empty()) {
        banner.push_str(&format!(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Bot <span class="mono">{}</span> added — the bot starts polling after the next restart.</div>"#,
            esc(k.trim())
        ));
    }
    if q.saved
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .is_some()
    {
        banner.push_str(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Bot settings saved — they apply on restart.</div>"#,
        );
    }
    if let Some(p) = q.disabled.as_deref().filter(|s| !s.trim().is_empty()) {
        banner.push_str(&format!(
            r#"<div class="alert alert-warn" role="status" aria-live="polite">Bot <span class="mono">{}</span> disabled — it stops responding after the next restart.</div>"#,
            esc(&banner_bot_key(bots, p))
        ));
    }
    if let Some(p) = q.deleted.as_deref().filter(|s| !s.trim().is_empty()) {
        banner.push_str(&format!(
            r#"<div class="alert alert-warn" role="status" aria-live="polite">Bot <span class="mono">{}</span> deleted from the console. Bots from your config file reappear on restart unless removed there.</div>"#,
            esc(&banner_bot_key(bots, p))
        ));
    }
    banner
}

/// The full Bots page body (shared by the GET handler and the bots_save
/// error re-render, so a failed save never dead-ends or wipes the list).
fn bots_page_html(state: &WebState, session: &Session, q: &BotsQuery, extra_error: &str) -> String {
    let bots = state.db.list_bots();
    let banner = format!("{}{}", extra_error, bots_page_banners(&bots, q));
    // ?saved=1&bot=N opens just the saved card's <details> (spec §2b); an
    // unparseable param opens nothing (collapsed by default).
    let open_bot = q.bot.as_deref().and_then(|s| s.trim().parse::<i64>().ok());
    let cards = bots
        .iter()
        .map(|b| bot_card_html(state, b, open_bot == Some(b.id)))
        .collect::<Vec<_>>()
        .join("");
    let add_link = format!(
        r#"<p><a class="btn btn-sm" href="{}">Add bot</a></p>"#,
        state.url("/bots/add")
    );
    let body = if bots.is_empty() {
        // P3-1: the reassurance must reflect a REAL primary bot. A
        // placeholder context (id 0) may still carry a dummy token, so the
        // guard is the id, not the token.
        let empty_copy = if state.bots.primary.id != 0 {
            // A real primary bot exists: lead with the reassuring fact,
            // keep jargon out (F3.7/R4).
            r#"<p class="empty"><b>No extra bots yet.</b> Your primary bot is already running from .env — most owners start with one. Add another bot here only when you need a second one.</p>"#
        } else {
            // No real primary: the reassurance would be a lie — keep the
            // config-file fallback as the trailing sentence instead.
            r#"<p class="empty"><b>No bots configured yet.</b> Add a bot here, or add an entry to <code>BOTS_CONFIG</code> / <code>BOTS_JSON</code> (or env) and restart.</p>"#
        };
        format!(
            r#"<h1>Bots <span class="sub">multi-bot management</span></h1>
{}<article class="panel"><h3>Configured bots</h3>{}</article>"#,
            add_link, empty_copy
        )
    } else {
        format!(
            r#"<h1>Bots <span class="sub">multi-bot management</span></h1>
{}{}"#,
            add_link, cards
        )
    };
    let body = if banner.is_empty() {
        body
    } else {
        format!("{}{}", banner, body)
    };
    html_page(state, Some(session), "Bots", &body)
}

/// One bot card: a collapsed `<details>` accordion (MANAGEMENT_DISCLOSURE_SPEC
/// §2) — the summary is the card header (key, badges, owner, hint), the body
/// holds the settings form (Save), the reversible/destructive action row
/// (Disable → Delete, R3 order), and the honest token story (F3.5/F3.6).
fn bot_card_html(state: &WebState, b: &crate::db::BotRecord, open: bool) -> String {
    // Token badge: derived at render time from the loaded BotContext matched
    // by id. Never persisted; only the last 4 characters are ever echoed.
    let token_badge = match state.bots.get(b.id) {
        Some(ctx) if !ctx.bot.token().trim().is_empty() => {
            let t = ctx.bot.token();
            let last4: String = t
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            format!(r#"<span class="badge">token …{}</span>"#, esc(&last4))
        }
        Some(_) => r#"<span class="badge badge-warning">token not saved yet</span>"#.to_string(),
        None => r#"<span class="badge">token in config file</span>"#.to_string(),
    };
    let enabled_badge = if b.enabled {
        r#"<span class="badge badge-success">enabled</span>"#.to_string()
    } else {
        r#"<span class="badge badge-warning">disabled</span>"#.to_string()
    };
    let owner_val = b.owner_user_id.to_string();
    let channel_val = b
        .default_channel_id
        .map(|c| c.to_string())
        .unwrap_or_default();
    let enabled_attr = if b.enabled { " checked" } else { "" };
    let open_attr = if open { " open" } else { "" };
    // Card title: the display name when set, else the key with a visible
    // hint so the "no display name" state is never silent.
    let display_name = bot_display_name(b);
    let name_hint = if b.name.is_empty() {
        r#" <span class="help">no display name set</span>"#
    } else {
        ""
    };
    // P3-2: a key interpolated into a confirm() JS string survives both the
    // HTML entity round-trip (esc() entities re-hydrate before JS compiles)
    // and JS quoting only when it is a strict slug; any other key degrades
    // to generic copy instead of reaching the JS context.
    let js_key = if !b.key.is_empty()
        && b.key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        b.key.clone()
    } else {
        "this bot".to_string()
    };
    format!(
        r#"<details class="panel bot-card" id="bot-{id}"{open_attr}>
  <summary>
    <h3>{title}{name_hint}</h3>
    {enabled_badge}
    {token_badge}
    <span class="bot-card-owner">owner {owner_val}</span>
    <span class="bot-card-hint">Expand to edit owner, default channel, or enabled state</span>
  </summary>
{body}"#,
        id = b.id,
        open_attr = open_attr,
        title = esc(display_name),
        name_hint = name_hint,
        enabled_badge = enabled_badge,
        token_badge = token_badge,
        owner_val = esc(&owner_val),
        body = bot_card_form(state, b, &owner_val, &channel_val, enabled_attr, &js_key),
    )
}

/// The expanded card body: the settings form (Save), the
/// reversible/destructive action row (Disable → Delete, R3 order), and the
/// honest token story (F3.5/F3.6). Move-only split from bot_card_html
/// (P3-1 body budget); output is byte-identical.
fn bot_card_form(
    state: &WebState,
    b: &crate::db::BotRecord,
    owner_val: &str,
    channel_val: &str,
    enabled_attr: &str,
    js_key: &str,
) -> String {
    // SETTINGS_PER_BOT_TOKEN_SPEC §6: the old "never shows or edits a token"
    // clause was already false (Settings edits TELEGRAM_BOT_TOKEN in legacy
    // mode, and writes per-bot tokens in multi-bot mode) — say what is true
    // for THIS page and point at the Settings section that edits the token.
    let settings_url = state.url("/settings?section=connection");
    let token_line = if multi_bot_mode(state) {
        format!(
            r#"Token: set it in Settings → <a href="{}">{}</a>."#,
            esc(&settings_url),
            esc(bot_display_name(b))
        )
    } else {
        format!(
            r#"Token: set it in Settings → <a href="{}">Connection</a>."#,
            esc(&settings_url)
        )
    };
    format!(
        r#"  <div class="bot-card-body">
    <form method="post" action="{bots_url}" class="bot-card-form">
      <input type="hidden" name="bot_id" value="{id}">
      <label for="bot-name-{id}">Display name</label>
      <input id="bot-name-{id}" type="text" name="name" value="{name_val}" maxlength="64">
      <span class="help">Shown in the console instead of the bot key. Leave blank to use the key.</span>
      <label for="bot-owner-{id}">Owner user ID</label>
      <input id="bot-owner-{id}" type="number" name="owner_user_id" value="{owner_val}" required>
      <span class="help">Your own Telegram account number (@userinfobot replies with it).</span>
      <label for="bot-channel-{id}">Default channel ID</label>
      <input id="bot-channel-{id}" type="number" name="default_channel_id" value="{channel_val}">
      <span class="help">Numeric chat ID of the channel (starts with -100…). Leave blank if this bot has no default channel.</span>
      <label class="check"><input type="checkbox" name="enabled"{enabled_attr}> Enabled</label>
      <button type="submit">Save</button>
    </form>
    <p class="help">Disabling is reversible — the bot stays listed, it just stops after the next restart. Deleting removes its settings from the console.</p>
    <div class="row-actions">
      <form method="post" action="{disable_url}" class="inline-form">
        <button type="submit" class="btn btn-sm btn-secondary" onclick="return confirm('Disable {js_key}? It stops responding after the next restart. You can enable it again any time.')">Disable</button>
      </form>
      <form method="post" action="{delete_url}" class="inline-form">
        <button type="submit" class="btn btn-sm btn-danger" onclick="return confirm('Delete {js_key}? Its settings are removed from the console. Bots that come from your config file will reappear on restart unless you remove them from the file.')">Delete</button>
      </form>
    </div>
    <p class="help">Tokens are stored only in your config file (.env / bots.toml) and are read once at startup — this page never shows a token. Owner and channel changes here apply on restart. {token_line}</p>
  </div>
</details>"#,
        id = b.id,
        name_val = esc(&b.name),
        bots_url = state.url("/bots"),
        owner_val = esc(owner_val),
        channel_val = esc(channel_val),
        enabled_attr = enabled_attr,
        js_key = js_key,
        disable_url = state.url(&format!("/bots/{}/disable", b.id)),
        delete_url = state.url(&format!("/bots/{}/delete", b.id)),
        token_line = token_line,
    )
}

/// "Bots" management page — global owner only. Token editing is out of
/// scope by design: tokens live in env/config and a restart, never here.
#[tracing::instrument(skip_all)]
pub(super) async fn bots_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<BotsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Bots", "Bots").into_response();
    }
    Html(bots_page_html(&state, &session, &q, "")).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn bots_save(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<BotSettingsForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Bots", "Bots").into_response();
    }
    // A malformed id or owner never redirects silently: re-render the Bots
    // page with a focused alert that says nothing was changed (F3.4/R5).
    let parsed = (
        f.bot_id.trim().parse::<i64>().ok(),
        f.owner_user_id.trim().parse::<i64>().ok(),
    );
    let (id, owner) = match parsed {
        (Some(id), Some(owner)) => (id, owner),
        _ => return bots_save_error_page(&state, &session),
    };
    let channel = f
        .default_channel_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<i64>().ok());
    // Display name is bounded here (the DB setter trims but caps nothing):
    // over the cap nothing at all is written, not even a co-submitted owner
    // change. Absent/blank falls back to the key via the empty string.
    let name = f.name.as_deref().map(str::trim).unwrap_or("");
    if name.chars().count() > 64 {
        return bots_save_error_page_msg(
            &state,
            &session,
            "Display name must be at most 64 characters. Nothing was changed.",
        );
    }
    // An unchecked checkbox is absent from the form body — None means off.
    let enabled = f.enabled.is_some();
    // Name first (it is the card's title field), then identity settings;
    // both setters refuse an unknown bot id, so a vanished row still lands
    // on the focused "nothing was changed" re-render below.
    if state.db.set_bot_name(id, name).is_err()
        || !state.db.update_bot_settings(id, owner, channel, enabled)
    {
        return bots_save_error_page(&state, &session);
    }
    // The DB `bots` rows are boot-loaded: the change applies on restart.
    state.db.set_config("restart_required", "1");
    tracing::info!(bot_id = id, "bot settings updated from console");
    // ?saved=1 carries the banner; &bot=N re-opens the saved card (spec §2b).
    Redirect::to(&state.url(&format!("/bots?saved=1&bot={}", id))).into_response()
}

/// The bots_save failure surface: the ordinary Bots page with a focused
/// error alert (no silent redirect, no dead-end 404 — R5).
fn bots_save_error_page(state: &WebState, session: &Session) -> Response {
    bots_save_error_page_msg(
        state,
        session,
        "That bot no longer exists or the owner ID isn't a number. Nothing was changed.",
    )
}

fn bots_save_error_page_msg(state: &WebState, session: &Session, msg: &str) -> Response {
    let alert = focused_error_alert(msg);
    Html(bots_page_html(
        state,
        session,
        &BotsQuery::default(),
        &alert,
    ))
    .into_response()
}

// Add wizard + token validation live in bots_add.rs (MANAGEMENT_DISCLOSURE_SPEC §1).

#[tracing::instrument(skip_all)]
pub(super) async fn bots_disable(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Bots", "Bots").into_response();
    }
    // Unknown id: honest no-op (P3-3) — no mutation, no restart flag, no
    // success banner; the Bots page re-renders with the warn banner.
    let Some(bot) = state.db.get_bot(id) else {
        let banner = r#"<div class="alert alert-warn" role="status" aria-live="polite">That bot no longer exists — nothing was changed.</div>"#;
        return Html(bots_page_html(
            &state,
            &session,
            &BotsQuery::default(),
            banner,
        ))
        .into_response();
    };
    let key = bot.key;
    state.db.set_bot_enabled(id, false);
    // Boot-loaded DB row: the bot stops polling only after a restart (R1).
    state.db.set_config("restart_required", "1");
    tracing::info!(bot_id = id, "bot disabled from console");
    let param = bot_key_url_param(&key, id);
    let loc = state.url(&format!("/bots?disabled={}", param));
    let body = format!(
        r#"<div class="alert alert-warn" role="status" aria-live="polite">Bot <span class="mono">{}</span> disabled — it stops responding after the next restart.</div>"#,
        esc(&key)
    );
    (
        StatusCode::SEE_OTHER,
        AppendHeaders([(header::LOCATION, loc)]),
        Html(html_page(&state, Some(&session), "Bots", &body)),
    )
        .into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn bots_delete(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Bots", "Bots").into_response();
    }
    // Unknown id: honest no-op (P3-3) — nothing deleted, no restart flag,
    // no success banner; the Bots page re-renders with the warn banner.
    let Some(bot) = state.db.get_bot(id) else {
        let banner = r#"<div class="alert alert-warn" role="status" aria-live="polite">That bot no longer exists — nothing was changed.</div>"#;
        return Html(bots_page_html(
            &state,
            &session,
            &BotsQuery::default(),
            banner,
        ))
        .into_response();
    };
    let key = bot.key;
    // delete_bot's result is honored, not discarded: a row that vanished
    // between the read and the delete is the same honest no-op.
    if !state.db.delete_bot(id) {
        let banner = r#"<div class="alert alert-warn" role="status" aria-live="polite">That bot no longer exists — nothing was changed.</div>"#;
        return Html(bots_page_html(
            &state,
            &session,
            &BotsQuery::default(),
            banner,
        ))
        .into_response();
    }
    // Boot-loaded DB row: config-driven bots reappear on restart until
    // removed from the file — the banner says so (R1/F3.5).
    state.db.set_config("restart_required", "1");
    tracing::info!(bot_id = id, "bot deleted from console");
    let param = bot_key_url_param(&key, id);
    let loc = state.url(&format!("/bots?deleted={}", param));
    let body = format!(
        r#"<div class="alert alert-warn" role="status" aria-live="polite">Bot <span class="mono">{}</span> deleted from the console. Bots from your config file reappear on restart unless removed there.</div>"#,
        esc(&key)
    );
    (
        StatusCode::SEE_OTHER,
        AppendHeaders([(header::LOCATION, loc)]),
        Html(html_page(&state, Some(&session), "Bots", &body)),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The banner param only rides URL-safe keys; anything else falls back
    /// to the numeric id so the redirect URI can never be malformed.
    #[test]
    fn bot_key_url_param_guards_url_safety() {
        assert_eq!(bot_key_url_param("secondary", 7), "secondary");
        assert_eq!(bot_key_url_param("bad key/with?chars", 7), "7");
        assert_eq!(bot_key_url_param("", 7), "7");
    }
}
