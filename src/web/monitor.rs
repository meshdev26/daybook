use super::*;
// ---- Monitor (read-only timeline) ---------------------------------------------

#[derive(Deserialize)]
pub(super) struct MonitorQuery {
    pub(super) ch: Option<String>,
    pub(super) q: Option<String>,
    pub(super) before: Option<String>,
}

/// What the timeline panel shows. `NotReady` renders the state panels (§5);
/// the fetch result is passed in pre-computed so the body stays a pure,
/// testable function.
pub(super) enum TimelineView {
    NotReady,
    Prompt,
    Results(Result<Vec<NormalizedMessage>, IdentityError>, bool),
}

pub(super) const MONITOR_HISTORY_LIMIT: usize = 100;
pub(super) const MONITOR_SEARCH_LIMIT: usize = 50;

#[tracing::instrument(skip_all)]
pub(super) async fn monitor_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<MonitorQuery>,
) -> Response {
    let session = match identity_owner_session(&state, &headers, "Monitor") {
        Ok(s) => s,
        Err(resp) => return resp,
    };
    let status = state.identity.status().await;
    let ch = q.ch.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let query = q.q.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let before = q.before.as_deref().and_then(|s| s.parse::<i64>().ok());
    // Locked = no fetch: the data fetch sits behind the same gate as every
    // identity action (IDENTITY_MONITOR_UX_SPEC §7.3) — the state panel is
    // the whole content.
    let view = if status.state != IdentityState::Ready {
        TimelineView::NotReady
    } else if let Some(query) = query {
        TimelineView::Results(
            state.identity.search(query, ch, MONITOR_SEARCH_LIMIT).await,
            true,
        )
    } else if let Some(c) = ch {
        TimelineView::Results(
            state
                .identity
                .history(c, before, MONITOR_HISTORY_LIMIT)
                .await,
            false,
        )
    } else {
        TimelineView::Prompt
    };
    let body = monitor_body(&state, &status, ch, query, before, &view);
    Html(html_page(&state, Some(&session), "Monitor", &body)).into_response()
}

/// The exactly-two GET forms (IDENTITY_MONITOR_UX_SPEC §2.1). All state on
/// this page lives in the URL — shareable, refreshable, back-button honest.
pub(super) fn monitor_controls(
    base: &str,
    channels: &[(String, Option<String>)],
    selected: Option<&str>,
    q: Option<&str>,
) -> String {
    let mut options = String::new();
    if channels.is_empty() {
        options.push_str(r#"<option value="">No channels yet</option>"#);
    } else {
        for (c, title) in channels {
            let sel = if selected == Some(c.as_str()) {
                " selected"
            } else {
                ""
            };
            options.push_str(&format!(
                r#"<option value="{v}"{sel}>{label}</option>"#,
                v = esc(c),
                label = channel_label(title.as_deref(), c)
            ));
        }
    }
    format!(
        r#"<div class="form-row">
<form method="get" action="{base}/monitor" class="inline-form">
<label for="monitor-channel" class="sr-only">Channel</label>
<select id="monitor-channel" name="ch">{options}</select>
<button type="submit" class="btn btn-secondary btn-sm">View</button>
</form>
<form method="get" action="{base}/monitor" class="inline-form">
<label for="monitor-search" class="sr-only">Search messages</label>
<input type="search" id="monitor-search" name="q" placeholder="Search messages" value="{qv}">
<button type="submit" class="btn btn-secondary btn-sm">Search</button>
</form>
</div>"#,
        base = base,
        options = options,
        qv = esc(q.unwrap_or("")),
    )
}

/// One timeline entry: mono `{date} · {sender}` header above the esc'd body.
/// Empty sender falls back to `user {id}`; empty text is a one-line media
/// placeholder (the monitor reads conversation, never attachments).
pub(super) fn timeline_entry(m: &NormalizedMessage) -> String {
    let sender = if m.sender.trim().is_empty() {
        format!("user {}", m.id)
    } else {
        m.sender.clone()
    };
    let body = if m.text.trim().is_empty() {
        "[media]".to_string()
    } else {
        esc(&m.text)
    };
    format!(
        r#"<li><div class="mono">{} · {}</div><p>{}</p>"#,
        esc(&m.date),
        esc(&sender),
        body
    )
}

/// The semantic `<ol>` timeline (IDENTITY_MONITOR_UX_SPEC §3.1): newest
/// first, replies nested exactly one level. Replies whose parent was clipped
/// off this page render top-level — a reply never disappears, and no deeper
/// tree is ever built. Pure fn: tested for nesting and escaping directly.
pub(super) fn render_timeline(messages: &[NormalizedMessage], label: &str) -> String {
    use std::collections::HashMap;
    let mut by_id: HashMap<i64, ()> = HashMap::new();
    for m in messages {
        by_id.entry(m.id).or_insert(());
    }
    let mut top: Vec<&NormalizedMessage> = Vec::new();
    let mut children: HashMap<i64, Vec<&NormalizedMessage>> = HashMap::new();
    for m in messages {
        match m.reply_to {
            // A parent on this page groups the reply under it (self-replies
            // stay top-level so nothing can vanish into its own subtree).
            // Front-insert keeps the sub-list in thread order even though
            // the page itself is newest-first.
            Some(p) if p != m.id && by_id.contains_key(&p) => {
                children.entry(p).or_default().insert(0, m);
            }
            _ => top.push(m),
        }
    }
    let mut out = format!(r#"<ol class="timeline" aria-label="{}">"#, esc(label));
    for m in top {
        out.push_str(&timeline_entry(m));
        if let Some(kids) = children.get(&m.id) {
            out.push_str(r#"<ol aria-label="Replies">"#);
            for k in kids {
                out.push_str(&timeline_entry(k));
                out.push_str("</li>");
            }
            out.push_str("</ol>");
        }
        out.push_str("</li>");
    }
    out.push_str("</ol>");
    out
}

/// Render the Results/Ok timeline article (extracted to keep monitor_body
/// short). Behavior-preserving: identical HTML for every input.
pub(super) fn monitor_results_html(
    state: &WebState,
    ch: Option<&str>,
    before: Option<i64>,
    msgs: &[NormalizedMessage],
    is_search: bool,
) -> String {
    let (h3, label) = if is_search {
        (
            "Search results".to_string(),
            "Search results, newest first".to_string(),
        )
    } else {
        let c = ch.unwrap_or("");
        (
            format!("{} · latest messages", esc(c)),
            format!("Messages from {}, newest first", esc(c)),
        )
    };
    let mut out = format!(r#"<article class="panel"><h3>{h3}</h3>"#,);
    if is_search {
        let scope = match ch {
            Some(c) => format!(
                "Matches in {} · newest first · latest {}.",
                esc(c),
                MONITOR_SEARCH_LIMIT
            ),
            None => format!(
                "Matches in all channels · newest first · latest {}.",
                MONITOR_SEARCH_LIMIT
            ),
        };
        out.push_str(&format!(r#"<p class="help">{scope}</p>"#));
    }
    if msgs.is_empty() {
        if is_search {
            out.push_str(&format!(
                r#"<p class="empty"><b>No matches.</b> Try fewer words, or search all channels.</p><p><a href="{}/monitor">Clear search</a></p></article>"#,
                state.base_path
            ));
        } else {
            out.push_str(
                r#"<p class="empty"><b>No messages.</b> Nothing to show in this channel yet.</p></article>"#,
            );
        }
    } else {
        out.push_str(&render_timeline(msgs, &label));
        if is_search {
            // Search has no cursor in the read API — no older link.
        } else if msgs.len() >= MONITOR_HISTORY_LIMIT {
            let oldest = msgs.last().map(|m| m.id).unwrap_or_default();
            let chq = ch.map(qenc).unwrap_or_default();
            out.push_str(&format!(
                r#"<p class="help">Showing the latest {} messages.</p><p><a href="{}/monitor?ch={}&before={}">Older messages</a></p></article>"#,
                MONITOR_HISTORY_LIMIT,
                state.base_path,
                chq,
                oldest
            ));
        } else {
            let _ = before;
            out.push_str(r#"<p class="help">That's the oldest message available.</p></article>"#);
        }
    }
    out
}

/// The /monitor body. `state` supplies the console channel list (membership
/// as the console knows it — the picker renders a single empty option when
/// there is none, per §2.1).
pub(super) fn monitor_body(
    state: &WebState,
    status: &IdentityStatus,
    ch: Option<&str>,
    q: Option<&str>,
    before: Option<i64>,
    view: &TimelineView,
) -> String {
    let mut body =
        r#"<h1>Monitor <span class="sub">read-only · viewing as your personal account</span></h1>"#
            .to_string();
    match status.state {
        IdentityState::Locked => {
            body.push_str(
                r#"<article class="panel"><p class="empty"><b>Your identity is locked.</b> Unlock it in the banner above to read as your personal account.</p></article>"#,
            );
            return body;
        }
        IdentityState::Disabled => {
            body.push_str(&format!(
                r#"<article class="panel"><p class="empty"><b>Your identity is disabled.</b> Telegram reported its authorization key is used elsewhere — re-pair it to read as yourself.</p><p><a href="{}">See the Identity page.</a></p></article>"#,
                state.url("/identity")
            ));
            return body;
        }
        // Everything that is not Ready renders its state panel and fetches
        // nothing (§5); Ready falls through to the timeline below.
        IdentityState::Ready => {}
        _ => {
            body.push_str(&format!(
                r#"<article class="panel"><p class="empty"><b>No personal identity connected.</b> Connect one on the Identity page to read as yourself.</p><p><a href="{}">Go to Identity.</a></p></article>"#,
                state.url("/identity")
            ));
            return body;
        }
    }
    let channels: Vec<(String, Option<String>)> = state
        .db
        .list_channels(None)
        .iter()
        .map(|c| (c.channel.clone(), c.title.clone()))
        .collect();
    body.push_str(&monitor_controls(&state.base_path, &channels, ch, q));
    match view {
        TimelineView::NotReady => {}
        TimelineView::Prompt => {
            if channels.is_empty() {
                body.push_str(
                    r#"<article class="panel"><p class="empty"><b>No channels to read yet.</b> Channels your personal account joins appear here.</p></article>"#,
                );
            } else {
                body.push_str(
                    r#"<article class="panel"><p class="help">Pick a channel to view its latest messages, or search across them.</p></article>"#,
                );
            }
        }
        TimelineView::Results(result, is_search) => match result {
            Err(e) => {
                // Unknown ch is a warn, not an error (§2.1); anything else
                // renders its typed copy.
                let alert = match e {
                    IdentityError::PeerNotFound(_) => {
                        r#"<div class="alert alert-warn" role="alert">That channel isn't available to your account.</div>"#.to_string()
                    }
                    other => identity_error_html(other),
                };
                body.push_str(&format!(r#"<article class="panel">{alert}</article>"#));
            }
            Ok(msgs) => {
                body.push_str(&monitor_results_html(state, ch, before, msgs, *is_search));
            }
        },
    }
    body
}
