use super::*;
// ---------- Referrals ----------
#[tracing::instrument(skip_all)]
pub(super) async fn referrals_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    // Collect referrals across the visible seller scope.
    let mut all: Vec<(i64, i64, i64, i64, String, String, f64)> = Vec::new(); // (seller, id, referrer, referee, code, status, reward)
    if Role::from_role_str(&session.role) == Role::Seller {
        if let Some(sid) = session.seller_id {
            for r in state.db.list_referrals(sid, None) {
                all.push((sid, r.0, r.1, r.2, r.3, r.4, r.5));
            }
        }
    } else {
        let mut scopes = state.db.list_sellers();
        scopes.push(0);
        for sid in scopes {
            for r in state.db.list_referrals(sid, None) {
                all.push((sid, r.0, r.1, r.2, r.3, r.4, r.5));
            }
        }
    }
    let rows_html = if all.is_empty() {
        r#"<tr><td colspan="6" class="empty"><b>No referrals yet.</b> Buyers join with your referral code — rewards appear here.</td></tr>"#.to_string()
    } else {
        all.iter()
            .map(|r| {
                // C2 (EDIT_FLOW_SPEC): the badge IS the status fact — the
                // raw word column printed the same thing twice.
                format!(
                    r#"<tr><td>{}</td><td class="mono">{}</td><td>{}</td><td>{}</td><td class="mono">{}</td><td class="num">{}</td></tr>"#,
                    r.0,
                    r.2,
                    r.3,
                    esc(&r.4),
                    if r.5 == "rewarded" {
                        r#"<span class="badge badge-success">rewarded</span>"#
                    } else {
                        r#"<span class="badge badge-warning">pending</span>"#
                    },
                    r.6
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let table = format!(
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Referrals"><table aria-label="Referrals"><thead><tr><th scope="col">Seller</th><th scope="col">Referrer</th><th scope="col">Referee</th><th scope="col">Code</th><th scope="col">Status</th><th scope="col">Reward</th></tr></thead><tbody>{}</tbody></table></div>"#,
        rows_html
    );
    // Bonus balance display for the session user/seller.
    let bonus_sid = session.seller_id.unwrap_or(0);
    let bonus_bal = state.db.bonus_balance(bonus_sid, session.account_id);
    let body = format!(
        r#"<h1>Referrals <span class="sub">referral program &amp; rewards</span></h1>
<article class="panel"><div class="stat"><span class="stat-label">Bonus balance</span><span class="stat-value">{}</span></div></article>
{table}"#,
        bonus_bal
    );
    Html(html_page(&state, Some(&session), "Referrals", &body)).into_response()
}

// ---------- Orders ----------

#[tracing::instrument(skip_all)]
/// Order status → badge class, the same dot-before-a-word vocabulary the
/// subscriptions and finance queues use: waiting amber, delivered green,
/// refunded/cancelled brick, shipped in the plain default badge (it reads as
/// "in motion", not "waiting").
pub(super) fn order_badge(status: &str) -> &'static str {
    match status {
        "delivered" => "badge badge-success",
        "pending" => "badge badge-warning",
        "refunded" | "cancelled" => "badge badge-danger",
        _ => "badge",
    }
}

type OrderRow = (String, i64, i64, String, Option<String>, String);

fn orders_rows_html(state: &WebState, filtered: &[OrderRow]) -> String {
    if filtered.is_empty() {
        return r#"<tr><td colspan="7" class="empty"><b>No orders.</b></td></tr>"#.to_string();
    }
    filtered
        .iter()
        .map(|o| {
            // Money back to the buyer goes through the Finance page's
            // audited refund flow (create -> approve -> execute), never a
            // silent transfer: live orders link to the pre-filled
            // refund-create form. Orders never carry a "paid" status
            // (pending/shipped/delivered do); Finance enforces the
            // paid-invoice rule when the refund is submitted.
            // The inline status select therefore offers no "refunded"
            // option (EDIT_FLOW_SPEC E8): one path, one word. The handler
            // still accepts `refunded` for REST/bot parity.
            let refund_link = if o.3 != "refunded" && o.3 != "cancelled" {
                format!(
                    r#"<a class="btn btn-ghost btn-sm" href="{}" aria-label="Request refund for order {}">Refund</a>"#,
                    state.url(&format!("/finance?refund_order={}", esc(&o.0))),
                    esc(&o.0)
                )
            } else {
                String::new()
            };
            format!(
                r#"<tr><td class="mono">{}</td><td>{}</td><td class="mono">{}</td><td><span class="{}">{}</span></td><td class="mono">{}</td><td class="mono">{}</td><td>
<form method="post" action="{}" class="inline-form"><input type="hidden" name="order_id" value="{}"><select name="status" aria-label="New status for order {}"><option value="pending">pending</option><option value="shipped">shipped</option><option value="delivered">delivered</option><option value="cancelled">cancelled</select><button type="submit" class="btn btn-secondary btn-sm">Save</button></form>
<form method="post" action="{}" class="inline-form"><input type="hidden" name="order_id" value="{}"><input type="text" name="tracking" placeholder="Tracking number" aria-label="Tracking number for order {}"><button type="submit" class="btn btn-secondary btn-sm">Save</button></form>
{}</td></tr>"#,
                esc(&o.0),
                o.1,
                o.2,
                order_badge(&o.3),
                esc(&o.3),
                // Tracking is operator free-text — always escaped on
                // render like every other user-supplied cell.
                esc(o.4.as_deref().unwrap_or("—")),
                esc(&o.5),
                state.url("/orders/status"),
                esc(&o.0),
                esc(&o.0),
                state.url("/orders/tracking"),
                esc(&o.0),
                esc(&o.0),
                refund_link
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

pub(super) async fn orders_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let scope = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id
    } else {
        None
    };
    let bot_scope = bot_scope_sellers(&state, &session);
    let orders = state
        .db
        .list_orders(scope)
        .into_iter()
        .filter(|o| in_bot_scope(&bot_scope, o.1))
        .collect::<Vec<_>>();
    let status_filter = q.status.clone().unwrap_or_else(|| "all".to_string());
    let filtered: Vec<_> = if status_filter == "all" {
        orders.clone()
    } else {
        orders
            .iter()
            .filter(|o| o.3 == status_filter)
            .cloned()
            .collect()
    };
    let rows_html = orders_rows_html(&state, &filtered);
    let table = format!(
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Orders"><table aria-label="Orders"><thead><tr><th scope="col">Order</th><th scope="col">Seller</th><th scope="col">Buyer</th><th scope="col">Status</th><th scope="col">Tracking</th><th scope="col">Created</th><th scope="col">Actions</th></tr></thead><tbody>{}</tbody></table></div>"#,
        rows_html
    );
    let statuses = [
        "all",
        "pending",
        "shipped",
        "delivered",
        "refunded",
        "cancelled",
    ];
    let filter_links = statuses
        .iter()
        .map(|s| {
            let cls = if *s == status_filter {
                "btn btn-secondary btn-sm"
            } else {
                "btn btn-ghost btn-sm"
            };
            let aria = if *s == status_filter {
                r#" aria-current="page""#
            } else {
                ""
            };
            format!(
                r#"<a class="{}" href="{}"{}>{}</a>"#,
                cls,
                state.url(&format!("/orders?status={}", s)),
                aria,
                s
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let filter_nav = format!(
        r#"<nav aria-label="Filter orders" class="inline-form">{}</nav>"#,
        filter_links
    );

    // Outcome banner from the PRG redirect (?saved=1): a status or tracking
    // edit used to succeed silently.
    let saved_banner = if q.saved.as_deref() == Some("1") {
        r#"<div class="alert alert-success" role="status" aria-live="polite">Order updated.</div>"#
    } else {
        ""
    };
    let body = format!(
        r#"{saved_banner}<h1>Orders <span class="sub">order management</span></h1>
{filter_nav}
<p class="help">Refunds go through the <a href="{finance}">Finance</a> page: create from a paid invoice, then approve and execute the transfer there.</p>
{table}"#,
        saved_banner = saved_banner,
        filter_nav = filter_nav,
        table = table,
        finance = state.url("/finance")
    );
    Html(html_page(&state, Some(&session), "Orders", &body)).into_response()
}

#[derive(Deserialize)]
pub(super) struct ReviewApproveForm {
    pub(super) id: i64,
}

#[tracing::instrument(skip_all)]
pub(super) async fn reviews_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_scope = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id
    } else {
        None
    };
    // Bot scoping (web-bot-scoping N1): narrow the product list before the
    // per-product review fetch, so a selected bot's page never reads another
    // bot's product rows at all. The approve POST keeps its own gate.
    let bot_scope = bot_scope_sellers(&state, &session);
    let mut products = state.db.list_products(seller_scope);
    products.retain(|p| in_bot_scope(&bot_scope, p.1));
    let mut rows: Vec<String> = Vec::new();
    for p in &products {
        let seller_id = p.1;
        let pid = p.0;
        let pname = &p.2;
        let reviews = state.db.list_reviews(seller_id, pid, false);
        for (rid, buyer, rating, text) in reviews {
            // N2: the approve control names the review by product + buyer,
            // not the internal row id.
            rows.push(format!(
                r#"<tr><td>{}</td><td class="mono">{}</td><td class="num">{}</td><td>{}</td><td class="actions"><form method="post" action="{}" class="inline-form"><input type="hidden" name="id" value="{}"><button type="submit" class="btn btn-secondary btn-sm" aria-label="Approve review from {buyer} on {}">Approve</button></form></td></tr>"#,
                esc(pname),
                buyer,
                rating,
                esc(&text),
                state.url("/reviews/approve"),
                rid,
                esc(pname)
            ));
        }
    }
    let table = if rows.is_empty() {
        // N2: the bold-lead + help empty-state idiom (content.rs:30).
        r#"<p class="empty"><b>No reviews to moderate.</b> Buyer reviews appear here after delivery.</p>"#.to_string()
    } else {
        format!(
            "<div class=\"table-wrap\" tabindex=\"0\" role=\"region\" aria-label=\"Reviews\"><table aria-label=\"Reviews\"><thead><tr><th scope=\"col\">Product</th><th scope=\"col\">Buyer</th><th scope=\"col\">Rating</th><th scope=\"col\">Text</th><th scope=\"col\"><span class=\"sr-only\">Actions</span></th></tr></thead><tbody>{}</tbody></table></div>",
            rows.join("")
        )
    };
    // E9: the approve POST redirects ?saved=1 — the row vanishing with zero
    // acknowledgment used to be indistinguishable from a failed click.
    let saved_banner = if q.saved.as_deref() == Some("1") {
        r#"<div class="alert alert-success" role="status" aria-live="polite">Review approved.</div>"#
    } else {
        ""
    };
    let body =
        format!(r#"<h1>Reviews <span class="sub">moderation</span></h1>{saved_banner}{table}"#);
    Html(html_page(&state, Some(&session), "Reviews", &body)).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn reviews_approve(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<ReviewApproveForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    // Seller scoping: only approve reviews belonging to this session's products.
    let seller_scope = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id
    } else {
        None
    };
    let products = state.db.list_products(seller_scope);
    let allowed: std::collections::HashSet<i64> = products
        .iter()
        .flat_map(|p| {
            state
                .db
                .list_reviews(p.1, p.0, false)
                .into_iter()
                .map(|r| r.0)
        })
        .collect();
    if Role::from_role_str(&session.role) == Role::Seller && !allowed.contains(&f.id) {
        return Redirect::to(&state.url("/reviews")).into_response();
    }
    state.db.approve_review(f.id);
    Redirect::to(&state.url("/reviews?saved=1")).into_response()
}

#[derive(Deserialize)]
pub(super) struct OrdersStatusForm {
    pub(super) order_id: String,
    pub(super) status: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
pub(super) struct OrdersTrackingForm {
    pub(super) order_id: String,
    pub(super) tracking: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn orders_status(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<OrdersStatusForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_id = if Role::from_role_str(&session.role).has_level(Role::Owner) {
        0i64
    } else {
        match session.seller_id {
            Some(v) => v,
            None => return Redirect::to(&state.url("/orders")).into_response(),
        }
    };
    let allowed = ["pending", "shipped", "delivered", "refunded", "cancelled"];
    if !allowed.contains(&f.status.as_str()) {
        return Redirect::to(&state.url("/orders")).into_response();
    }
    state.db.set_order_status(seller_id, &f.order_id, &f.status);
    Redirect::to(&state.url("/orders?saved=1")).into_response()
}

#[tracing::instrument(skip_all)]
#[allow(dead_code)]
pub(super) async fn orders_tracking(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<OrdersTrackingForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let seller_id = if Role::from_role_str(&session.role).has_level(Role::Owner) {
        0i64
    } else {
        match session.seller_id {
            Some(v) => v,
            None => return Redirect::to(&state.url("/orders")).into_response(),
        }
    };
    state
        .db
        .set_order_tracking(seller_id, &f.order_id, &f.tracking);
    Redirect::to(&state.url("/orders?saved=1")).into_response()
}
