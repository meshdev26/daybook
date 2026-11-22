use super::*;
#[tracing::instrument(skip_all)]
pub(super) async fn channels_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ChannelsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let banners = channels_banners(&q, &state, &session);
    let success_banner: Option<String> = if banners.is_empty() {
        None
    } else {
        Some(banners.join("\n"))
    };
    Html(html_page(
        &state,
        Some(&session),
        "Channels",
        &channels_body(&state, &session, None, success_banner.as_deref(), Some(&q)),
    ))
    .into_response()
}

/// One console banner line (MANAGEMENT_DISCLOSURE_SPEC §5e): fixed wrapper,
/// caller-owned copy. info/success/warn announce politely; error is
/// assertive — shared by the list, the add page, and the manage detail.
pub(super) fn channels_banner(kind: &str, msg: &str) -> String {
    let (class, attrs) = if kind == "error" {
        ("alert-error", "role=\"alert\"")
    } else {
        (kind, "role=\"status\" aria-live=\"polite\"")
    };
    format!("<div class=\"alert {class}\" {attrs}>{msg}</div>")
}

/// Fixed copy for the channels pages' `?error=` keys (kept out of
/// `channels_banners` for the 100-line body budget): fixed keys render fixed
/// copy; unknown values stay escaped — a query string never injects markup.
/// Refusals link the Identity gate.
pub(super) fn channels_error_copy(e: &str, state: &WebState, session: &Session) -> String {
    match e {
        "empty" => "Type a channel name first.".to_string(),
        // §3.4 fixed refusal keys — shape-branched add validation (§3.4.2/3).
        "private-id" => {
            "Private channels are addressed by their numeric -100 ID only — paste the ID, or switch Visibility to Public if the channel has an @handle."
                .to_string()
        }
        "public-shape" => {
            "Public channels are addressed by an @handle or a t.me link — or switch Visibility to Private and paste the channel's numeric -100 ID."
                .to_string()
        }
        "posting" => {
            if Role::from_role_str(&session.role).has_level(Role::Owner) {
                format!(
                    "Posting as your personal account isn't enabled yet. Enable it on the <a href=\"{}\">Identity page</a> first.",
                    state.url("/identity")
                )
            } else {
                "Posting as your personal account isn't enabled yet — the console owner enables it on the Identity page first.".to_string()
            }
        }
        "locked" => "Your identity is locked — unlock it in the banner above.".to_string(),
        "identity" => {
            if Role::from_role_str(&session.role).has_level(Role::Owner) {
                format!(
                    "The personal identity isn't connected — finish connecting it on the <a href=\"{}\">Identity page</a> first.",
                    state.url("/identity")
                )
            } else {
                "The personal identity isn't connected — the console owner connects it on the Identity page first.".to_string()
            }
        }
        other => esc(other),
    }
}

/// The channels pages' PRG outcome banners, one per redirect flag — every
/// line wraps through `channels_banner`. Called by BOTH the list and the
/// manage detail (spec §6): old `/channels?flag=1` bookmarks keep working.
pub(super) fn channels_banners(
    q: &ChannelsQuery,
    state: &WebState,
    session: &Session,
) -> Vec<String> {
    // The same query-param idiom /subs and /finance use. ?notitle=1 means
    // the add succeeded but the Telegram title could not be resolved.
    let mut banners: Vec<String> = Vec::new();
    if q.added.as_deref() == Some("1") {
        // One outcome, one banner (CHANNEL_ADD_REDESIGN_SPEC §4.4): when the
        // add redirect anchors the new row (?channel=), the in-panel callout
        // owns the message — no top banner. Legacy redirects without
        // `channel` keep today's banners byte-identical.
        let anchored = q
            .channel
            .as_deref()
            .map(str::trim)
            .is_some_and(|c| !c.is_empty());
        if !anchored {
            if q.notitle.as_deref() == Some("1") {
                // Honest add (CHANNELS_TITLE_UX_SPEC §3): resolution failed, say so.
                let tail = if Role::from_role_str(&session.role).has_level(Role::Owner) {
                    " Open Manage to name it."
                } else {
                    ""
                };
                banners.push(channels_banner(
                    "info",
                    &format!("Channel added — I couldn't fetch its name from Telegram (the bot may not be an admin there yet).{tail}"),
                ));
            } else {
                banners.push(channels_banner("success", "Channel added."));
            }
        }
    }
    if q.default.as_deref() == Some("1") {
        banners.push(channels_banner("success", "Default channel set."));
    }
    if q.removed.as_deref() == Some("1") {
        banners.push(channels_banner("success", "Channel removed."));
    }
    if q.renamed.as_deref() == Some("1") && q.same.as_deref() == Some("1") {
        let msg = "Nothing changed — that's still the channel's address. Use Save name in the Identity section to change its label.";
        banners.push(channels_banner("info", msg));
    } else if q.renamed.as_deref() == Some("1") {
        let name = q.channel.as_deref().unwrap_or("the channel");
        let msg = format!("Channel renamed to {}.", esc(name));
        banners.push(channels_banner("success", &msg));
    }
    // Sender-binding flashes (IDENTITY_SENDER_UX_SPEC §3): the channel name
    // rides the redirect (an identifier, not PII — §7.5), escaped again here.
    if q.bound_user.as_deref() == Some("1") {
        let name = q.channel.as_deref().unwrap_or("the channel");
        let msg = format!(
            "Posts to {} will be sent as your personal account.",
            esc(name)
        );
        banners.push(channels_banner("success", &msg));
    }
    if q.bound_bot.as_deref() == Some("1") {
        let name = q.channel.as_deref().unwrap_or("the channel");
        banners.push(channels_banner(
            "success",
            &format!("Posts to {} are sent by the bot again.", esc(name)),
        ));
    }
    // §5a: the Edit-name PRG flag — the label changed, the identifier did not.
    if q.named.as_deref() == Some("1") {
        banners.push(channels_banner("success", "Channel name saved."));
    }
    if let Some(e) = q.error.as_ref() {
        let msg = channels_error_copy(e, state, session);
        banners.push(channels_banner("error", &msg));
    }
    banners
}

/// Query shared by the channels GET page: the success flags its mutation
/// handlers redirect with — `notitle` marks the add that succeeded without a
/// Telegram title, `same` marks the NOCASE self-rename guard (spec §5),
/// `named` marks the Edit-name save (CHANNEL_ADD_REDESIGN_SPEC §5a) — plus
/// the sender-binding flashes (IDENTITY_SENDER_UX_SPEC §8.4) and `channel`,
/// the binding's/add's channel name (§7.5; with `added=1` it anchors the
/// post-add reveal, §4).
#[derive(Deserialize)]
pub(super) struct ChannelsQuery {
    pub(super) added: Option<String>,
    pub(super) notitle: Option<String>,
    pub(super) default: Option<String>,
    pub(super) removed: Option<String>,
    pub(super) renamed: Option<String>,
    pub(super) same: Option<String>,
    pub(super) named: Option<String>,
    pub(super) error: Option<String>,
    #[serde(rename = "bound-user")]
    pub(super) bound_user: Option<String>,
    #[serde(rename = "bound-bot")]
    pub(super) bound_bot: Option<String>,
    pub(super) channel: Option<String>,
}

/// The channels manager body (PLAN v1.3.x): the owner manages the global
/// set; a seller sees only their own bot-managed channel. `alert` renders an
/// inline error (no redirect); `success` carries the pre-rendered PRG banner
/// HTML (?added=1 / ?default=1 / ?removed=1). `q` (absent on the inline-error
/// path) drives the post-add reveal: the highlighted row and the in-panel
/// callout (CHANNEL_ADD_REDESIGN_SPEC §4).
pub(super) fn channels_body(
    state: &WebState,
    session: &Session,
    alert: Option<&str>,
    success: Option<&str>,
    q: Option<&ChannelsQuery>,
) -> String {
    let alert = alert
        .map(|a| channels_banner("error", a))
        .unwrap_or_default();
    let success = success.unwrap_or_default().to_string();
    if Role::from_role_str(&session.role) == Role::Seller {
        return render_seller_channels(state, session, &alert, &success, q);
    }
    render_global_channels(state, session, &alert, &success, q)
}

/// The owner/admin master list (CHANNELS_MASTER_DETAIL_SPEC §4): a page-head
/// with the one `Add channel` primary affordance (owner only — admins have no
/// add permission and get no button, the same honesty as the old missing
/// form) and read-first rows. Every edit control moved to
/// `GET /channels/manage`.
fn render_global_channels(
    state: &WebState,
    session: &Session,
    alert: &String,
    success: &String,
    q: Option<&ChannelsQuery>,
) -> String {
    let is_owner = Role::from_role_str(&session.role).has_level(Role::Owner);
    let scope_bot = if is_owner {
        session.selected_bot_id
    } else {
        None
    };
    let focus = added_focus(q);
    let fetched = state.db.list_channels(None);
    // An empty SCOPED list with rows present is a scoping artifact, not an
    // empty set (MANAGEMENT_DISCLOSURE_SPEC §3) — count the unscoped rows.
    let total_global = fetched.iter().filter(|row| row.seller_id == 0).count();
    let rows = fetched
        .into_iter()
        .filter(|row| row.seller_id == 0)
        .filter(|row| scope_bot.is_none_or(|bid| state.bots.for_channel(&row.channel).id == bid))
        .map(|row| render_global_channel_row(state, is_owner, row, focus))
        .collect::<Vec<_>>()
        .join("");
    let list = if rows.is_empty() && scope_bot.is_some() && total_global > 0 {
        "<li class=\"empty\"><b>No channels resolve to this bot yet.</b> The bot may not be connected (no token) or these channels belong to another bot — switch the Bot selector above to All bots.</li>".to_string()
    } else if rows.is_empty() {
        "<li class=\"empty\"><b>No global channels yet.</b> Add one — it becomes a posting target for plans and posts.</li>".to_string()
    } else {
        rows
    };
    // §4.1: one primary affordance in the page head — `a.btn` with no
    // modifier IS the primary button (house CSS, first `.page-head` consumer).
    let add_button = if is_owner {
        format!(
            r#"<div class="actions"><a class="btn" href="{}">Add channel</a></div>"#,
            state.url("/channels/add")
        )
    } else {
        String::new()
    };
    // §4.3: the callout sits immediately after the <ul>, adjacent to the
    // highlighted last row.
    let callout = q
        .map(|q| channels_added_callout(state, session, q))
        .unwrap_or_default();
    format!(
        r#"{alert}{success}<div class="page-head"><h1>Channels <span class="sub">the global posting targets</span></h1>{add_button}</div>
<article class="panel"><h3>Global channels</h3><ul>{list}</ul>
{callout}
<p class="help">Sellers manage their own channel via /setchannel in the bot.</p>
</article>"#,
        alert = alert,
        success = success,
        add_button = add_button,
        list = list,
        callout = callout,
    )
}

/// Re-render the channels LIST page with an inline error (no redirect: the
/// failed action's message belongs on the page it failed from). The remaining
/// inline paths are scripted-only (spec §6) — every UI-reachable failure now
/// PRGs to the add page or the manage detail.
pub(super) fn channels_error(state: &WebState, session: &Session, msg: &str) -> Response {
    Html(html_page(
        state,
        Some(session),
        "Channels",
        &channels_body(state, session, Some(msg), None, None),
    ))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct ChannelForm {
    pub(super) channel: String,
    /// §3.1: the visibility radio (`public` | `private`). Absent (or
    /// unrecognized) keeps the legacy omnibus path (§3.4.1).
    pub(super) visibility: Option<String>,
    /// §3.1: the user-typed label — wins over getChat (§3.4.5).
    pub(super) title: Option<String>,
}

/// §3.4: visibility validation + best-effort resolution for `channels_add` —
/// the radio is the truth, the server is the enforcement. Returns the
/// canonical identifier to store plus the getChat title (best-effort), or a
/// fixed banner key to redirect with:
/// - private: the trimmed input must parse as `i64` AND start with `-100`
///   (else `private-id`);
/// - public: `normalize_channel_input` yields `@handle` from
///   `@handle`/`t.me/<handle>` — a numeric paste (a private ID) or junk under
///   Public is one honest refusal (`public-shape`);
/// - absent/unrecognized: the legacy omnibus path, today's behavior verbatim
///   — no shape enforcement, so scripted and legacy posters keep working.
///   Fail-open is correct here: getChat resolution stays best-effort and this
///   is a UX gate, not a security boundary.
async fn resolve_channel_add(
    bot: &Bot,
    ch: &str,
    visibility: Option<&str>,
) -> Result<(String, Option<String>), &'static str> {
    match visibility {
        Some("private") => {
            if !(ch.parse::<i64>().is_ok() && ch.starts_with("-100")) {
                return Err("private-id");
            }
            Ok(match channel_chat_info(bot, ch).await {
                Some((id, title)) => (id.to_string(), title),
                None => (ch.to_string(), None),
            })
        }
        Some("public") => match normalize_channel_input(ch) {
            // §3.4.4: public rows store @handle; getChat still runs for the
            // TITLE only — its numeric id is discarded, so the stored shape
            // matches the radio and the shape-derived badge stays truthful
            // (§6). Dialing @handle is fully supported (util.rs to_chat_id).
            Some(canonical) if canonical.parse::<i64>().is_err() => {
                let title = channel_chat_info(bot, &canonical)
                    .await
                    .and_then(|(_, t)| t);
                Ok((canonical, title))
            }
            _ => Err("public-shape"),
        },
        // Legacy omnibus path (§3.4.1).
        _ => Ok(match channel_chat_info(bot, ch).await {
            Some((id, title)) => (id.to_string(), title),
            None => (ch.to_string(), None),
        }),
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn channels_add(
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
        return Redirect::to(&state.url("/channels/add?error=empty")).into_response();
    }
    // Resolve the input (an @handle, a t.me link, or a numeric id) to a
    // canonical chat identifier via Telegram when the bot can answer
    // get_chat: a channel with both a title and an id is identified by its
    // id, and the title is display-only. Resolution is BEST-EFFORT — when
    // the bot cannot answer (placeholder token, offline, chat not visible
    // yet) the input is stored as-is so adding still succeeds; the title
    // simply stays unknown.
    let vis = f
        .visibility
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let user_title = f.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let bot = if Role::from_role_str(&session.role) == Role::Seller {
        session
            .seller_id
            .map(|sid| state.bots.for_seller(sid))
            .unwrap_or_else(|| state.bots.primary.clone())
    } else {
        effective_bot(&state, &session)
    };
    let (canonical, resolved_title) = match resolve_channel_add(&bot.bot, ch, vis).await {
        Ok(res) => res,
        Err(key) => {
            return Redirect::to(&state.url(&format!("/channels/add?error={key}"))).into_response();
        }
    };
    // §3.4.5: user-typed title > getChat title > none. notitle rides the
    // redirect only when BOTH are absent — a typed title means nothing is
    // unresolved.
    let chosen_title = user_title.map(str::to_string).or(resolved_title);
    let notitle_q = if chosen_title.is_none() {
        "&notitle=1"
    } else {
        ""
    };
    // §3.4.6: visibility-aware adds anchor the redirect at the new row so the
    // page can reveal it (§4); legacy adds (no visibility field) keep today's
    // bare flags byte-identical.
    let added_q = match vis {
        None => format!("/channels?added=1{notitle_q}"),
        Some(_) => format!(
            "/channels?added=1&channel={}{}",
            urlencode_query(&canonical),
            notitle_q
        ),
    };
    // Sellers adding = setting their own channel (the bot's /setchannel
    // semantics); owners add to the global set (seller_id 0).
    if Role::from_role_str(&session.role) == Role::Seller {
        match session.seller_id {
            Some(sid) => {
                state.db.set_seller_channel(sid, &canonical);
                Redirect::to(&state.url(&added_q)).into_response()
            }
            None => forbidden_page(&state, "Channels", "Your session has no seller profile."),
        }
    } else if Role::from_role_str(&session.role).has_level(Role::Owner) {
        match state.db.add_channel(&canonical, 0) {
            Ok(()) => {
                state
                    .db
                    .set_channel_title(&canonical, chosen_title.as_deref());
                Redirect::to(&state.url(&added_q)).into_response()
            }
            // The DB-Err inline path (duplicate NOCASE add) re-renders the
            // ADD page with the focused alert (spec §3.3).
            Err(e) => channels_add_error(&state, &session, &esc(&e)),
        }
    } else {
        forbidden_page(&state, "Channels", "Global channels")
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn channels_default(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Channels", "The default channel");
    }
    let ch = f.channel.trim();
    if ch.is_empty() {
        return Redirect::to(&state.url("/channels?error=empty")).into_response();
    }
    // set_default_channel refuses seller-owned rows and unknown names —
    // surface that honestly instead of redirecting as if it worked.
    match state.db.set_default_channel(ch) {
        // PRG to the detail view (spec §2): the flagged row is the managed one.
        Ok(()) => Redirect::to(&state.url(&format!(
            "/channels/manage?channel={}&default=1",
            urlencode_query(ch)
        )))
        .into_response(),
        Err(e) => channels_error(&state, &session, &esc(&e)),
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn channels_remove(
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
        return Redirect::to(&state.url("/channels?error=empty")).into_response();
    }
    // The default channel is the fallback posting target: removing it would
    // silently break every plan that relies on the fallback, so it is
    // refused until another channel takes over. The check is scope-aware —
    // seller rows are never the global default.
    if Role::from_role_str(&session.role).has_level(Role::Owner) {
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
    } else if Role::from_role_str(&session.role) == Role::Seller {
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

#[derive(Deserialize)]
pub(super) struct ChannelRenameForm {
    pub(super) old: Option<String>,
    #[serde(rename = "old_name")]
    pub(super) old_name: Option<String>,
    #[serde(rename = "old_channel")]
    pub(super) old_channel: Option<String>,
    #[serde(rename = "from")]
    pub(super) from: Option<String>,
    #[serde(rename = "from_name")]
    pub(super) from_name: Option<String>,
    #[serde(rename = "channel")]
    pub(super) channel: Option<String>,
    #[serde(rename = "source")]
    pub(super) source: Option<String>,
    pub(super) new: Option<String>,
    #[serde(rename = "new_name")]
    pub(super) new_name: Option<String>,
    #[serde(rename = "new_channel")]
    pub(super) new_channel: Option<String>,
    #[serde(rename = "to")]
    pub(super) to: Option<String>,
    #[serde(rename = "to_name")]
    pub(super) to_name: Option<String>,
    #[serde(rename = "target")]
    pub(super) target: Option<String>,
    #[serde(rename = "rename_to")]
    pub(super) rename_to: Option<String>,
}

fn first_non_empty(opts: &[Option<String>]) -> String {
    for v in opts.iter().flatten() {
        let t = v.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    String::new()
}

#[tracing::instrument(skip_all)]
pub(super) async fn channels_rename(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelRenameForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let sid = if Role::from_role_str(&session.role).has_level(Role::Owner) {
        0i64
    } else if Role::from_role_str(&session.role) == Role::Seller {
        match session.seller_id {
            Some(id) => id,
            None => {
                return Redirect::to(&state.url("/channels?error=Forbidden%3A+no+seller+profile"))
                    .into_response();
            }
        }
    } else {
        return Redirect::to(
            &state.url("/channels?error=Forbidden%3A+not+allowed+to+rename+channels"),
        )
        .into_response();
    };
    let old = first_non_empty(&[
        f.old.clone(),
        f.old_name.clone(),
        f.old_channel.clone(),
        f.from.clone(),
        f.from_name.clone(),
        f.channel.clone(),
        f.source.clone(),
    ]);
    let new = first_non_empty(&[
        f.new.clone(),
        f.new_name.clone(),
        f.new_channel.clone(),
        f.to.clone(),
        f.to_name.clone(),
        f.target.clone(),
        f.rename_to.clone(),
    ]);
    if old.is_empty() || new.is_empty() {
        return Redirect::to(&state.url("/channels?error=empty")).into_response();
    }
    // Self-rename guard (MANAGEMENT_DISCLOSURE_SPEC §5): a NOCASE match of
    // the box against the identifier changed nothing — say so (mirrors
    // ?notitle=1) instead of a false "renamed" banner. PRGs land on the
    // detail view (spec §2); the DB-Err target carries the OLD identifier.
    if old.eq_ignore_ascii_case(&new) {
        return Redirect::to(&state.url(&format!(
            "/channels/manage?channel={}&renamed=1&same=1",
            urlencode_query(&old)
        )))
        .into_response();
    }
    match state.db.rename_channel(
        sid,
        &old,
        &new,
        Role::from_role_str(&session.role).has_level(Role::Owner),
    ) {
        Ok(()) => Redirect::to(&state.url(&format!(
            "/channels/manage?channel={}&renamed=1",
            urlencode_query(&new)
        )))
        .into_response(),
        Err(e) => Redirect::to(&state.url(&format!(
            "/channels/manage?channel={}&error={}",
            urlencode_query(&old),
            urlencode_query(&e)
        )))
        .into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct ChannelNameForm {
    pub(super) channel: String,
    pub(super) name: Option<String>,
}

/// POST /channels/name sets a global channel's display label
/// (`channels.title`) via the existing Db::set_channel_title (db.rs:2031) —
/// the human label, never the dialed identifier (CHANNEL_ADD_REDESIGN_SPEC
/// §5a). Owner-only: global rows are owner data. The identifier is rewritten
/// only by POST /channels/rename (§5b), which is why this is a separate
/// route and not a rename-handler field (`POST /channels/title` is occupied
/// by the Bot API set_chat_title handler).
#[tracing::instrument(skip_all)]
pub(super) async fn channels_name(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelNameForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Channels", "Channel names");
    }
    let ch = f.channel.trim();
    if ch.is_empty() {
        return Redirect::to(&state.url("/channels?error=empty")).into_response();
    }
    // set_channel_title no-ops on a missing row — say so instead of a false
    // success (existence probe precedent: bind-sender, channels.rs:743).
    if state.db.get_channel_seller_id(ch) != Some(0) {
        return channels_error(
            &state,
            &session,
            "That channel isn't in the global list — add it first.",
        );
    }
    let name = f.name.as_deref().map(str::trim).filter(|t| !t.is_empty());
    state.db.set_channel_title(ch, name);
    // Audit parity with bind-sender (§5c): a label edit writes a log_action row.
    let channel_id = state.db.get_channel_id(ch).unwrap_or(0);
    state.db.log_action(
        session.account_id,
        &session.role,
        "channel_name",
        "channel",
        channel_id,
        None,
    );
    // PRG to the detail view (spec §2): the named row is the managed one.
    Redirect::to(&state.url(&format!(
        "/channels/manage?channel={}&named=1",
        urlencode_query(ch)
    )))
    .into_response()
}

#[derive(Deserialize)]
pub(super) struct ChannelBindForm {
    pub(super) channel: String,
    pub(super) identity: String,
}

/// POST /channels/bind-sender set one global channel's posting sender
/// (`'bot'` | `'user'`, IDENTITY_SENDER_UX_SPEC §1–§3). Owner-only: the
/// identity is the owner's personal account, so binding is an owner decision
/// (sellers and admins get 403; they see the badge as a fact only).
///
/// Binding to `'user'` requires, in order: the Enable-posting gate
/// (`crate::identity::user_writes_enabled` — the console points at that gate,
/// it never re-asks the ToS question here, spec §3) and a **Ready** identity —
/// binding a locked or disconnected identity would brick every send on the
/// channel, so the mutation is refused and any existing binding survives
/// untouched. Binding back to `'bot'` is always allowed. Both directions
/// write a `log_action` audit row (spec §7.2).
#[tracing::instrument(skip_all)]
pub(super) async fn channels_bind_sender(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ChannelBindForm>,
) -> Response {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Channels", "Channel sender binding");
    }
    let ch = f.channel.trim();
    if ch.is_empty() {
        return Redirect::to(&state.url("/channels?error=empty")).into_response();
    }
    if !matches!(f.identity.trim(), "bot" | "user") {
        return channels_error(
            &state,
            &session,
            "That sender choice isn't one of the console's — use the row buttons on the Channels page.",
        );
    }
    let identity = f.identity.trim();
    // The binding decides whose account posts, so it exists only for the
    // owner-managed global channels; a seller's channel always posts via the
    // seller's own bot (a forged POST must not route seller content through
    // the owner's personal account).
    if state.db.get_channel_seller_id(ch) != Some(0) {
        return channels_error(
            &state,
            &session,
            &format!(
                "{} isn't one of the global channels — add it on this page first.",
                esc(ch)
            ),
        );
    }
    if identity == "user" {
        // Gate first (spec §3): the Enable-posting acceptance is the one ToS
        // surface; this page links to it, never re-asks it.
        if !crate::identity::user_writes_enabled(&state.db) {
            return Redirect::to(&state.url(&format!(
                "/channels/manage?channel={}&error=posting",
                urlencode_query(ch)
            )))
            .into_response();
        }
        // Then readiness: binding 'user' while the identity is locked (or not
        // connected) would brick every send on the channel. Refuse the
        // mutation with the §3 copy — the stored binding survives.
        match state.identity.status().await.state {
            IdentityState::Ready => {}
            IdentityState::Locked => {
                return Redirect::to(&state.url(&format!(
                    "/channels/manage?channel={}&error=locked",
                    urlencode_query(ch)
                )))
                .into_response();
            }
            _ => {
                return Redirect::to(&state.url(&format!(
                    "/channels/manage?channel={}&error=identity",
                    urlencode_query(ch)
                )))
                .into_response();
            }
        }
    }
    if !state.db.set_channel_sender_identity(ch, identity) {
        return channels_error(
            &state,
            &session,
            "The binding could not be written — the channel row went missing or the store refused the change.",
        );
    }
    let (action, flash) = if identity == "user" {
        ("channel_bind_user", "bound-user=1")
    } else {
        ("channel_bind_bot", "bound-bot=1")
    };
    let channel_id = state.db.get_channel_id(ch).unwrap_or(0);
    state.db.log_action(
        session.account_id,
        &session.role,
        action,
        "channel",
        channel_id,
        None,
    );
    // PRG (spec §3): the channel name rides the redirect for the success
    // copy — an identifier, not PII (§7.5), escaped again at render.
    Redirect::to(&state.url(&format!(
        "/channels/manage?channel={}&{flash}",
        urlencode_query(ch)
    )))
    .into_response()
}
