use super::*;
// ---- Phase 3: Customers + Tickets (web) ----

#[derive(Deserialize)]
pub(super) struct CustomersQuery {
    pub(super) tag: Option<String>,
    pub(super) status: Option<String>,
    pub(super) q: Option<String>,
}

#[derive(Clone)]
struct Row {
    pub(super) seller_id: i64,
    pub(super) user_id: i64,
    pub(super) last_seen: String,
    pub(super) tags: Option<String>,
    pub(super) spent: f64,
    pub(super) orders: i64,
    pub(super) status: String,
    pub(super) notes: String,
}

/// The statuses the system actually writes (db default `active`, /block's
/// `blocked`) — chip filters that exist only here would lie with empty
/// tables. Extend when the product writes more statuses (EDIT_FLOW_SPEC C6).
const CUSTOMER_STATUSES: [&str; 2] = ["active", "blocked"];

/// Status chips nav (the orders/bookings idiom): real links, the active one
/// marked aria-current="page". Non-empty tag/search filters ride along so
/// clicking a chip never silently resets them. The all-chip is the bare URL
/// — `list_customers` has no "all" row value, so `?status=all` would filter
/// literally and show an empty table (the orders page guards the same way,
/// referrals.rs:161).
fn customers_filter_nav(
    state: &WebState,
    status_filter: &Option<String>,
    tag: &Option<String>,
    search: &Option<String>,
) -> String {
    let mut extra = String::new();
    if let Some(t) = tag {
        extra.push_str(&format!("&tag={}", qenc(t)));
    }
    if let Some(s) = search {
        extra.push_str(&format!("&q={}", qenc(s)));
    }
    let base = state.url("/customers");
    let all_href = if extra.is_empty() {
        base.clone()
    } else {
        format!("{}?{}", base, extra.trim_start_matches('&'))
    };
    let current = status_filter.clone().unwrap_or_else(|| "all".to_string());
    let links = std::iter::once("all")
        .chain(CUSTOMER_STATUSES.iter().copied())
        .map(|s| {
            let cls = if s == current {
                "btn btn-secondary btn-sm"
            } else {
                "btn btn-ghost btn-sm"
            };
            let aria = if s == current {
                r#" aria-current="page""#
            } else {
                ""
            };
            let href = if s == "all" {
                all_href.clone()
            } else {
                format!("{}?status={}{}", base, s, extra)
            };
            format!(r#"<a class="{}" href="{}"{}>{}</a>"#, cls, href, aria, s)
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        r#"<nav aria-label="Filter customers by status" class="inline-form">{}</nav>"#,
        links
    )
}

#[tracing::instrument(skip_all)]
pub(super) async fn customers_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<CustomersQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let tag_filter = q.tag.clone().filter(|s| !s.trim().is_empty());
    // "all" is the no-filter choice (the orders guard, referrals.rs:161):
    // list_customers has no "all" row value — passing it through would
    // filter literally and show an empty table.
    let status_filter = q
        .status
        .clone()
        .filter(|s| !s.trim().is_empty() && s != "all");
    let search = q.q.clone().filter(|s| !s.trim().is_empty());

    let rows = load_customers(&state, seller_scope, &tag_filter, &status_filter, &search);

    let tag_val = tag_filter.clone().unwrap_or_default();
    let q_val = search.clone().unwrap_or_default();
    // C6: status is chip-filtered; Tag and Search stay free-form inputs.
    // Enter still submits the form (implicit submission) — the old ghost
    // Filter button is gone because the chips are the links.
    let filter_form = format!(
        r#"<form method="get" action="{}" class="inline-form">
<label>Tag <input name="tag" value="{}"></label>
<label>Search <input name="q" value="{}"></label>
</form>
{}"#,
        state.url("/customers"),
        esc(&tag_val),
        esc(&q_val),
        customers_filter_nav(&state, &status_filter, &tag_filter, &search)
    );

    let rows_html = customers_rows_html(&rows);
    let table = format!(
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Customers"><table aria-label="Customers"><thead><tr><th scope="col">User</th><th scope="col">Seller</th><th scope="col">Last seen</th><th scope="col">Tags</th><th scope="col">Spent</th><th scope="col">Orders</th><th scope="col">Status</th><th scope="col">Notes</th></tr></thead><tbody>{}</tbody></table></div>"#,
        rows_html
    );
    let body = format!(
        r#"<h1>Customers <span class="sub">CRM</span></h1>
{filter_form}
{table}"#,
        filter_form = filter_form,
        table = table
    );
    Html(html_page(&state, Some(&session), "Customers", &body)).into_response()
}

fn load_customers(
    state: &WebState,
    seller_scope: Option<i64>,
    tag_filter: &Option<String>,
    status_filter: &Option<String>,
    search: &Option<String>,
) -> Vec<Row> {
    let sellers: Vec<i64> = match seller_scope {
        Some(sid) => vec![sid],
        None => {
            let mut v = state.db.list_sellers();
            v.push(0);
            v
        }
    };
    let mut rows: Vec<Row> = Vec::new();
    for sid in sellers {
        for (uid, last_seen, tags, spent, orders, status) in
            state
                .db
                .list_customers(sid, tag_filter.as_deref(), status_filter.as_deref())
        {
            let notes = state
                .db
                .get_customer(sid, uid)
                .and_then(|c| c.5)
                .unwrap_or_default();
            if let Some(s) = search {
                let s = s.to_lowercase();
                let hit = uid.to_string().contains(&s)
                    || tags.as_deref().unwrap_or("").to_lowercase().contains(&s)
                    || notes.to_lowercase().contains(&s)
                    || status.to_lowercase().contains(&s);
                if !hit {
                    continue;
                }
            }
            rows.push(Row {
                seller_id: sid,
                user_id: uid,
                last_seen,
                tags,
                spent,
                orders,
                status,
                notes,
            });
        }
    }
    rows
}

fn customers_rows_html(rows: &[Row]) -> String {
    if rows.is_empty() {
        return r#"<tr><td colspan="8" class="empty"><b>No customers yet.</b> They appear after a buyer's first purchase.</td></tr>"#
            .to_string();
    }
    rows.iter()
        .map(|r| {
            // Status uses the console's dot-before-a-word badge vocabulary
            // (B15.1): vip/regular both map to the plain default badge.
            format!(
                r#"<tr><td class="num">{}</td><td class="num">{}</td><td>{}</td><td>{}</td><td class="num">{:.2}</td><td class="num">{}</td><td><span class="badge">{}</span></td><td>{}</td></tr>"#,
                r.user_id,
                r.seller_id,
                esc(&r.last_seen),
                esc(r.tags.as_deref().unwrap_or("—")),
                r.spent,
                r.orders,
                esc(&r.status),
                esc(&r.notes)
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Ticket status → the console's dot-before-a-word badge vocabulary
/// (B15.1): the word always renders beside the dot, color never alone.
fn ticket_badge(status: &str) -> &'static str {
    match status {
        "pending" => "badge badge-warning",
        "resolved" => "badge badge-success",
        "escalated" => "badge badge-danger",
        _ => "badge",
    }
}

#[derive(Deserialize)]
pub(super) struct TicketsQuery {
    pub(super) status: Option<String>,
    pub(super) id: Option<i64>,
    /// C5 (EDIT_FLOW_SPEC): both ticket mutations redirect ?saved=1 — the
    /// status change and the reply used to re-render with no feedback.
    pub(super) saved: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct TicketReplyForm {
    pub(super) ticket_id: i64,
    pub(super) text: String,
}

#[derive(Deserialize)]
pub(super) struct TicketStatusForm {
    pub(super) ticket_id: i64,
    pub(super) status: String,
}

/// Ticket chips nav (the bookings/orders idiom, EDIT_FLOW_SPEC C5): the
/// known status vocabulary as real links instead of a free-text input. The
/// all-chip is the bare URL — `list_tickets` has no "all" row value, so
/// `?status=all` would filter literally and show an empty table (the orders
/// page guards the same way, referrals.rs:161).
fn tickets_filter_nav(state: &WebState, status_filter: &Option<String>) -> String {
    let statuses = ["all", "open", "pending", "resolved", "closed", "escalated"];
    let current = status_filter.clone().unwrap_or_else(|| "all".to_string());
    let links = statuses
        .iter()
        .map(|s| {
            let cls = if *s == current {
                "btn btn-secondary btn-sm"
            } else {
                "btn btn-ghost btn-sm"
            };
            let aria = if *s == current {
                r#" aria-current="page""#
            } else {
                ""
            };
            let href = if *s == "all" {
                state.url("/tickets")
            } else {
                format!("{}?status={}", state.url("/tickets"), s)
            };
            format!(r#"<a class="{}" href="{}"{}>{}</a>"#, cls, href, aria, s)
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        r#"<nav aria-label="Filter tickets by status" class="inline-form">{}</nav>"#,
        links
    )
}

#[tracing::instrument(skip_all)]
pub(super) async fn tickets_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<TicketsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };

    if let Some(id) = q.id {
        return ticket_detail_response(&state, &session, seller_scope, id, &q);
    }

    // "all" is the no-filter choice (the orders guard, referrals.rs:161):
    // list_tickets has no "all" row value — passing it through would filter
    // literally and show an empty table.
    let status_filter = q
        .status
        .clone()
        .filter(|s| !s.trim().is_empty() && s != "all");
    let tickets = if let Some(ss) = seller_scope {
        state
            .db
            .list_tickets(Some(ss), None, status_filter.as_deref())
    } else {
        state.db.list_tickets(None, None, status_filter.as_deref())
    };
    let rows_html = if tickets.is_empty() {
        r#"<tr><td colspan="6" class="empty"><b>No tickets.</b> Buyer conversations opened with /support appear here.</td></tr>"#.to_string()
    } else {
        tickets
            .iter()
            .map(|(id, sid, buyer, subject, status, updated)| {
                format!(
                    r#"<tr><td class="num"><a href="{}?id={}">{}</a></td><td class="num">{}</td><td class="num">{}</td><td>{}</td><td><span class="{}">{}</span></td><td>{}</td></tr>"#,
                    state.url("/tickets"),
                    id,
                    id,
                    sid,
                    buyer,
                    esc(subject),
                    ticket_badge(status),
                    esc(status),
                    esc(updated)
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let filter_form = tickets_filter_nav(&state, &status_filter);
    let table = format!(
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Tickets"><table aria-label="Tickets"><thead><tr><th scope="col">ID</th><th scope="col">Seller</th><th scope="col">Buyer</th><th scope="col">Subject</th><th scope="col">Status</th><th scope="col">Updated</th></tr></thead><tbody>{}</tbody></table></div>"#,
        rows_html
    );
    let body = format!(
        r#"<h1>Tickets <span class="sub">support</span></h1>
{filter_form}
{table}"#,
        filter_form = filter_form,
        table = table
    );
    Html(html_page(&state, Some(&session), "Tickets", &body)).into_response()
}

fn ticket_detail_response(
    state: &WebState,
    session: &Session,
    seller_scope: Option<i64>,
    id: i64,
    q: &TicketsQuery,
) -> Response {
    let ticket = match state.db.get_ticket(id) {
        Some(t) => t,
        None => return Redirect::to(&state.url("/tickets")).into_response(),
    };
    let (_id, seller_id, buyer_id, subject, status, created, updated) = ticket;
    if let Some(ss) = seller_scope
        && seller_id != ss
    {
        return Redirect::to(&state.url("/tickets")).into_response();
    }
    let msgs = state.db.list_ticket_messages(id);
    let mut thread = String::new();
    for (_mid, role, sid, text, created_at) in msgs {
        thread.push_str(&format!(
            r#"<div class="panel"><b>{} ({})</b> <span class="help">{}</span><p>{}</p></div>"#,
            esc(&role),
            sid,
            esc(&created_at),
            esc(&text)
        ));
    }
    let status_form = format!(
        r#"<form method="post" action="{}" class="inline-form">
<input type="hidden" name="ticket_id" value="{}">
<select name="status" aria-label="Set status for ticket {}">
<option value="open" {}>open</option>
<option value="pending" {}>pending</option>
<option value="resolved" {}>resolved</option>
<option value="closed" {}>closed</option>
<option value="escalated" {}>escalated</option>
</select>
<button type="submit" class="btn btn-ghost btn-sm">Save</button>
</form>"#,
        state.url("/tickets/status"),
        id,
        id,
        if status == "open" { "selected" } else { "" },
        if status == "pending" { "selected" } else { "" },
        if status == "resolved" { "selected" } else { "" },
        if status == "closed" { "selected" } else { "" },
        if status == "escalated" {
            "selected"
        } else {
            ""
        }
    );
    let reply_form = format!(
        r#"<form method="post" action="{}">
<input type="hidden" name="ticket_id" value="{}">
<label>Reply <textarea name="text" required></textarea></label>
<button type="submit">Reply</button>
</form>"#,
        state.url("/tickets/reply"),
        id
    );
    // C5: the reply/status POSTs land here with ?saved=1 — both used to
    // re-render with zero acknowledgment.
    let saved_banner = if q.saved.as_deref() == Some("1") {
        r#"<div class="alert alert-success" role="status" aria-live="polite">Ticket updated.</div>"#
    } else {
        ""
    };
    let body = format!(
        r#"<h1>Ticket #{} <span class="sub">{}</span></h1>
{saved_banner}
<p>Seller {} · Buyer {} · Status <b>{}</b> · Created {} · Updated {}</p>
{status_form}
{thread}
{reply_form}
<p><a href="{}">Back to tickets</a></p>"#,
        id,
        esc(&subject),
        seller_id,
        buyer_id,
        esc(&status),
        esc(&created),
        esc(&updated),
        state.url("/tickets")
    );
    Html(html_page(state, Some(session), "Tickets", &body)).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn tickets_reply(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<TicketReplyForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    if let Some(t) = state.db.get_ticket(f.ticket_id) {
        let (_id, seller_id, _buyer, _subj, _status, _c, _u) = t;
        if seller_scope.is_none() || seller_scope == Some(seller_id) {
            let role_str = if session.role == "owner" || session.role == "admin" {
                "admin"
            } else {
                "seller"
            };
            state
                .db
                .add_ticket_message(f.ticket_id, role_str, session.account_id, &f.text);
        }
    }
    Redirect::to(&state.url(&format!("/tickets?id={}&saved=1", f.ticket_id))).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn tickets_status(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<TicketStatusForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    if let Some(t) = state.db.get_ticket(f.ticket_id) {
        let (_id, seller_id, _buyer, _subj, _status, _c, _u) = t;
        if seller_scope.is_none() || seller_scope == Some(seller_id) {
            let _ = state.db.set_ticket_status(f.ticket_id, &f.status);
        }
    }
    Redirect::to(&state.url(&format!("/tickets?id={}&saved=1", f.ticket_id))).into_response()
}
