use super::*;

#[derive(Deserialize)]
pub(super) struct SubsRevokeForm {
    #[serde(default)]
    pub(super) id: Option<i64>,
    #[serde(default)]
    pub(super) sub_id: Option<i64>,
    /// Buyer + channel for the post-revoke kick. Both are server-rendered
    /// from the subscription row at page time (subs_body); the handler is
    /// owner-only and the kick is the same ban+unban the REST refund
    /// processing (finance_refunds_process) and subs_expirecheck perform.
    #[serde(default)]
    pub(super) user_id: Option<i64>,
    #[serde(default)]
    pub(super) channel: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct SubsQuery {
    pub(super) status: Option<String>,
    pub(super) expired: Option<String>,
    pub(super) revoked: Option<String>,
    pub(super) granted: Option<String>,
    pub(super) extended: Option<String>,
    pub(super) bulkrevoked: Option<String>,
    pub(super) warn: Option<String>,
    pub(super) failed: Option<String>,
    pub(super) partial: Option<String>,
    pub(super) error: Option<String>,
    pub(super) id: Option<String>,
    pub(super) page: Option<String>,
    pub(super) q: Option<String>,
    /// /orders outcome flag: the status/tracking handlers redirect with
    /// ?saved=1 (the query struct is shared with /subs, which ignores it).
    pub(super) saved: Option<String>,
    /// /codes delete flag (EDIT_FLOW_SPEC E5): the codes page shares this
    /// struct and renders "Code deleted." from it; /subs ignores it.
    pub(super) deleted: Option<String>,
    /// /posts outcome flag (M2): a post edit held by the outbound gate
    /// redirects here. Content-free feedback only — never the held text.
    pub(super) held: Option<String>,
}

pub(super) fn normalize_sub_status(s: Option<String>) -> String {
    match s.as_deref() {
        Some("all") | Some("active") | Some("expired") | Some("revoked") | Some("refunded") => {
            // Matched-Some: the default is unreachable — kept unwrap-free per
            // the no-unwrap production rule (F-3).
            s.unwrap_or_default()
        }
        _ => "all".to_string(),
    }
}

fn subs_alert_banner(q: &SubsQuery) -> Option<String> {
    let mut banners: Vec<String> = Vec::new();
    if let Some(nstr) = q.expired.as_ref() {
        let n: i64 = nstr.parse().unwrap_or(0);
        if n == 0 {
            banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite">No subscriptions were due for expiry.</div>"#.to_string());
        } else {
            banners.push(format!(
                r#"<div class="alert alert-success" role="status" aria-live="polite">Marked {} subscription(s) expired.</div>"#,
                n
            ));
        }
    }
    if q.revoked.as_deref() == Some("1") {
        banners.push(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Subscription revoked.</div>"#.to_string(),
        );
    }
    if q.granted.as_deref() == Some("1") {
        banners.push(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Subscription granted.</div>"#.to_string(),
        );
    }
    if q.extended.as_deref() == Some("1") {
        banners.push(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Subscription extended.</div>"#.to_string(),
        );
    }
    if let Some(nstr) = q.bulkrevoked.as_ref() {
        let n: usize = nstr.parse().unwrap_or(0);
        banners.push(format!(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Revoked {} subscription(s).</div>"#,
            n
        ));
    }
    // Kick warnings: the status flip succeeded but Telegram refused the
    // ban+unban, so the buyer may still be in the channel. Never silent.
    if q.warn.as_deref() == Some("kick") {
        let failed: usize = q
            .failed
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1);
        banners.push(format!(
            r#"<div class="alert alert-warn" role="alert" aria-live="polite">Channel kick failed for {} subscription(s) — the buyer may still be in the channel. Check the bot's admin rights there and remove access manually.</div>"#,
            failed
        ));
    }
    if q.partial.as_deref() == Some("1") {
        banners.push(
            r#"<div class="alert alert-warn" role="alert" aria-live="polite">Only the first 1000 matching subscriptions were processed for the kick.</div>"#.to_string(),
        );
    }
    if let Some(err) = q.error.as_ref() {
        let msg = match err.as_str() {
            "invalid" => "Invalid subscription id.".to_string(),
            "forbidden" => {
                "Subscriptions management is restricted to the console owner.".to_string()
            }
            "revoke" => {
                let id = q.id.clone().unwrap_or_default();
                format!("Could not revoke subscription {}.", esc(&id))
            }
            "notfound" => "Subscription not found.".to_string(),
            _ => String::new(),
        };
        if !msg.is_empty() {
            banners.push(format!(
                r#"<div class="alert alert-error" role="alert" aria-live="polite">{}</div>"#,
                msg
            ));
        }
    }
    if banners.is_empty() {
        None
    } else {
        Some(banners.join("\n"))
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn subs_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let status_filter = normalize_sub_status(q.status.clone());
    let page: i64 = q
        .page
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1)
        .max(1);
    let search_q = q.q.clone().unwrap_or_default();

    let alert_banner = subs_alert_banner(&q);

    Html(html_page(
        &state,
        Some(&session),
        "Subscriptions",
        &subs_body(
            &state,
            &session,
            &status_filter,
            alert_banner.as_deref(),
            page,
            &search_q,
        ),
    ))
    .into_response()
}

fn subs_count_cards(active: i64, expired: i64, revoked: i64, refunded: i64, total: i64) -> String {
    format!(
        r#"<section aria-labelledby="subs-counts-heading"><h2 id="subs-counts-heading">Status counts</h2><div class="grid-stats">
<div class="panel"><div class="stat"><span class="stat-label">Active</span><span class="stat-value">{active}</span></div></div>
<div class="panel"><div class="stat"><span class="stat-label">Expired</span><span class="stat-value">{expired}</span></div></div>
<div class="panel"><div class="stat"><span class="stat-label">Revoked</span><span class="stat-value">{revoked}</span></div></div>
<div class="panel"><div class="stat"><span class="stat-label">Refunded</span><span class="stat-value">{refunded}</span></div></div>
<div class="panel"><div class="stat"><span class="stat-label">Total</span><span class="stat-value">{total}</span></div></div>
</div></section>"#,
        active = active,
        expired = expired,
        revoked = revoked,
        refunded = refunded,
        total = total
    )
}

fn subs_filter_nav(state: &WebState, filter: &str, search_q: &str) -> String {
    let statuses = ["all", "active", "expired", "revoked", "refunded"];
    let qparam = if search_q.trim().is_empty() {
        String::new()
    } else {
        format!("&q={}", qenc(search_q.trim()))
    };
    let filter_links = statuses
        .iter()
        .map(|s| {
            let cls = if *s == filter {
                "btn btn-secondary btn-sm"
            } else {
                "btn btn-ghost btn-sm"
            };
            let aria = if *s == filter {
                r#" aria-current="page""#
            } else {
                ""
            };
            format!(
                r#"<a class="{}" href="{}"{}>{}</a>"#,
                cls,
                state.url(&format!("/subs?status={}{}", s, qparam)),
                aria,
                s
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        r#"<nav aria-label="Filter subscriptions by status" class="inline-form">{}</nav>"#,
        filter_links
    )
}

fn subs_table_html(
    state: &WebState,
    _session: &Session,
    is_owner: bool,
    rows: &[(i64, i64, i64, String, String, String)],
    filter: &str,
) -> String {
    let col_count = if is_owner { 7 } else { 5 };
    let titles = channel_titles(state);
    let header = if is_owner {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">Seller</th><th scope="col">User</th><th scope="col">Channel</th><th scope="col">Status</th><th scope="col">Expires</th><th scope="col">Actions</th></tr>"#
    } else {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">User</th><th scope="col">Channel</th><th scope="col">Status</th><th scope="col">Expires</th></tr>"#
    };

    let rows_html = if rows.is_empty() {
        if filter != "all" {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No {} subscriptions.</b> <a href="{}">Clear filter</a></td></tr>"#,
                col_count,
                esc(filter),
                state.url("/subs")
            )
        } else {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No subscriptions yet.</b> They appear when a buyer pays or when you grant access with /grant in the bot.</td></tr>"#,
                col_count
            )
        }
    } else {
        rows
            .iter()
            .map(|r| {
                let status_badge = match r.4.as_str() {
                    "active" => "badge badge-success",
                    "revoked" | "expired" => "badge badge-danger",
                    _ => "badge",
                };
                let status_cell = format!(
                    r#"<span class="{}">{}</span>"#,
                    status_badge,
                    esc(&r.4)
                );
                let id_cell = format!(r#"<td class="num">{}</td>"#, r.0);
                let user_cell = format!(r#"<td class="mono">{}</td>"#, esc(&r.2.to_string()));
                let channel_cell = format!(
                    r#"<td class="mono">{}</td>"#,
                    channel_label(titles.get(&r.3.to_ascii_lowercase()).map(String::as_str), &r.3)
                );
                let expires_cell = format!(r#"<td class="mono">{}</td>"#, esc(&r.5));
                if is_owner {
                    let seller_cell = format!(r#"<td>{}</td>"#, r.1);
                    // The kick data (buyer + channel) rides along as
                    // server-rendered hidden fields so subs_revoke can
                    // ban+unban without a second lookup.
                    let revoke_form = format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="sub_id" value="{}"><input type="hidden" name="user_id" value="{}"><input type="hidden" name="channel" value="{}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Revoke subscription {}" onclick="return confirm('Revoke subscription #{}? The buyer loses access; this cannot be undone from the console.')">Revoke</button></form>"#,
                        state.url("/subs/revoke"),
                        r.0,
                        r.2,
                        esc(&r.3),
                        r.0,
                        r.0
                    );
                    // Extend: the handler takes sub_id + days (clamped
                    // 1..=3650). C4: secondary weight — the row's primary
                    // action space belongs to nothing here; Extend mutates
                    // access like finance Approve, not like a fresh grant.
                    let extend_form = format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="sub_id" value="{}"><input type="number" name="days" value="30" min="1" max="3650" aria-label="Days to extend subscription {}" class="input-num"><button type="submit" class="btn btn-secondary btn-sm" onclick="return confirm('Extend subscription #{} by the given days?')">Extend</button></form>"#,
                        state.url("/subs/extend"),
                        r.0,
                        r.0,
                        r.0
                    );
                    let actions_cell =
                        format!(r#"<td class="actions">{}{}</td>"#, revoke_form, extend_form);
                    format!(
                        "<tr>{}{}{}{}<td>{}</td>{}{}</tr>",
                        id_cell, seller_cell, user_cell, channel_cell, status_cell, expires_cell, actions_cell
                    )
                } else {
                    format!(
                        "<tr>{}{}{}<td>{}</td>{}</tr>",
                        id_cell, user_cell, channel_cell, status_cell, expires_cell
                    )
                }
            })
            .collect::<Vec<_>>()
            .join("")
    };

    format!(
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Subscriptions"><table aria-label="Subscriptions"><caption class="sr-only">Subscriptions</caption><thead>{}</thead><tbody>{}</tbody></table></div>"#,
        header, rows_html
    )
}

fn subs_search_form(state: &WebState, filter: &str, search_q: &str) -> String {
    format!(
        r#"<form method="get" action="{}" class="inline-form">
<input type="text" name="q" value="{}" placeholder="user id or channel" aria-label="Search subscriptions">
<input type="hidden" name="status" value="{}">
<button type="submit" class="btn btn-secondary btn-sm">Search</button>
<a class="btn btn-ghost btn-sm" href="{}">Export CSV</a>
</form>"#,
        state.url("/subs"),
        esc(search_q),
        esc(filter),
        state.url(&format!(
            "/subs/export?status={}&q={}",
            filter,
            qenc(search_q.trim())
        ))
    )
}

fn subs_pagination(
    state: &WebState,
    filter: &str,
    search_q: &str,
    total_filtered: i64,
    page: i64,
) -> String {
    let total_pages = if total_filtered == 0 {
        1
    } else {
        (total_filtered + 199) / 200
    };
    let page = page.max(1).min(total_pages.max(1));
    let mut pag = String::new();
    if total_pages > 1 {
        let prev = if page > 1 {
            format!(
                r#"<a class="btn btn-ghost btn-sm" href="{}">Prev</a>"#,
                state.url(&format!(
                    "/subs?status={}&q={}&page={}",
                    filter,
                    qenc(search_q.trim()),
                    page - 1
                ))
            )
        } else {
            String::new()
        };
        let next = if page < total_pages {
            format!(
                r#"<a class="btn btn-ghost btn-sm" href="{}">Next</a>"#,
                state.url(&format!(
                    "/subs?status={}&q={}&page={}",
                    filter,
                    qenc(search_q.trim()),
                    page + 1
                ))
            )
        } else {
            String::new()
        };
        pag.push_str(&format!(
            r#"<nav class="inline-form" aria-label="Subscriptions pagination">{prev}<span class="help">Page {page} of {total_pages}</span>{next}</nav>"#
        ));
    }
    pag
}

fn subs_grant_form(state: &WebState) -> String {
    // C3 (EDIT_FLOW_SPEC): visible <label for> like every other create form
    // on the console — the aria-label-only inputs are gone.
    format!(
        r#"<article class="panel"><h3>Grant subscription</h3>
<form method="post" action="{}" class="inline-form">
<label for="g-user">Telegram user id</label><input id="g-user" type="number" name="user_id" placeholder="user id" required>
<label for="g-days">Days</label><input id="g-days" type="number" name="days" placeholder="days" value="30" required>
<label for="g-channel">Channel</label><input id="g-channel" type="text" name="channel" placeholder="@channel" required>
<button type="submit">Grant</button>
</form></article>"#,
        state.url("/subs/grant")
    )
}

fn subs_load_counts(state: &WebState, seller_scope: Option<i64>) -> (i64, i64, i64, i64, i64) {
    let counts = state.db.subscription_status_counts(seller_scope);
    let mut active: i64 = 0;
    let mut expired: i64 = 0;
    let mut revoked: i64 = 0;
    let mut refunded: i64 = 0;
    for (st, c) in counts {
        match st.as_str() {
            "active" => active = c,
            "expired" => expired = c,
            "revoked" => revoked = c,
            "refunded" => refunded = c,
            _ => {}
        }
    }
    let total = state.db.count_subscriptions(seller_scope, None);
    (active, expired, revoked, refunded, total)
}

pub(super) fn subs_body(
    state: &WebState,
    session: &Session,
    filter: &str,
    alert: Option<&str>,
    page: i64,
    search_q: &str,
) -> String {
    let alert = alert.unwrap_or_default();
    let is_owner = Role::from_role_str(&session.role).has_level(Role::Owner);
    let seller_scope = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id
    } else {
        None
    };

    let status_opt = if filter == "all" { None } else { Some(filter) };
    let search_opt = if search_q.trim().is_empty() {
        None
    } else {
        Some(search_q.trim())
    };
    let offset = (page.max(1) - 1) * 200;
    let rows = state.db.list_subscriptions_filtered_paged(
        seller_scope,
        status_opt,
        search_opt,
        200,
        offset,
    );
    let total_filtered =
        state
            .db
            .count_subscriptions_filtered(seller_scope, status_opt, search_opt);
    let (active, expired, revoked, refunded, total) = subs_load_counts(state, seller_scope);

    let cards = subs_count_cards(active, expired, revoked, refunded, total);
    let filter_nav = subs_filter_nav(state, filter, search_q);
    let expire_check = if is_owner {
        format!(
            r#"<form method="post" action="{}" class="inline-form"><button type="submit" class="btn btn-secondary btn-sm">Run expire check</button></form>"#,
            state.url(&format!("/subs/expirecheck?status={}", filter))
        )
    } else {
        String::new()
    };
    let table = subs_table_html(state, session, is_owner, &rows, filter);

    let scoping_note = if Role::from_role_str(&session.role) == Role::Seller {
        "Showing your subscriptions only."
    } else {
        "Showing all sellers' subscriptions."
    };
    let row_note = if total_filtered > 200 {
        format!(
            " Showing {} of {} subscriptions.",
            rows.len(),
            total_filtered
        )
    } else {
        String::new()
    };
    let search_form = subs_search_form(state, filter, search_q);
    let pag = subs_pagination(state, filter, search_q, total_filtered, page);
    let grant_form = subs_grant_form(state);
    let footer = format!(r#"<p class="help">{}{}</p>"#, scoping_note, row_note);

    format!(
        r#"{alert}<h1>Subscriptions <span class="sub">buyer access and expiry</span></h1>
{cards}
<div class="inline-form">{filter_nav}{expire_check}</div>
{search_form}
{grant_form}
{table}
{pag}
{footer}"#,
        alert = alert,
        cards = cards,
        filter_nav = filter_nav,
        expire_check = expire_check,
        search_form = search_form,
        grant_form = grant_form,
        table = table,
        pag = pag,
        footer = footer
    )
}

#[tracing::instrument(skip_all)]
pub(super) async fn subs_export(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsExportQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let is_owner = Role::from_role_str(&session.role).has_level(Role::Owner);
    let seller_scope = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id
    } else {
        None
    };
    let status_filter = normalize_sub_status(q.status.clone());
    let status_opt = if status_filter == "all" {
        None
    } else {
        Some(status_filter.as_str())
    };
    let search_opt = q.q.clone().filter(|s| !s.trim().is_empty());
    let reveal = q.reveal.as_deref() == Some("1")
        || q.include_user_ids.as_deref() == Some("true")
        || q.include_user_ids.as_deref() == Some("1");
    let allow_reveal = is_owner && reveal;

    let rows = state.db.list_subscriptions_filtered_paged(
        seller_scope,
        status_opt,
        search_opt.as_deref(),
        100000,
        0,
    );

    let mut csv = String::from("id,seller_id,user_id,channel_id,status,expires_at\n");
    for r in rows {
        let user_val = if allow_reveal {
            r.2.to_string()
        } else {
            mask_user_id(r.2)
        };
        csv.push_str(&format!(
            "{},{},{},{},{},{}\n",
            r.0,
            r.1,
            user_val,
            esc_csv(&r.3),
            esc_csv(&r.4),
            esc_csv(&r.5)
        ));
    }
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=\"subscriptions.csv\"",
            ),
        ],
        csv,
    )
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct SubsExportQuery {
    #[serde(default)]
    pub(super) status: Option<String>,
    #[serde(default)]
    pub(super) q: Option<String>,
    #[serde(default)]
    pub(super) reveal: Option<String>,
    #[serde(default)]
    pub(super) include_user_ids: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn subs_revoke(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SubsRevokeForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Subs", "Subscriptions management").into_response();
    }
    let id = match f.id.or(f.sub_id) {
        Some(v) => v,
        None => {
            return Redirect::to(&state.url("/subs?error=invalid")).into_response();
        }
    };
    // validate parse already done by serde; treat non-positive as invalid.
    if id <= 0 {
        return Redirect::to(&state.url("/subs?error=invalid")).into_response();
    }
    state.db.set_subscription_status(id, "revoked");
    // Revoke must actually revoke: kick (ban+unban) so the buyer loses
    // channel access immediately — the same move finance_refunds_process
    // makes for refunded invoices. Best-effort, but never silent: a failed
    // kick surfaces a warning banner instead of a plain success.
    let mut kick_failed = false;
    if let (Some(uid), Some(ch)) = (f.user_id, f.channel.as_deref()) {
        let ch = ch.trim();
        if uid > 0 && !ch.is_empty() {
            // The form carries no seller id, so resolve the kick's bot from
            // the target channel (channel -> seller -> bot, primary when the
            // channel has no owner). One bot never moderates another bot's
            // channel.
            let kick_bot = state.bots.for_channel(ch);
            let cid = crate::util::to_chat_id(ch);
            let ban = kick_bot
                .bot
                .ban_chat_member(cid.clone(), teloxide::types::UserId(uid as u64))
                .await;
            let unban = kick_bot
                .bot
                .unban_chat_member(cid, teloxide::types::UserId(uid as u64))
                .await;
            kick_failed = ban.is_err() || unban.is_err();
        }
    }
    if kick_failed {
        Redirect::to(&state.url("/subs?revoked=1&warn=kick")).into_response()
    } else {
        Redirect::to(&state.url("/subs?revoked=1")).into_response()
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn subs_expirecheck(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Subs", "Subscriptions management").into_response();
    }
    let filter = normalize_sub_status(q.status.clone());
    let expired = state.db.expired_subscriptions();
    let count = expired.len() as i64;
    let mut kick_failed = 0usize;
    for (id, seller_id, user, chan) in expired {
        // Kick the member from the channel (ban + unban) before marking.
        // The row's seller owns the channel, so their bot does the kick —
        // the same resolution the periodic sweeper uses.
        let kick_bot = state.bots.for_seller(seller_id);
        let cid = crate::util::to_chat_id(&chan);
        let ban = kick_bot
            .bot
            .ban_chat_member(cid.clone(), teloxide::types::UserId(user as u64))
            .await;
        let unban = kick_bot
            .bot
            .unban_chat_member(cid, teloxide::types::UserId(user as u64))
            .await;
        if ban.is_err() || unban.is_err() {
            kick_failed += 1;
        }
        state.db.mark_subscription_expired(id);
    }
    let mut url = format!("/subs?expired={}", count);
    if filter != "all" {
        url = format!("/subs?status={}&expired={}", filter, count);
    }
    if kick_failed > 0 {
        url.push_str(&format!("&warn=kick&failed={}", kick_failed));
    }
    Redirect::to(&state.url(&url)).into_response()
}

#[derive(Deserialize)]
pub(super) struct SubsExtendForm {
    pub(super) id: Option<i64>,
    pub(super) sub_id: Option<i64>,
    pub(super) days: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn subs_extend(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<SubsExtendForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Subs", "Subscriptions management").into_response();
    }
    let id = match f.id.or(f.sub_id) {
        Some(v) if v > 0 => v,
        _ => return Redirect::to(&state.url("/subs?error=invalid")).into_response(),
    };
    let days: i64 = f
        .days
        .as_deref()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30)
        .clamp(1, crate::constants::MAX_PLAN_DAYS);
    if state.db.extend_subscription(id, days) {
        Redirect::to(&state.url("/subs?extended=1")).into_response()
    } else {
        Redirect::to(&state.url("/subs?error=notfound")).into_response()
    }
}

#[derive(Deserialize)]
pub(super) struct SubsBulkRevokeQuery {
    pub(super) status: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn subs_bulkrevoke(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsBulkRevokeQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Subs", "Subscriptions management").into_response();
    }
    // A revoke that only flips a status column leaves the buyer in the
    // channel. Fetch the matching rows first (the same filter
    // bulk_revoke_subscriptions applies), kick each live member, then flip.
    // Capped at 1000 like the console's other bulk reads; anything beyond
    // the cap is reported instead of silently skipped.
    let status_opt = q
        .status
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "all");
    pub(super) const KICK_CAP: i64 = 1000;
    let rows = state
        .db
        .list_subscriptions_filtered_paged(None, status_opt, None, KICK_CAP, 0);
    let total = state
        .db
        .count_subscriptions_filtered(None, status_opt, None);
    let mut kick_failed = 0usize;
    for (_id, _seller, uid, ch, st, _exp) in &rows {
        if st.as_str() == "revoked" {
            continue;
        }
        if *uid <= 0 || ch.trim().is_empty() {
            continue;
        }
        let cid = crate::util::to_chat_id(ch);
        // Each row's channel decides which bot kicks (channel -> seller ->
        // bot, primary fallback), so a bulk revoke never bans through
        // another seller's bot.
        let kick_bot = state.bots.for_channel(ch);
        let ban = kick_bot
            .bot
            .ban_chat_member(cid.clone(), teloxide::types::UserId(*uid as u64))
            .await;
        let unban = kick_bot
            .bot
            .unban_chat_member(cid, teloxide::types::UserId(*uid as u64))
            .await;
        if ban.is_err() || unban.is_err() {
            kick_failed += 1;
        }
    }
    let n = state.db.bulk_revoke_subscriptions(status_opt);
    let mut url = format!("/subs?bulkrevoked={}", n);
    if kick_failed > 0 {
        url.push_str(&format!("&warn=kick&failed={}", kick_failed));
    }
    if total > rows.len() as i64 {
        url.push_str("&partial=1");
    }
    Redirect::to(&state.url(&url)).into_response()
}
