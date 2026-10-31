use super::*;
// ---------- Finance ----------

#[derive(Deserialize)]
pub(super) struct FinanceQuery {
    pub(super) status: Option<String>,
    /// Orders page cross-link: prefill the refund-create form with the
    /// invoice bound to this order.
    pub(super) refund_order: Option<String>,
    // Action outcomes, rendered as banners.
    pub(super) payout_created: Option<String>,
    pub(super) refund_created: Option<String>,
    pub(super) payout_approved: Option<String>,
    pub(super) refund_approved: Option<String>,
    pub(super) payout_executed: Option<String>,
    pub(super) refund_executed: Option<String>,
    pub(super) warn: Option<String>,
    pub(super) error: Option<String>,
    pub(super) detail: Option<String>,
}

/// Same shape as `normalize_sub_status`: unknown values fall back to "all"
/// instead of becoming an injection vector into generated URLs.
pub(super) fn normalize_finance_status(s: Option<String>) -> String {
    match s.as_deref() {
        Some("all") | Some("pending") | Some("approved") | Some("executing") | Some("paid")
        | Some("executed") => s.unwrap(),
        _ => "all".to_string(),
    }
}

/// Percent-encodes a value for a redirect query string. Only unreserved
/// characters survive verbatim, so no context can misread the rest.
pub(super) fn qenc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

pub(super) fn finance_redirect(state: &WebState, query: &str) -> Response {
    Redirect::to(&state.url(&format!("/finance?{}", query))).into_response()
}

/// The queue sort: pending first, then approved, then the rest — stable, so
/// each group keeps the DB's newest-first order.
pub(super) fn finance_rank(status: &str) -> u8 {
    match status {
        "pending" => 0,
        "approved" => 1,
        _ => 2,
    }
}

pub(super) fn finance_badge(status: &str) -> &'static str {
    match status {
        "paid" | "executed" => "badge badge-success",
        // Waiting/in-flight states read amber: pending = awaiting approval,
        // approved = awaiting execution, executing = a transfer is claimed
        // and in flight (or was interrupted mid-flight) — the text carries
        // which is which.
        "pending" | "approved" | "executing" => "badge badge-warning",
        "failed" | "rejected" => "badge badge-danger",
        _ => "badge",
    }
}

/// Resolves a Crypto Pay token exactly like the REST finance handlers (and
/// the webhook): the startup config first, then the runtime `crypto_token`
/// config key.
pub(super) fn crypto_token(state: &WebState) -> Option<String> {
    state
        .cfg
        .crypto_token
        .clone()
        .or_else(|| state.db.get_config("crypto_token"))
}

/// Whether a CryptoPay `transfer` error is a definite API rejection — the
/// request completed a round trip and the API refused, so no money moved and
/// the claimed row can safely go back to 'approved' for retry — versus an
/// ambiguous transport outcome (timeout, network failure, garbled or gateway
/// response) where the transfer state is unknown. `CryptoPay::call` surfaces
/// errors as strings in three shapes: reqwest transport failures ("error
/// sending request…", "error decoding response body…", "builder error…"),
/// non-2xx responses ("HTTP <status> - <body>"), and the API's own `ok:false`
/// error name for a parsed rejection (no prefix). Only the last is always a
/// refusal; of the "HTTP" shape only a 4xx other than 408 proves it — a 5xx
/// means an intermediary may have dropped a transfer the API actually
/// processed, and 408 is a timeout by definition. Ambiguous outranks
/// definite on doubt: reverting an executed transfer to 'approved' invites a
/// double payout, while a stuck 'executing' row only costs a manual check of
/// the queue.
pub(super) fn transfer_rejected_definitely(e: &str) -> bool {
    if let Some(rest) = e.strip_prefix("HTTP ") {
        let code = rest.split([' ', '-']).next().unwrap_or_default();
        return code.starts_with('4') && code != "408";
    }
    !e.starts_with("error sending request")
        && !e.starts_with("error decoding response body")
        && !e.starts_with("error reading a body")
        && !e.starts_with("builder error")
}

fn finance_page_banners(q: &FinanceQuery) -> Option<String> {
    // Banners (already-escaped HTML); several can apply at once.
    let mut banners: Vec<String> = Vec::new();
    if q.payout_created.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite">Payout requested. It stays pending until the owner approves it.</div>"#.to_string());
    }
    if q.refund_created.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite">Refund created. It stays pending until an admin or the owner approves it.</div>"#.to_string());
    }
    if q.payout_approved.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite">Payout approved. Execute the transfer when ready.</div>"#.to_string());
    }
    if q.refund_approved.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite">Refund approved. Execute the transfer when ready.</div>"#.to_string());
    }
    if q.payout_executed.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite"><strong>Payout executed.</strong> The Crypto Pay transfer completed and the payout is marked paid.</div>"#.to_string());
    }
    if q.refund_executed.as_deref() == Some("1") {
        banners.push(r#"<div class="alert alert-success" role="status" aria-live="polite"><strong>Refund executed.</strong> The Crypto Pay transfer completed; the invoice is marked refunded and the buyer's subscription revoked.</div>"#.to_string());
    }
    if q.warn.as_deref() == Some("kick") {
        banners.push(r#"<div class="alert alert-warn" role="alert" aria-live="polite">The transfer succeeded, but removing the buyer from the channel failed — check the bot's admin rights there and remove access manually.</div>"#.to_string());
    }
    if let Some(err) = q.error.as_ref() {
        let (msg, with_detail) = match err.as_str() {
            "invalid" => ("Invalid payout or refund id.".to_string(), false),
            "seller_required" => (
                "Seller id is required — admins must name the seller.".to_string(),
                false,
            ),
            "amount_required" => ("Amount and asset are required.".to_string(), false),
            "invoice_required" => ("Invoice id is required.".to_string(), false),
            "invoice_notfound" => ("Invoice not found.".to_string(), false),
            "invoice_owner" => ("That invoice belongs to another seller.".to_string(), false),
            "invoice_state" => ("Only paid invoices can be refunded.".to_string(), false),
            "notfound" => ("Payout or refund not found.".to_string(), false),
            "destination" => ("Payout has no destination.".to_string(), false),
            "bad_destination" => ("Destination must be a Telegram user id.".to_string(), false),
            "no_user" => (
                "Refund has no bound user; cannot transfer.".to_string(),
                false,
            ),
            "executing" => (
                "Execution already in progress — check the queue before retrying.".to_string(),
                false,
            ),
            "ambiguous" => (
                "Transfer state unknown — do NOT retry until you check the payouts queue; the row shows 'executing' until resolved.".to_string(),
                true,
            ),
            "invalid_asset" => (
                "Refund asset must match the invoice asset.".to_string(),
                false,
            ),
            "amount_number" => ("Refund amount must be a number.".to_string(), false),
            "amount_positive" => ("Refund amount must be positive.".to_string(), false),
            "transfer_unavailable" => (
                "Crypto Pay is not configured — no transfer was attempted.".to_string(),
                false,
            ),
            "state" => (
                "That record is not in a state that allows this action.".to_string(),
                true,
            ),
            "transfer" => (
                "Transfer failed — the record stays approved and can be retried.".to_string(),
                true,
            ),
            _ => (String::new(), false),
        };
        if !msg.is_empty() {
            let detail = match (with_detail, q.detail.as_deref()) {
                (true, Some(d)) if !d.is_empty() => format!(" {}", esc(d)),
                _ => String::new(),
            };
            banners.push(format!(
                r#"<div class="alert alert-error" role="alert" aria-live="polite"><strong>{}</strong>{}</div>"#,
                msg, detail
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
pub(super) async fn finance_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<FinanceQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    // Seller-or-admin, the same population the REST finance list endpoints
    // serve; a seller without a seller profile is the REST `not_a_seller`
    // case.
    if session.role != "seller" && !is_admin_role(&session.role) {
        return forbidden_page(&state, "Finance", "Finance").into_response();
    }
    if session.role == "seller" && session.seller_id.is_none() {
        return forbidden_page(&state, "Finance", "Finance").into_response();
    }
    let filter = normalize_finance_status(q.status.clone());

    let banner_html = finance_page_banners(&q);

    // Prefill the refund form when arriving from an Orders page link.
    let refund_prefill = q
        .refund_order
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|oid| state.db.get_order(oid))
        .map(|(_, _, inv)| inv)
        .filter(|inv| !inv.trim().is_empty());

    Html(html_page(
        &state,
        Some(&session),
        "Finance",
        &finance_body(
            &state,
            &session,
            &filter,
            banner_html.as_deref(),
            refund_prefill,
        ),
    ))
    .into_response()
}

fn finance_payout_form(state: &WebState, is_admin: bool) -> String {
    let seller_field = if is_admin {
        r#"<label for="seller_id">Seller id</label><input id="seller_id" type="number" name="seller_id" required>"#.to_string()
    } else {
        String::new()
    };
    format!(
        r#"<article class="panel"><h2>Request payout</h2>
<form method="post" action="{}">
{}
<label for="amount">Amount</label><input id="amount" type="text" name="amount" required>
<label for="asset">Asset</label><input id="asset" type="text" name="asset" value="USDT" required>
<label for="destination">Destination (Telegram user id)</label><input id="destination" type="text" name="destination" inputmode="numeric">
<button type="submit">Request payout</button>
</form>
<p class="help">{}</p>
</article>"#,
        state.url("/finance/payout/create"),
        seller_field,
        if is_admin {
            "The transfer goes through Crypto Pay to the destination Telegram user id."
        } else {
            "Requested for your own seller account; the transfer goes through Crypto Pay to the destination Telegram user id."
        }
    )
}

fn finance_refund_form(state: &WebState, refund_prefill: Option<String>) -> String {
    format!(
        r#"<article class="panel" id="refund-form"><h2>Create refund</h2>
<form method="post" action="{}">
<label for="invoice_id">Invoice id</label><input id="invoice_id" type="text" name="invoice_id" value="{}" required>
<label for="amount">Amount (defaults to the invoice's)</label><input id="amount" type="text" name="amount">
<label for="asset">Asset (defaults to the invoice's)</label><input id="asset" type="text" name="asset">
<label for="reason">Reason</label><input id="reason" type="text" name="reason">
<button type="submit">Create refund</button>
</form>
<p class="help">Only paid invoices can be refunded; the refund pays back the invoice's buyer.</p>
</article>"#,
        state.url("/finance/refund/create"),
        esc(refund_prefill.as_deref().unwrap_or(""))
    )
}

fn finance_payout_row(
    state: &WebState,
    is_admin: bool,
    is_owner: bool,
    p: &(i64, i64, String, String, Option<String>, String),
) -> String {
    let (id, seller, amount, asset, destination, status) = p;
    let seller_cell = if is_admin {
        format!(r#"<td>{}</td>"#, seller)
    } else {
        String::new()
    };
    let dest_cell = format!(
        r#"<td class="mono">{}</td>"#,
        esc(destination.as_deref().unwrap_or("—"))
    );
    let status_cell = format!(
        r#"<td><span class="{}">{}</span></td>"#,
        finance_badge(status),
        esc(status)
    );
    let actions = if *status == "pending" && is_owner {
        format!(
            r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="id" value="{}"><button type="submit" class="btn btn-secondary btn-sm" aria-label="Approve payout {}" onclick="return confirm('Approve payout #{} for {} {}? This records your approval as the owner.')">Approve</button></form>"#,
            state.url("/finance/payout/approve"),
            id,
            id,
            id,
            esc(amount),
            esc(asset)
        )
    } else if *status == "approved" && is_owner {
        format!(
            r#"<form method="post" action="{}" class="inline-form" onsubmit="var b=this.querySelector('button[type=submit]');if(b){{b.disabled=true;b.setAttribute('aria-busy','true');}}"><input type="hidden" name="id" value="{}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Execute payout {}" onclick="return confirm('Execute payout #{}? This sends {} {} to the destination via Crypto Pay — real money.')">Execute</button></form>"#,
            state.url("/finance/payout/execute"),
            id,
            id,
            id,
            esc(amount),
            esc(asset)
        )
    } else {
        String::new()
    };
    format!(
        r#"<tr><td class="num">{}</td>{}<td class="mono">{} {}</td>{}{}<td class="actions">{}</td></tr>"#,
        id,
        seller_cell,
        esc(amount),
        esc(asset),
        dest_cell,
        status_cell,
        actions
    )
}

fn finance_payout_table(state: &WebState, session: &Session, filter: &str) -> String {
    let is_admin = is_admin_role(&session.role);
    let is_owner = session.role == "owner";
    let scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let bot_scope = bot_scope_sellers(state, session);
    let mut payouts = state.db.list_payouts(scope);
    payouts.retain(|p| in_bot_scope(&bot_scope, p.1));
    if filter != "all" {
        payouts.retain(|p| p.5 == filter);
    }
    payouts.sort_by_key(|p| finance_rank(&p.5));

    let payout_cols = if is_admin { 6 } else { 5 };
    let payout_head = if is_admin {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">Seller</th><th scope="col">Amount</th><th scope="col">Destination</th><th scope="col">Status</th><th scope="col">Actions</th></tr>"#
    } else {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">Amount</th><th scope="col">Destination</th><th scope="col">Status</th><th scope="col">Actions</th></tr>"#
    };
    let payout_rows = if payouts.is_empty() {
        if filter != "all" {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No {} payouts.</b> <a href="{}">Clear filter</a></td></tr>"#,
                payout_cols,
                esc(filter),
                state.url("/finance")
            )
        } else {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No payouts yet.</b> Request one above — it stays pending until the owner approves it, and only then can money move.</td></tr>"#,
                payout_cols
            )
        }
    } else {
        payouts
            .iter()
            .map(|p| finance_payout_row(state, is_admin, is_owner, p))
            .collect::<Vec<_>>()
            .join("")
    };
    let payout_note = if is_owner {
        String::new()
    } else {
        r#"<p class="help">Payout approval and execution are owner actions (two-person money rule).</p>"#.to_string()
    };
    format!(
        r#"{note}<div class="table-wrap" tabindex="0" role="region" aria-label="Payouts"><table aria-label="Payouts"><caption class="sr-only">Payouts</caption><thead>{head}</thead><tbody>{rows}</tbody></table></div>"#,
        note = payout_note,
        head = payout_head,
        rows = payout_rows
    )
}

type RefundRow = (
    i64,
    i64,
    Option<String>,
    Option<i64>,
    String,
    String,
    Option<String>,
    String,
);

fn finance_refund_row(state: &WebState, is_admin: bool, r: &RefundRow) -> String {
    let (id, seller, invoice_id, _user, amount, asset, reason, status) = r;
    let seller_cell = if is_admin {
        format!(r#"<td>{}</td>"#, seller)
    } else {
        String::new()
    };
    let invoice_cell = match reason.as_deref() {
        Some(rs) if !rs.trim().is_empty() => format!(
            r#"<td class="mono">{}<div class="help">{}</div></td>"#,
            esc(invoice_id.as_deref().unwrap_or("—")),
            esc(rs)
        ),
        _ => format!(
            r#"<td class="mono">{}</td>"#,
            esc(invoice_id.as_deref().unwrap_or("—"))
        ),
    };
    let status_cell = format!(
        r#"<td><span class="{}">{}</span></td>"#,
        finance_badge(status),
        esc(status)
    );
    let actions = if *status == "pending" && is_admin {
        format!(
            r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="id" value="{}"><button type="submit" class="btn btn-secondary btn-sm" aria-label="Approve refund {}" onclick="return confirm('Approve refund #{} for {} {}?')">Approve</button></form>"#,
            state.url("/finance/refund/approve"),
            id,
            id,
            id,
            esc(amount),
            esc(asset)
        )
    } else if *status == "approved" && is_admin {
        format!(
            r#"<form method="post" action="{}" class="inline-form" onsubmit="var b=this.querySelector('button[type=submit]');if(b){{b.disabled=true;b.setAttribute('aria-busy','true');}}"><input type="hidden" name="id" value="{}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Execute refund {}" onclick="return confirm('Execute refund #{}? This sends {} {} back to the buyer via Crypto Pay — real money.')">Execute</button></form>"#,
            state.url("/finance/refund/execute"),
            id,
            id,
            id,
            esc(amount),
            esc(asset)
        )
    } else {
        String::new()
    };
    format!(
        r#"<tr><td class="num">{}</td>{}{}<td class="mono">{} {}</td>{}<td class="actions">{}</td></tr>"#,
        id,
        seller_cell,
        invoice_cell,
        esc(amount),
        esc(asset),
        status_cell,
        actions
    )
}

fn finance_refund_table(state: &WebState, session: &Session, filter: &str) -> String {
    let is_admin = is_admin_role(&session.role);
    let scope = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let bot_scope = bot_scope_sellers(state, session);
    let mut refunds = state.db.list_refunds(scope);
    refunds.retain(|r| in_bot_scope(&bot_scope, r.1));
    if filter != "all" {
        refunds.retain(|r| r.7 == filter);
    }
    refunds.sort_by_key(|r| finance_rank(&r.7));

    let refund_cols = if is_admin { 6 } else { 5 };
    let refund_head = if is_admin {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">Seller</th><th scope="col">Invoice</th><th scope="col">Amount</th><th scope="col">Status</th><th scope="col">Actions</th></tr>"#
    } else {
        r#"<tr><th scope="col" class="num">ID</th><th scope="col">Invoice</th><th scope="col">Amount</th><th scope="col">Status</th><th scope="col">Actions</th></tr>"#
    };
    let refund_rows = if refunds.is_empty() {
        if filter != "all" {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No {} refunds.</b> <a href="{}">Clear filter</a></td></tr>"#,
                refund_cols,
                esc(filter),
                state.url("/finance")
            )
        } else {
            format!(
                r#"<tr><td colspan="{}" class="empty"><b>No refunds yet.</b> Create one above from a paid invoice, or follow the Refund action on an order.</td></tr>"#,
                refund_cols
            )
        }
    } else {
        refunds
            .iter()
            .map(|r| finance_refund_row(state, is_admin, r))
            .collect::<Vec<_>>()
            .join("")
    };
    let refund_note = if is_admin {
        String::new()
    } else {
        r#"<p class="help">Refund approval and execution require admin or owner.</p>"#.to_string()
    };
    format!(
        r#"{note}<div class="table-wrap" tabindex="0" role="region" aria-label="Refunds"><table aria-label="Refunds"><caption class="sr-only">Refunds</caption><thead>{head}</thead><tbody>{rows}</tbody></table></div>"#,
        note = refund_note,
        head = refund_head,
        rows = refund_rows
    )
}

fn finance_filter_nav(state: &WebState, filter: &str) -> String {
    let statuses = [
        "all",
        "pending",
        "approved",
        "executing",
        "paid",
        "executed",
    ];
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
                state.url(&format!("/finance?status={}", s)),
                aria,
                s
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        r#"<nav aria-label="Filter finance queues by status" class="inline-form">{}</nav>"#,
        filter_links
    )
}

pub(super) fn finance_body(
    state: &WebState,
    session: &Session,
    filter: &str,
    banners: Option<&str>,
    refund_prefill: Option<String>,
) -> String {
    let banners = banners.unwrap_or_default();
    let is_admin = is_admin_role(&session.role);
    let payout_form = finance_payout_form(state, is_admin);
    let refund_form = finance_refund_form(state, refund_prefill);
    let payout_table = finance_payout_table(state, session, filter);
    let refund_table = finance_refund_table(state, session, filter);
    let filter_nav = finance_filter_nav(state, filter);
    let scoping_note = if session.role == "seller" {
        "Showing payouts and refunds for your seller account."
    } else {
        "Showing payouts and refunds for all sellers."
    };

    format!(
        r#"{banners}<h1>Finance <span class="sub">payouts, refunds and money movement</span></h1>
<p class="help">{scoping_note} Approval then execution, every step audited; the bot never moves money.</p>
{filter_nav}
<div class="grid-2">
{payout_form}
{refund_form}
</div>
<section aria-labelledby="payouts-heading"><h2 id="payouts-heading">Payouts</h2>
{payout_table}
</section>
<section aria-labelledby="refunds-heading"><h2 id="refunds-heading">Refunds</h2>
{refund_table}
</section>"#,
        banners = banners,
        scoping_note = scoping_note,
        filter_nav = filter_nav,
        payout_form = payout_form,
        refund_form = refund_form,
        payout_table = payout_table,
        refund_table = refund_table
    )
}
