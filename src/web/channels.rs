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
        &channels_body(&state, &session, None, success_banner.as_deref()),
    ))
    .into_response()
}

/// One console banner line (MANAGEMENT_DISCLOSURE_SPEC §5e): fixed wrapper,
/// caller-owned copy. info/success/warn announce politely; error is
/// assertive — output is byte-identical to the previous inline blocks.
fn channels_banner(kind: &str, msg: &str) -> String {
    let (class, attrs) = if kind == "error" {
        ("alert-error", "role=\"alert\"")
    } else {
        (kind, "role=\"status\" aria-live=\"polite\"")
    };
    format!("<div class=\"alert {class}\" {attrs}>{msg}</div>")
}

/// The channels page's PRG outcome banners, one per redirect flag — every
/// line wraps through `channels_banner` (MANAGEMENT_DISCLOSURE_SPEC §5e).
fn channels_banners(q: &ChannelsQuery, state: &WebState, session: &Session) -> Vec<String> {
    // The same query-param idiom /subs and /finance use. ?notitle=1 means
    // the add succeeded but the Telegram title could not be resolved.
    let mut banners: Vec<String> = Vec::new();
    if q.added.as_deref() == Some("1") {
        if q.notitle.as_deref() == Some("1") {
            // Honest add (CHANNELS_TITLE_UX_SPEC §3): resolution failed, say so.
            let tail = match session.role.as_str() {
                "owner" => " Use Rename to name it.",
                _ => "",
            };
            banners.push(channels_banner(
                "info",
                &format!("Channel added — I couldn't fetch its name from Telegram (the bot may not be an admin there yet).{tail}"),
            ));
        } else {
            banners.push(channels_banner("success", "Channel added."));
        }
    }
    if q.default.as_deref() == Some("1") {
        banners.push(channels_banner("success", "Default channel set."));
    }
    if q.removed.as_deref() == Some("1") {
        banners.push(channels_banner("success", "Channel removed."));
    }
    if q.renamed.as_deref() == Some("1") && q.same.as_deref() == Some("1") {
        let msg = "Nothing changed — that's still the channel's ID. Clear the box and type a name to label it.";
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
    if let Some(e) = q.error.as_ref() {
        // Fixed keys render fixed copy; unknown values stay escaped — a
        // query string never injects markup. Refusals link the Identity gate.
        let msg = match e.as_str() {
            "empty" => "Type a channel name first.".to_string(),
            "posting" => {
                if session.role == "owner" {
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
                if session.role == "owner" {
                    format!(
                        "The personal identity isn't connected — finish connecting it on the <a href=\"{}\">Identity page</a> first.",
                        state.url("/identity")
                    )
                } else {
                    "The personal identity isn't connected — the console owner connects it on the Identity page first.".to_string()
                }
            }
            other => esc(other),
        };
        banners.push(channels_banner("error", &msg));
    }
    banners
}

/// Query shared by the channels GET page: the success flags its mutation
/// handlers redirect with — `notitle` marks the add that succeeded without a
/// Telegram title, `same` marks the NOCASE self-rename guard (spec §5) —
/// plus the sender-binding flashes (IDENTITY_SENDER_UX_SPEC §8.4) and
/// `channel`, the binding's channel name for the success copy (§7.5).
#[derive(Deserialize)]
pub(super) struct ChannelsQuery {
    pub(super) added: Option<String>,
    pub(super) notitle: Option<String>,
    pub(super) default: Option<String>,
    pub(super) removed: Option<String>,
    pub(super) renamed: Option<String>,
    pub(super) same: Option<String>,
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
/// HTML (?added=1 / ?default=1 / ?removed=1).
pub(super) fn channels_body(
    state: &WebState,
    session: &Session,
    alert: Option<&str>,
    success: Option<&str>,
) -> String {
    let alert = alert
        .map(|a| channels_banner("error", a))
        .unwrap_or_default();
    let success = success.unwrap_or_default().to_string();
    if session.role == "seller" {
        return render_seller_channels(state, session, &alert, &success);
    }
    render_global_channels(state, session, &alert, &success)
}

fn render_seller_channels(
    state: &WebState,
    session: &Session,
    alert: &String,
    success: &String,
) -> String {
    let sid = session.seller_id.unwrap_or(0);
    let list = match state.db.seller_channel(sid) {
        Some(ch) => {
            // Same label contract as the owner rows (CHANNELS_TITLE_UX_SPEC
            // §2): the title is read from the channels table, case-insensitively.
            let title = state
                .db
                .list_channels(Some(sid))
                .into_iter()
                .find(|row| row.channel.eq_ignore_ascii_case(&ch))
                .and_then(|row| row.title);
            let label = channel_label(title.as_deref(), &ch);
            let notitle_hint = if title.as_deref().is_none_or(|t| t.trim().is_empty()) {
                r#"<span class="help">no title stored for this channel yet</span>"#
            } else {
                ""
            };
            let type_badge = channel_type_badge(&ch);
            let confirm_msg = esc(&format!(
                "Remove {}? This cannot be undone.",
                js_arg(&ch)
            ));
            format!(
                "<li><span class=\"mono\">{}</span> {}<span class=\"badge badge-success\">bot-managed</span>{} <div class=\"row-actions\"><form method=\"post\" action=\"{}\" class=\"inline-form\"><input type=\"hidden\" name=\"channel\" value=\"{}\"><button type=\"submit\" class=\"btn btn-danger btn-sm\" aria-label=\"Remove {}\" onclick=\"return confirm('{}')\">Delete</button></form></div></li>",
                label,
                type_badge,
                notitle_hint,
                state.url("/channels/remove"),
                esc(&ch),
                esc(&ch),
                confirm_msg
            )
        }
        None => "<li class=\"empty\"><b>No channel yet.</b>Add one below, or use /setchannel in the bot.</li>"
            .to_string(),
    };
    format!(
        r#"{alert}{success}<h1>Channels <span class="sub">your posting target</span></h1>
<article class="panel"><h3>Your channel</h3><ul>{list}</ul>
<form method="post" action="{}">
<label for="channel">Channel</label><input id="channel" name="channel" required placeholder="@yourchannel"><span class="help">@handle, t.me link, or -100 ID — the bot must be admin in the channel.</span><details class="help-more"><summary>Which forms are accepted?</summary><span class="help">Accepted: @handle, t.me link, or numeric -100 ID. Recommended: the channel's @handle (memorable) or its t.me link. Private channel without a public @handle: paste the numeric chat ID (starts with -100) — forward any message from it to @userinfobot to get it. The bot must be an admin in the channel. Adding one replaces the previous.</span></details>
<button type="submit">Set channel</button>
</form></article>"#,
        state.url("/channels/add"),
        alert = alert,
        success = success,
        list = list,
    )
}

fn render_global_channels(
    state: &WebState,
    session: &Session,
    alert: &String,
    success: &String,
) -> String {
    let is_owner = session.role == "owner";
    let scope_bot = if is_owner {
        session.selected_bot_id
    } else {
        None
    };
    let fetched = state.db.list_channels(None);
    // An empty SCOPED list with rows present is a scoping artifact, not an
    // empty set (MANAGEMENT_DISCLOSURE_SPEC §3) — count the unscoped rows.
    let total_global = fetched.iter().filter(|row| row.seller_id == 0).count();
    let rows = fetched
        .into_iter()
        .filter(|row| row.seller_id == 0)
        .filter(|row| scope_bot.is_none_or(|bid| state.bots.for_channel(&row.channel).id == bid))
        .map(|row| render_global_channel_row(state, is_owner, row))
        .collect::<Vec<_>>()
        .join("");
    let list = if rows.is_empty() && scope_bot.is_some() && total_global > 0 {
        "<li class=\"empty\"><b>No channels resolve to this bot yet.</b> The bot may not be connected (no token) or these channels belong to another bot — switch the Bot selector above to All bots.</li>".to_string()
    } else if rows.is_empty() {
        "<li class=\"empty\"><b>No global channels yet.</b> Add one below — it becomes a posting target for plans and posts.</li>".to_string()
    } else {
        rows
    };
    let add_form = if is_owner {
        format!(
            r#"<form method="post" action="{}">
<label for="channel">Channel</label><input id="channel" name="channel" required placeholder="@yourchannel"><span class="help">@handle, t.me link, or -100 ID — the bot must be admin in the channel.</span><details class="help-more"><summary>Which forms are accepted?</summary><span class="help">Accepted: @handle, t.me link, or numeric -100 ID. Recommended: the channel's @handle (memorable) or its t.me link. Private channel without a public @handle: paste the numeric chat ID (starts with -100) — forward any message from it to @userinfobot to get it. The bot must be an admin in the channel. Adding one does not change the default — use Set default.</span></details>
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

fn render_global_channel_row(
    state: &WebState,
    is_owner: bool,
    row: crate::db::ChannelRow,
) -> String {
    let type_badge = channel_type_badge(&row.channel);
    let badge = if row.is_default {
        "<span class=\"badge badge-success\">default</span>".to_string()
    } else {
        String::new()
    };
    let user_bound = state
        .db
        .get_channel_sender_identity(&row.channel)
        .as_deref()
        == Some("user");
    let user_badge = if user_bound {
        "<span class=\"badge badge-warning\">as user</span>"
    } else {
        ""
    };
    let bind_action = if is_owner {
        if user_bound {
            format!(
                "<form method=\"post\" action=\"{}\" class=\"inline-form\"><input type=\"hidden\" name=\"channel\" value=\"{}\"><input type=\"hidden\" name=\"identity\" value=\"bot\"><button type=\"submit\" class=\"btn btn-secondary btn-sm\" aria-label=\"Post to {} as the bot\">Post as bot</button></form>",
                state.url("/channels/bind-sender"),
                esc(&row.channel),
                esc(&row.channel)
            )
        } else {
            let confirm_msg = esc(&format!(
                "Bind {} to your personal account? Posts to this channel will be sent as you — not as the bot — and every member sees them as yours.",
                js_arg(&row.channel)
            ));
            format!(
                "<form method=\"post\" action=\"{}\" class=\"inline-form\"><input type=\"hidden\" name=\"channel\" value=\"{}\"><input type=\"hidden\" name=\"identity\" value=\"user\"><button type=\"submit\" class=\"btn btn-ghost btn-sm\" aria-label=\"Post to {} as your personal account\" onclick=\"return confirm('{}')\">Post as user…</button></form>",
                state.url("/channels/bind-sender"),
                esc(&row.channel),
                esc(&row.channel),
                confirm_msg
            )
        }
    } else {
        String::new()
    };
    let set_default_action = if is_owner && !row.is_default {
        format!(
            "<form method=\"post\" action=\"{}\" class=\"inline-form\"><input type=\"hidden\" name=\"channel\" value=\"{}\"><button type=\"submit\" class=\"btn btn-ghost btn-sm\" aria-label=\"Set {} as default\">Set default</button></form>",
            state.url("/channels/default"),
            esc(&row.channel),
            esc(&row.channel)
        )
    } else {
        String::new()
    };
    let delete_action = if is_owner {
        // Owners can remove (delete) non-default rows only. The default
        // channel is protected: show a hint instead of a Delete button.
        if row.is_default {
            String::new()
        } else {
            let confirm_msg = esc(&format!(
                "Remove {}? This cannot be undone.",
                js_arg(&row.channel)
            ));
            format!(
                "<form method=\"post\" action=\"{}\" class=\"inline-form\"><input type=\"hidden\" name=\"channel\" value=\"{}\"><button type=\"submit\" class=\"btn btn-danger btn-sm\" aria-label=\"Delete {}\" onclick=\"return confirm('{}')\">Delete</button></form>",
                state.url("/channels/remove"),
                esc(&row.channel),
                esc(&row.channel),
                confirm_msg
            )
        }
    } else {
        String::new()
    };
    // Sellers can remove their own channel; handled in seller view, not here.
    let rename_id = esc(&format!("rename-{}", row.channel));
    // MANAGEMENT_DISCLOSURE_SPEC §5: pre-fill the stored title when one
    // exists; else EMPTY with a plain placeholder — the identifier rides the
    // hidden `channel` field, never the visible box.
    let current = row.title.as_deref().map(str::trim);
    let current = current.filter(|t| !t.is_empty());
    let (rename_value, rename_ph) = match current {
        Some(t) => (esc(t), ""),
        None => (String::new(), "Friendly name, e.g. Tech Tips"),
    };
    let rename_details = if is_owner {
        format!(
            r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="channel" value="{}"><label class="sr-only" for="{}">New name for {}</label><input id="{}" name="new" value="{}" placeholder="{rename_ph}" required><button type="submit" class="btn btn-secondary btn-sm" aria-label="Rename {}">Rename</button></form>"#,
            state.url("/channels/rename"),
            esc(&row.channel),
            rename_id,
            esc(&row.channel),
            rename_id,
            rename_value,
            esc(&row.channel)
        )
    } else {
        String::new()
    };
    let default_hint = if is_owner && row.is_default {
        r#"<span class="help">Set another channel as default to remove</span>"#.to_string()
    } else {
        String::new()
    };
    let actions =
        format!("{bind_action}{set_default_action}{rename_details}{delete_action}{default_hint}");
    // Show the human title (when known) alongside the canonical id so a title
    // and its id are never mistaken for two different channels (CHANNELS_TITLE_UX_SPEC §2).
    let label = channel_label(row.title.as_deref(), &row.channel);
    // The hint names the Rename action, which is owner-only (F-2): an admin
    // reading a titleless global row sees the bare id, not a dead pointer.
    let notitle_hint = if is_owner && row.title.as_deref().is_none_or(|t| t.trim().is_empty()) {
        r#"<span class="help">no title yet — use Rename to name it</span>"#
    } else {
        ""
    };
    format!(
        "<li><span class=\"mono\">{}</span> {}{badge}{user_badge}{notitle_hint}<div class=\"row-actions\">{actions}</div></li>",
        label,
        type_badge,
        badge = badge,
        user_badge = user_badge,
        notitle_hint = notitle_hint,
        actions = actions
    )
}

/// Re-render the channels page with an inline error (no redirect: the failed
/// action's message belongs on the page it failed from).
pub(super) fn channels_error(state: &WebState, session: &Session, msg: &str) -> Response {
    Html(html_page(
        state,
        Some(session),
        "Channels",
        &channels_body(state, session, Some(msg), None),
    ))
    .into_response()
}

fn channel_type_badge(ch: &str) -> &'static str {
    if ch.starts_with('@') {
        r#"<span class="badge">public</span>"#
    } else {
        r#"<span class="badge">private</span>"#
    }
}

/// The house channel label: "Title (id)" when a human title is stored, the
/// bare identifier otherwise. Every console surface that shows a channel
/// renders through this, so a channel never reads two different ways
/// (CHANNELS_TITLE_UX_SPEC §4). Both fields are escaped here.
pub(super) fn channel_label(title: Option<&str>, channel: &str) -> String {
    match title.map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => format!("{} ({})", esc(t), esc(channel)),
        None => esc(channel),
    }
}

/// One `list_channels` scan → lowercase-keyed title map for pages that show
/// channel identifiers (dashboard readout, Posts, Subs, Sellers, Monitor).
/// Keys are `to_ascii_lowercase()` to mirror the column's NOCASE collation.
/// No new SQL — the existing read, shaped for lookup.
pub(super) fn channel_titles(state: &WebState) -> std::collections::HashMap<String, String> {
    state
        .db
        .list_channels(None)
        .into_iter()
        .filter_map(|row| {
            row.title
                .filter(|t| !t.trim().is_empty())
                .map(|t| (row.channel.to_ascii_lowercase(), t))
        })
        .collect()
}

#[derive(Deserialize)]
pub(super) struct ChannelForm {
    pub(super) channel: String,
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
        return Redirect::to(&state.url("/channels?error=empty")).into_response();
    }
    // Resolve the input (an @handle, a t.me link, or a numeric id) to a
    // canonical numeric chat id via Telegram when the bot can answer
    // get_chat: a channel with both a title and an id is identified by its
    // id, and the title is display-only. Resolution is BEST-EFFORT — when
    // the bot cannot answer (placeholder token, offline, chat not visible
    // yet) the input is stored as-is so adding still succeeds; the title
    // simply stays unknown.
    let bot = if session.role == "seller" {
        session
            .seller_id
            .map(|sid| state.bots.for_seller(sid))
            .unwrap_or_else(|| state.bots.primary.clone())
    } else {
        effective_bot(&state, &session)
    };
    let (canonical, title) = match channel_chat_info(&bot.bot, ch).await {
        Some((id, title)) => (id.to_string(), title),
        None => (ch.to_string(), None),
    };
    // PRG honesty (CHANNELS_TITLE_UX_SPEC §3): when getChat couldn't answer,
    // the redirect carries ?notitle=1 so the page says so instead of a bare
    // "Channel added."
    let added_q = if title.is_none() {
        "/channels?added=1&notitle=1"
    } else {
        "/channels?added=1"
    };
    // Sellers adding = setting their own channel (the bot's /setchannel
    // semantics); owners add to the global set (seller_id 0).
    if session.role == "seller" {
        match session.seller_id {
            Some(sid) => {
                state.db.set_seller_channel(sid, &canonical);
                Redirect::to(&state.url(added_q)).into_response()
            }
            None => forbidden_page(&state, "Channels", "Your session has no seller profile."),
        }
    } else if session.role == "owner" {
        match state.db.add_channel(&canonical, 0) {
            Ok(()) => {
                state.db.set_channel_title(&canonical, title.as_deref());
                Redirect::to(&state.url(added_q)).into_response()
            }
            Err(e) => channels_error(&state, &session, &esc(&e)),
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
    if session.role != "owner" {
        return forbidden_page(&state, "Channels", "The default channel");
    }
    let ch = f.channel.trim();
    if ch.is_empty() {
        return Redirect::to(&state.url("/channels?error=empty")).into_response();
    }
    // set_default_channel refuses seller-owned rows and unknown names —
    // surface that honestly instead of redirecting as if it worked.
    match state.db.set_default_channel(ch) {
        Ok(()) => Redirect::to(&state.url("/channels?default=1")).into_response(),
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
    let sid = if session.role == "owner" {
        0i64
    } else if session.role == "seller" {
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
    // ?notitle=1) instead of a false "renamed" banner.
    if old.eq_ignore_ascii_case(&new) {
        return Redirect::to(&state.url("/channels?renamed=1&same=1")).into_response();
    }
    match state
        .db
        .rename_channel(sid, &old, &new, session.role == "owner")
    {
        Ok(()) => Redirect::to(&state.url(&format!(
            "/channels?renamed=1&channel={}",
            urlencode_query(&new)
        )))
        .into_response(),
        Err(e) => Redirect::to(&state.url(&format!("/channels?error={}", urlencode_query(&e))))
            .into_response(),
    }
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
    if session.role != "owner" {
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
            return Redirect::to(&state.url("/channels?error=posting")).into_response();
        }
        // Then readiness: binding 'user' while the identity is locked (or not
        // connected) would brick every send on the channel. Refuse the
        // mutation with the §3 copy — the stored binding survives.
        match state.identity.status().await.state {
            IdentityState::Ready => {}
            IdentityState::Locked => {
                return Redirect::to(&state.url("/channels?error=locked")).into_response();
            }
            _ => {
                return Redirect::to(&state.url("/channels?error=identity")).into_response();
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
        "/channels?{flash}&channel={}",
        urlencode_query(ch)
    )))
    .into_response()
}
