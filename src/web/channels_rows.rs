//! Channel row, label, and badge rendering — the row-rendering family
//! extracted move-only from `channels.rs` (CHANNEL_ADD_REDESIGN_SPEC §2;
//! at the source line cap the next change splits first).
//! CHANNELS_MASTER_DETAIL_SPEC §4: rows are read-first — label cluster +
//! state badges + exactly ONE affordance (`Manage →`); every edit control
//! moved to the detail view (`channels_manage.rs`). The pinned row assertions
//! in `src/web/tests/channels.rs` are the oracle.
use super::*;

/// One row of the master channels list (CHANNELS_MASTER_DETAIL_SPEC §4.2).
/// The DOM skeleton is unchanged (`li > span.mono + badges + .row-actions`)
/// so the flex row styles and the `.row-added` tint keep working unchanged;
/// badges are STATE, not affordances (the exception-only `as user` marking
/// stays on the row). `focus` is the add redirect's `?channel=` anchor
/// (present only when `?added=1`): a NOCASE match renders the post-add
/// reveal row (§4) — highlighted, `tabindex="-1"`, focused by APP_JS on load.
pub(super) fn render_global_channel_row(
    state: &WebState,
    is_owner: bool,
    row: crate::db::ChannelRow,
    focus: Option<&str>,
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
    // Show the human title (when known) alongside the canonical id so a title
    // and its id are never mistaken for two different channels (CHANNELS_TITLE_UX_SPEC §2).
    let label = channel_label(row.title.as_deref(), &row.channel);
    // The hint points at the detail view, where naming now lives; it stays
    // owner-only because an admin cannot name a channel (F-2 role honesty) —
    // an admin reading a titleless global row sees the bare id, not a dead
    // pointer.
    let notitle_hint = if is_owner && row.title.as_deref().is_none_or(|t| t.trim().is_empty()) {
        r#"<span class="help">no title yet — open Manage to name it</span>"#
    } else {
        ""
    };
    // The one affordance (§4.2): `Manage →` with a per-channel accessible
    // name — the stored title when present ("Manage Wire Guard"), else the
    // identifier ("Manage @wire"). The visible text is `Manage` plus an
    // aria-hidden arrow; the arrow rides the `.btn` flex gap (no new CSS).
    let manage_name = row
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or(&row.channel);
    let manage = format!(
        r#"<div class="row-actions"><a class="btn btn-secondary btn-sm" href="{href}" aria-label="Manage {name}">Manage <span aria-hidden="true">→</span></a></div>"#,
        href = state.url(&format!(
            "/channels/manage?channel={}",
            urlencode_query(&row.channel)
        )),
        name = esc(manage_name),
    );
    let li_open = if focus.is_some_and(|c| c.eq_ignore_ascii_case(&row.channel)) {
        r#"<li class="row-added" tabindex="-1" data-focus-on-load>"#
    } else {
        "<li>"
    };
    format!(
        "{li_open}<span class=\"mono\">{}</span> {}{badge}{user_badge}{notitle_hint}{manage}</li>",
        label,
        type_badge,
        badge = badge,
        user_badge = user_badge,
        notitle_hint = notitle_hint,
        manage = manage,
    )
}

/// The post-add callout (CHANNEL_ADD_REDESIGN_SPEC §4.3): one outcome, spoken
/// once, rendered inside the panel immediately after the row list. `{name}` is
/// prose — the stored title when one exists, else the identifier — escaped
/// here. The notitle variant replaces the "couldn't fetch" sentence instead of
/// stacking it, and the Set-default clause renders only for owners
/// (admins/sellers have no Set default control — role honesty, §4.3). The
/// owner tails re-point at the detail view, where those controls now live
/// (CHANNELS_MASTER_DETAIL_SPEC §4.4).
pub(super) fn channel_added_callout(name: &str, notitle: bool, is_owner: bool) -> String {
    let fetch = if notitle {
        " I couldn't fetch its name from Telegram."
    } else {
        ""
    };
    let tail = if is_owner {
        " — add it in Telegram, then open Manage and set it default if this is your main channel."
    } else {
        " — add it in Telegram."
    };
    let label_hint = if notitle && is_owner {
        " Open Manage to give it a label."
    } else {
        ""
    };
    format!(
        "<div class=\"alert alert-info\" role=\"status\">{} is in.{fetch} The bot must be an admin in it before anything posts{tail}{label_hint}</div>",
        esc(name)
    )
}

/// The badge derives from the STORED identifier's shape at render: `@` →
/// public, anything else → private. There is no visibility column (no DB
/// changes) — the badge stays render-derived (CHANNEL_ADD_REDESIGN_SPEC §6).
/// Residual edge (§6, documented, no action): rows created before the public
/// add path stored `@handle` (§3.4.4) may hold a numeric id for a genuinely
/// public channel and read private until retouched via Change address (§5b)
/// or re-added.
pub(super) fn channel_type_badge(ch: &str) -> &'static str {
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

/// The add redirect's row-focus anchor (CHANNEL_ADD_REDESIGN_SPEC §4):
/// `?channel=` only when `added=1`.
pub(super) fn added_focus(q: Option<&ChannelsQuery>) -> Option<&str> {
    q.filter(|q| q.added.as_deref() == Some("1"))
        .and_then(|q| q.channel.as_deref())
}

/// §4.3: the in-panel post-add callout body — rendered for every anchored add
/// (`?added=1&channel=…`), even when the row itself is filtered out by a
/// bot-scope selection, so the outcome message never disappears. The name is
/// the stored title when one exists, else the identifier.
pub(super) fn channels_added_callout(
    state: &WebState,
    session: &Session,
    q: &ChannelsQuery,
) -> String {
    if q.added.as_deref() != Some("1") {
        return String::new();
    }
    let Some(ch) = q
        .channel
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    else {
        return String::new();
    };
    let title = state
        .db
        .list_channels(None)
        .into_iter()
        .find(|row| row.channel.eq_ignore_ascii_case(ch))
        .and_then(|row| row.title)
        .filter(|t| !t.trim().is_empty());
    channel_added_callout(
        title.as_deref().unwrap_or(ch),
        q.notitle.as_deref() == Some("1"),
        Role::from_role_str(&session.role).has_level(Role::Owner),
    )
}

/// The seller's master view (CHANNELS_MASTER_DETAIL_SPEC §7): page-head with
/// the one `Add channel` affordance (for a seller add and replace are the
/// same act — the form lives on `GET /channels/add`, seller variant), the
/// single read-first row (label + type badge + `bot-managed` badge + the
/// notitle hint + `Manage →`), the honest empty state, and the post-add
/// reveal (§4.2). §2.3 overflow valve: row rendering by the same definition.
pub(super) fn render_seller_channels(
    state: &WebState,
    session: &Session,
    alert: &String,
    success: &String,
    q: Option<&ChannelsQuery>,
) -> String {
    let sid = session.seller_id.unwrap_or(0);
    let focus = added_focus(q);
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
            // The one affordance (§4.2/§7), named per §4.2: the stored title
            // when present, else the identifier.
            let manage_name = title
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or(&ch);
            let manage = format!(
                r#"<div class="row-actions"><a class="btn btn-secondary btn-sm" href="{href}" aria-label="Manage {name}">Manage <span aria-hidden="true">→</span></a></div>"#,
                href = state.url(&format!(
                    "/channels/manage?channel={}",
                    urlencode_query(&ch)
                )),
                name = esc(manage_name),
            );
            // §4.2: the just-added seller row highlights and takes focus.
            let li_open = if focus.is_some_and(|c| c.eq_ignore_ascii_case(&ch)) {
                r#"<li class="row-added" tabindex="-1" data-focus-on-load>"#
            } else {
                "<li>"
            };
            format!(
                "{li_open}<span class=\"mono\">{}</span> {}<span class=\"badge badge-success\">bot-managed</span>{}{manage}</li>",
                label,
                type_badge,
                notitle_hint,
                manage = manage,
            )
        }
        None => "<li class=\"empty\"><b>No channel yet.</b> Add one, or use /setchannel in the bot.</li>"
            .to_string(),
    };
    format!(
        r#"{alert}{success}<div class="page-head"><h1>Channels <span class="sub">your posting target</span></h1><div class="actions"><a class="btn" href="{add_link}">Add channel</a></div></div>
<article class="panel"><h3>Your channel</h3><ul>{list}</ul>
{callout}
</article>"#,
        add_link = state.url("/channels/add"),
        alert = alert,
        success = success,
        list = list,
        callout = q
            .map(|q| channels_added_callout(state, session, q))
            .unwrap_or_default(),
    )
}
