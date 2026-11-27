//! The channels master–detail pair (CHANNELS_MASTER_DETAIL_SPEC §3/§5): the
//! dedicated `GET /channels/add` page (owner and seller variants) and the
//! per-channel `GET /channels/manage` detail view with its five sections.
//! The detail section builders MOVE byte-verbatim from the old row cluster
//! (`channel_row_actions_owner` / `channel_row_name_controls`) — same POST
//! routes, same field names, same gates, same confirms; only the page
//! placement changed. The pinned row assertions in `src/web/tests/channels.rs`
//! and `src/web/identity_tests.rs` are the oracle.
use super::*;

// ---- GET /channels/add — the dedicated add page (spec §3) ------------------

/// The add page body. `banner` is pre-rendered banner HTML (the `?error=`
/// PRG banner through `channels_banner`, or the focused DB-Err alert through
/// `channels_add_error`) and renders after the h1, before the form panel.
/// Role-branched: the seller variant has NO Title field (§3.2 honesty —
/// `set_channel_title` only matches `channels` rows; seller channels live in
/// `sellers.default_channel`, so a seller-typed title would be dropped).
fn channels_add_body(state: &WebState, session: &Session, banner: &str) -> String {
    if Role::from_role_str(&session.role) == Role::Seller {
        return channels_add_body_seller(state, banner);
    }
    channels_add_body_owner(state, banner)
}

/// The owner add page (spec §3.1): rail-shell panel page (house precedent:
/// the Add-bot wizard). The form block is a byte-verbatim move of the old
/// list-page slot (CHANNEL_ADD_REDESIGN_SPEC §3.1) — no field, id, help, or
/// copy changes; APP_JS's `data-visibility-form` binder is page-generic.
fn channels_add_body_owner(state: &WebState, banner: &str) -> String {
    let form = format!(
        r#"<form method="post" action="{add_action}" data-visibility-form>
<label for="ch-title">Title</label>
<input id="ch-title" name="title" maxlength="64" autocomplete="off" placeholder="e.g. Wire Guard">
<span class="help">Your label for this channel — how it reads in lists and pickers. Leave empty and I'll fill it from Telegram when the bot can see the channel.</span>

<fieldset class="visibility">
<legend>Visibility</legend>
<label class="check"><input type="radio" name="visibility" value="public" checked> Public — it has an @handle anyone can find</label>
<label class="check"><input type="radio" name="visibility" value="private"> Private — invite-only; addressed by its numeric ID</label>
</fieldset>

<label for="ch-id">Channel</label>
<input id="ch-id" name="channel" required autocomplete="off" spellcheck="false"
       data-ph-public="@handle" data-ph-private="-1001234567890123" placeholder="@handle"
       aria-describedby="ch-id-help-public">
<span class="help" id="ch-id-help-public">@handle or a t.me link — the bot must be admin in the channel.</span>
<span class="help" id="ch-id-help-private" hidden>Its numeric -100 ID only — forward any message from the channel to @userinfobot to get the ID.</span>

<details class="help-more"><summary>Which forms are accepted?</summary><span class="help">Public: @handle or a t.me link (stored as @handle). Private: the numeric chat ID only — it starts with -100; forward any message from the channel to @userinfobot to get it. The bot must be an admin in the channel. Adding one does not change the default — use Set default.</span></details>

<button type="submit">Add channel</button>
</form>"#,
        add_action = state.url("/channels/add")
    );
    format!(
        r#"<h1>Add channel <span class="sub">new posting target</span></h1>
{banner}
<article class="panel">
{form}
<p><a href="{back}">Back to channels</a></p>
</article>"#,
        banner = banner,
        form = form,
        back = state.url("/channels"),
    )
}

/// The seller add page (spec §3.2): the same visibility radio and identifier
/// field with adaptive help as the owner's — but NO Title field, the own
/// help-more tail (`Adding one replaces the previous.`), and the `Set
/// channel` submit.
fn channels_add_body_seller(state: &WebState, banner: &str) -> String {
    let form = format!(
        r#"<form method="post" action="{add_action}" data-visibility-form>
<fieldset class="visibility">
<legend>Visibility</legend>
<label class="check"><input type="radio" name="visibility" value="public" checked> Public — it has an @handle anyone can find</label>
<label class="check"><input type="radio" name="visibility" value="private"> Private — invite-only; addressed by its numeric ID</label>
</fieldset>

<label for="ch-id">Channel</label>
<input id="ch-id" name="channel" required autocomplete="off" spellcheck="false"
       data-ph-public="@handle" data-ph-private="-1001234567890123" placeholder="@handle"
       aria-describedby="ch-id-help-public">
<span class="help" id="ch-id-help-public">@handle or a t.me link — the bot must be admin in the channel.</span>
<span class="help" id="ch-id-help-private" hidden>Its numeric -100 ID only — forward any message from the channel to @userinfobot to get the ID.</span>

<details class="help-more"><summary>Which forms are accepted?</summary><span class="help">Public: @handle or a t.me link (stored as @handle). Private: the numeric chat ID only — it starts with -100; forward any message from the channel to @userinfobot to get it. The bot must be an admin in the channel. Adding one replaces the previous.</span></details>

<button type="submit">Set channel</button>
</form>"#,
        add_action = state.url("/channels/add")
    );
    format!(
        r#"<h1>Add channel <span class="sub">your posting target</span></h1>
{banner}
<article class="panel">
{form}
<p><a href="{back}">Back to channels</a></p>
</article>"#,
        banner = banner,
        form = form,
        back = state.url("/channels"),
    )
}

/// GET /channels/add (CHANNELS_MASTER_DETAIL_SPEC §3): the dedicated add
/// page. Owner and seller → 200 (role-branched body); admin/user → 403,
/// mirroring the POST gate in `channels_add`; unauthenticated → /login.
/// Shape/empty refusals arrive as `?error=…` and render through the fixed
/// `channels_error_copy` keys (unknown values escaped; `role="alert"`).
#[tracing::instrument(skip_all)]
pub(super) async fn channels_add_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ChannelsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner)
        && Role::from_role_str(&session.role) != Role::Seller
    {
        return forbidden_page(&state, "Channels", "Global channels");
    }
    let banner = q
        .error
        .as_deref()
        .map(|e| channels_banner("error", &channels_error_copy(e, &state, &session)))
        .unwrap_or_default();
    Html(html_page(
        &state,
        Some(&session),
        "Channels",
        &channels_add_body(&state, &session, &banner),
    ))
    .into_response()
}

/// Re-render the ADD page with a focused inline error (spec §3.3): the DB-Err
/// path (duplicate NOCASE add) failed on this page, so its message belongs
/// here (precedent: `bot_add_error_page`) — replacing the old list-page
/// `channels_error` re-render for the add route only.
pub(super) fn channels_add_error(state: &WebState, session: &Session, msg: &str) -> Response {
    Html(html_page(
        state,
        Some(session),
        "Channels",
        &channels_add_body(state, session, &focused_error_alert(msg)),
    ))
    .into_response()
}

// ---- GET /channels/manage — the per-channel detail view (spec §5) ----------

/// A scoped-manage refusal page (spec §5.2): the fail-closed register of the
/// list's scoped-empty copy — an honest `alert-error` line + the back link,
/// 200, no channel data leaks. These paths are hand-typed URLs (the Manage
/// link only renders on in-scope rows) but must still fail honest.
fn channels_manage_refusal(state: &WebState, msg: &str) -> String {
    format!(
        r#"<p><a href="{back}">Back to channels</a></p>
<h1>Manage channel</h1>
{alert}"#,
        back = state.url("/channels"),
        alert = channels_banner("error", msg),
    )
}

/// GET /channels/manage?channel=X (CHANNELS_MASTER_DETAIL_SPEC §5): the
/// per-channel detail view. Owner: five sections (Identity → Address →
/// Default → Posting as → Danger zone). Admin: the same resolution, read-only
/// facts. Seller: own-channel facts + Danger zone. Resolution fails closed
/// (§5.2): unknown/seller-owned rows for an owner, foreign-bot rows under a
/// bot scope, and any non-own channel for a seller render the scoped
/// refusal; a missing `?channel=` redirects to the list.
#[tracing::instrument(skip_all)]
pub(super) async fn channels_manage_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<ChannelsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let Some(ch) = q
        .channel
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
    else {
        return Redirect::to(&state.url("/channels")).into_response();
    };
    if Role::from_role_str(&session.role) == Role::Seller {
        // The seller's own channel (the legacy entry may have no `channels`
        // row — the sellers column is the source of truth), NOCASE-matched.
        // A seller without a binding has no channel; 0 would be the operator's.
        let Some(sid) = session.seller_id else {
            let body = channels_manage_refusal(&state, "That channel isn't yours to manage.");
            return Html(html_page(&state, Some(&session), "Channels", &body)).into_response();
        };
        let own = state
            .db
            .seller_channel(sid)
            .is_some_and(|c| c.eq_ignore_ascii_case(&ch));
        let body = if own {
            channels_manage_body_seller(&state, &session, &ch, &q)
        } else {
            channels_manage_refusal(&state, "That channel isn't yours to manage.")
        };
        return Html(html_page(&state, Some(&session), "Channels", &body)).into_response();
    }
    // Owner and admin share the owner-side resolution (§5.2): the global row
    // with seller_id 0, NOCASE-matched (mirrors the list filter).
    let row = state
        .db
        .list_channels(None)
        .into_iter()
        .find(|row| row.seller_id == 0 && row.channel.eq_ignore_ascii_case(&ch));
    let Some(row) = row else {
        // Verbatim reuse of the channels_name refusal copy.
        let body = channels_manage_refusal(
            &state,
            "That channel isn't in the global list — add it first.",
        );
        return Html(html_page(&state, Some(&session), "Channels", &body)).into_response();
    };
    // Bot scope (§5.2): a scoped owner whose selected bot does not serve this
    // channel gets the honest refusal, same family as the list's
    // scoped-empty copy.
    if session
        .selected_bot_id
        .is_some_and(|bid| state.bots.for_channel(&row.channel).id != bid)
    {
        let body = channels_manage_refusal(
            &state,
            "This channel belongs to another bot — switch the Bot selector to All bots to manage it.",
        );
        return Html(html_page(&state, Some(&session), "Channels", &body)).into_response();
    }
    let body = if Role::from_role_str(&session.role).has_level(Role::Owner) {
        channels_manage_body_owner(&state, &session, &row, &q)
    } else {
        channels_manage_body_admin(&state, &row)
    };
    Html(html_page(&state, Some(&session), "Channels", &body)).into_response()
}

/// The owner detail body (spec §5.1): back link, h1 = the channel label,
/// state badge row, the action banners (`channels_banners` — flag-keyed and
/// page-agnostic, §6; the added-callout never renders here), then the five
/// sections in order. Heading order: h1 → h2 per section, no skips.
fn channels_manage_body_owner(
    state: &WebState,
    session: &Session,
    row: &crate::db::ChannelRow,
    q: &ChannelsQuery,
) -> String {
    let banners = channels_banners(q, state, session).join("\n");
    let user_bound = state
        .db
        .get_channel_sender_identity(&row.channel)
        .as_deref()
        == Some("user");
    let back = format!(
        r#"<p><a href="{}">Back to channels</a></p>"#,
        state.url("/channels")
    );
    format!(
        r#"{back}
<h1>{label}</h1>
<p>{type_badge}{default_badge}{user_badge}</p>
{banners}
{identity}
{address}
{default}
{posting}
{danger}"#,
        back = back,
        label = channel_label(row.title.as_deref(), &row.channel),
        type_badge = channel_type_badge(&row.channel),
        default_badge = if row.is_default {
            "<span class=\"badge badge-success\">default</span>"
        } else {
            ""
        },
        user_badge = if user_bound {
            "<span class=\"badge badge-warning\">as user</span>"
        } else {
            ""
        },
        banners = banners,
        identity = section_identity(state, row),
        address = section_address(state, row),
        default = section_default(state, row),
        posting = section_posting(state, row, user_bound),
        danger = section_danger(state, row),
    )
}

/// §5.3 Identity: the Edit-name control moved from the row — the form posts
/// the same fields (`channel`, `name`) to the same route (`/channels/name`);
/// the prefill contract is unchanged (stored title, else EMPTY with the plain
/// placeholder, MANAGEMENT_DISCLOSURE_SPEC §5) and ids keep the `name-{ch}`
/// scheme. The button noun follows the PRG banner (`Channel name saved.`);
/// `required` keeps an empty submit from reaching the no-op clear.
fn section_identity(state: &WebState, row: &crate::db::ChannelRow) -> String {
    let current = row.title.as_deref().map(str::trim);
    let current = current.filter(|t| !t.is_empty());
    let name_value = current.map(esc).unwrap_or_default();
    let name_id = esc(&format!("name-{}", row.channel));
    format!(
        r#"<article class="panel"><h2>Identity</h2><p class="help" id="identity-help">Your label for this channel — how it reads in lists and pickers. The dialed address is changed under Address.</p><form method="post" action="{action}" class="inline-form" aria-describedby="identity-help"><input type="hidden" name="channel" value="{ch}"><label for="{name_id}">Title</label><input id="{name_id}" name="name" value="{name_value}" placeholder="Friendly name, e.g. Wire Guard" required><button type="submit" class="btn btn-secondary">Save name</button></form></article>"#,
        action = state.url("/channels/name"),
        ch = esc(&row.channel),
        name_id = name_id,
        name_value = name_value,
    )
}

/// §5.4 Address: the dialed identifier as a read-only fact, with the
/// Change-address disclosure byte-verbatim from the old row controls
/// (`POST /channels/rename`, hidden `channel`, `addr-{ch}` input prefilled
/// with the current identifier, shape-branched warning, Retarget confirm —
/// hidden inputs and confirms keep quoting the RAW identifier, escaped).
fn section_address(state: &WebState, row: &crate::db::ChannelRow) -> String {
    let addr_id = esc(&format!("addr-{}", row.channel));
    let confirm_msg = esc(&format!(
        "Change where posts for {} go? This retargets every plan and post that uses it.",
        js_arg(&row.channel)
    ));
    // The warning branches on the identifier's shape at render — the same
    // `starts_with('@')` predicate the badge uses (CHANNEL_ADD_REDESIGN §5b).
    let addr_warning = if row.channel.starts_with('@') {
        "@handle changes on Telegram's side are not tracked here — update it here too."
    } else {
        "Private channels must keep their -100 ID. Changing this retargets where posts go."
    };
    format!(
        r#"<article class="panel"><h2>Address</h2><p>Posts dial <span class="mono">{ch}</span>.</p><details><summary>Change address…</summary><p class="help" id="address-help">{addr_warning}</p><form method="post" action="{action}" class="inline-form" aria-describedby="address-help"><input type="hidden" name="channel" value="{ch}"><label class="sr-only" for="{addr_id}">New address for {ch}</label><input id="{addr_id}" name="new" value="{ch}" required spellcheck="false"><button type="submit" class="btn btn-danger btn-sm" aria-label="Change the address of {ch}" onclick="return confirm('{confirm_msg}')">Retarget</button></form></details></article>"#,
        action = state.url("/channels/rename"),
        ch = esc(&row.channel),
        addr_id = addr_id,
        addr_warning = addr_warning,
        confirm_msg = confirm_msg,
    )
}

/// §5.5 Default: the Set-default form byte-verbatim from the old row cluster
/// (`POST /channels/default`, hidden `channel`, ghost submit, channel-named
/// aria-label); the default row states the fact and re-homes the old row
/// hint (`Set another channel as default to remove`), completed.
fn section_default(state: &WebState, row: &crate::db::ChannelRow) -> String {
    let inner = if row.is_default {
        r#"<p>This is the default channel.</p><span class="help">Set another channel as default to remove this one.</span>"#
            .to_string()
    } else {
        format!(
            r#"<p class="help" id="default-help">The default channel is the fallback posting target for plans and posts.</p><form method="post" action="{action}" class="inline-form" aria-describedby="default-help"><input type="hidden" name="channel" value="{ch}"><button type="submit" class="btn btn-ghost btn-sm" aria-label="Set {ch} as default">Set default</button></form>"#,
            action = state.url("/channels/default"),
            ch = esc(&row.channel),
        )
    };
    format!(r#"<article class="panel"><h2>Default</h2>{inner}</article>"#)
}

/// §5.6 Posting as: the bind forms byte-verbatim from the old row cluster
/// (`POST /channels/bind-sender`, `identity=bot|user`): the user-bound row
/// offers the safe revert with a channel-named aria-label and no confirm;
/// the bot-bound row offers the risky direction behind the confirm that
/// names the channel and the risk. The gate chain (Enable-posting → Ready
/// identity) and the audit writes stay in the handler.
fn section_posting(state: &WebState, row: &crate::db::ChannelRow, user_bound: bool) -> String {
    let inner = if user_bound {
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
    };
    format!(r#"<article class="panel"><h2>Posting as</h2>{inner}</article>"#)
}

/// §5.7 Danger zone: the Delete form byte-verbatim (`POST /channels/remove`,
/// `js_arg` confirm `Remove {ch}? This cannot be undone.`, channel-named
/// aria-label); default rows hide the button and state the protection fact in
/// the handler's own refusal copy, so the page explains before the POST
/// refuses. Last panel, visually flagged by the `.danger-zone` CSS rule.
fn section_danger(state: &WebState, row: &crate::db::ChannelRow) -> String {
    let inner = if row.is_default {
        r#"<p class="help">The default channel cannot be removed — set another channel as default first.</p>"#
            .to_string()
    } else {
        let confirm_msg = esc(&format!(
            "Remove {}? This cannot be undone.",
            js_arg(&row.channel)
        ));
        format!(
            "<form method=\"post\" action=\"{}\" class=\"inline-form\" aria-describedby=\"danger-desc\"><input type=\"hidden\" name=\"channel\" value=\"{}\"><button type=\"submit\" class=\"btn btn-danger\" aria-label=\"Delete {}\" onclick=\"return confirm('{}')\">Delete</button></form>",
            state.url("/channels/remove"),
            esc(&row.channel),
            esc(&row.channel),
            confirm_msg
        )
    };
    format!(
        r#"<article class="panel danger-zone"><h2>Danger zone</h2><p id="danger-desc">Removing disconnects every plan and post that dials this address. This cannot be undone.</p>{inner}</article>"#
    )
}

/// The admin detail body (spec §5.7): the same facts, read-only — Identity /
/// Address / Default / Posting as render as facts; no forms, no Danger zone
/// (admin is refused by every action route: default/remove/name/bind).
fn channels_manage_body_admin(state: &WebState, row: &crate::db::ChannelRow) -> String {
    let user_bound = state
        .db
        .get_channel_sender_identity(&row.channel)
        .as_deref()
        == Some("user");
    let default_fact = if row.is_default {
        "This is the default channel."
    } else {
        "Not the default channel."
    };
    let sender_fact = if user_bound {
        "Posts are sent as the owner's personal account."
    } else {
        "Posts are sent by the bot."
    };
    let title_fact = match row
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(t) => esc(t),
        None => "no title yet".to_string(),
    };
    format!(
        r#"<p><a href="{back}">Back to channels</a></p>
<h1>{label}</h1>
<p>{type_badge}{default_badge}{user_badge}</p>
<article class="panel"><h2>Identity</h2><p>{title_fact}</p></article>
<article class="panel"><h2>Address</h2><p>Posts dial <span class="mono">{ch}</span>.</p></article>
<article class="panel"><h2>Default</h2><p>{default_fact}</p></article>
<article class="panel"><h2>Posting as</h2><p>{sender_fact}</p></article>"#,
        back = state.url("/channels"),
        label = channel_label(row.title.as_deref(), &row.channel),
        type_badge = channel_type_badge(&row.channel),
        default_badge = if row.is_default {
            "<span class=\"badge badge-success\">default</span>"
        } else {
            ""
        },
        user_badge = if user_bound {
            "<span class=\"badge badge-warning\">as user</span>"
        } else {
            ""
        },
        title_fact = title_fact,
        ch = esc(&row.channel),
        default_fact = default_fact,
        sender_fact = sender_fact,
    )
}

/// The seller detail body (spec §7): header (label + `bot-managed` badge),
/// Identity and Address as read-only facts (a seller-typed title would be
/// silently dropped — add-spec §3.2), Default / Posting as hidden (owner-only
/// — silence is honest, these were never the seller's controls), and the
/// Danger-zone Delete byte-verbatim from the old seller row (`POST
/// /channels/remove`, `aria-label="Remove {ch}"`). Cross-scope X is refused
/// before this body ever renders (§5.2).
fn channels_manage_body_seller(
    state: &WebState,
    session: &Session,
    ch: &str,
    q: &ChannelsQuery,
) -> String {
    let banners = channels_banners(q, state, session).join("\n");
    let Some(sid) = session.seller_id else {
        return channels_manage_refusal(state, "That channel isn't yours to manage.");
    };
    // Same label contract as the seller rows (CHANNELS_TITLE_UX_SPEC §2):
    // the title is read from the channels table, case-insensitively.
    let title = state
        .db
        .list_channels(Some(sid))
        .into_iter()
        .find(|row| row.channel.eq_ignore_ascii_case(ch))
        .and_then(|row| row.title);
    let label = channel_label(title.as_deref(), ch);
    let identity_fact = match title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        Some(t) => format!("<p>{}</p>", esc(t)),
        None => "<p class=\"help\">no title stored for this channel yet</p>".to_string(),
    };
    let confirm_msg = esc(&format!("Remove {}? This cannot be undone.", js_arg(ch)));
    let danger = format!(
        r#"<article class="panel danger-zone"><h2>Danger zone</h2><p>Removing disconnects every plan and post that dials this address. This cannot be undone.</p><form method="post" action="{action}" class="inline-form"><input type="hidden" name="channel" value="{ch}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Remove {ch}" onclick="return confirm('{confirm_msg}')">Delete</button></form></article>"#,
        action = state.url("/channels/remove"),
        ch = esc(ch),
        confirm_msg = confirm_msg,
    );
    format!(
        r#"<p><a href="{back}">Back to channels</a></p>
<h1>{label}</h1>
<p><span class="badge badge-success">bot-managed</span></p>
{banners}
<article class="panel"><h2>Identity</h2>{identity_fact}</article>
<article class="panel"><h2>Address</h2><p>Posts dial <span class="mono">{ch_esc}</span>.</p></article>
{danger}"#,
        back = state.url("/channels"),
        label = label,
        banners = banners,
        identity_fact = identity_fact,
        ch_esc = esc(ch),
        danger = danger,
    )
}
