use super::*;
// ---------- Bookings (BOOKINGS_UX_SPEC) ----------

/// Table row cap with the footer note (spec §3.3/§10.5): never silently
/// truncate — `Showing the first 200 bookings.` renders when it bites.
pub(super) const BOOKINGS_ROW_CAP: usize = 200;
/// Count/audit ceiling for the "all rows" fetch the counts and the failure
/// classifier are derived from (the subs_export precedent). Counts must not
/// be wrong when the row cap clips the table (§10.5).
pub(super) const BOOKINGS_COUNT_LIMIT: i64 = 100_000;

/// Booking status → badge class, the §3.4 allow-list (the `order_badge`
/// idiom). The status word always renders beside the class, so color is
/// never the only channel; the class is chosen from this map, never from raw
/// input.
pub(super) fn booking_badge(status: &str) -> &'static str {
    match status {
        "held" => "badge badge-warning",
        "confirmed" => "badge badge-success",
        "expired" | "declined" => "badge badge-danger",
        _ => "badge",
    }
}

/// The console list/write scope (spec §3.7): derived from the session role
/// only — `Role` level, never a query or form value. `Some(None)` =
/// admin/owner, unscoped; `Some(Some(sid))` = that seller's rows; `None` =
/// a seller account with no seller binding, which may list and mutate
/// nothing (the Db scope `None` would be unscoped, so it must not be passed).
pub(super) fn booking_seller_scope(session: &Session) -> Option<Option<i64>> {
    if Role::from_role_str(&session.role).has_level(Role::Admin) {
        Some(None)
    } else {
        session.seller_id.map(Some)
    }
}

/// Unknown or missing `status` normalizes to `all` silently — shared links
/// and typed URLs stay forgiving (§3.1).
pub(super) fn normalize_booking_status(s: Option<&str>) -> String {
    match s {
        Some("held") => "held".to_string(),
        Some("confirmed") => "confirmed".to_string(),
        Some("expired") => "expired".to_string(),
        Some("declined") => "declined".to_string(),
        Some("cancelled") => "cancelled".to_string(),
        _ => "all".to_string(),
    }
}

#[derive(Deserialize)]
pub(super) struct BookingsQuery {
    pub(super) status: Option<String>,
    // PRG flash params (§8.1): done=confirm|decline|cancel and
    // error=invalid|notfound|forbidden|slot|state, with `b` carrying the
    // booking token (an 8-char capability reference, not an identity — §6.4).
    pub(super) done: Option<String>,
    pub(super) error: Option<String>,
    pub(super) b: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn bookings_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<BookingsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    Html(html_page(
        &state,
        Some(&session),
        "Bookings",
        &bookings_body(&state, &session, &q),
    ))
    .into_response()
}

fn bookings_banner_slot(q: &BookingsQuery) -> String {
    // Outcome banners (the subs_page idiom): already-escaped HTML collecting
    // into a list. On a banner-bearing render the container takes focus on
    // load (§5.6, script-free via autofocus on a tabindex="-1" wrapper).
    let mut banners: Vec<String> = Vec::new();
    let b = q.b.as_deref().map(esc).unwrap_or_default();
    match q.done.as_deref() {
        Some("confirm") => banners.push(format!(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Booking {} confirmed.</div>"#,
            b
        )),
        Some("decline") => banners.push(format!(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Booking {} declined.</div>"#,
            b
        )),
        Some("cancel") => banners.push(format!(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Booking {} cancelled. The slot is free again.</div>"#,
            b
        )),
        _ => {}
    }
    if let Some(err) = q.error.as_deref() {
        let msg = match err {
            "invalid" => "Invalid booking id.".to_string(),
            "notfound" => "Booking not found or already closed.".to_string(),
            "forbidden" => "Booking actions are limited to your own bookings.".to_string(),
            "slot" => format!(
                "Could not confirm booking {}: the slot is no longer available.",
                b
            ),
            "state" => format!("Booking {} can no longer be changed.", b),
            _ => String::new(),
        };
        if !msg.is_empty() {
            banners.push(format!(
                r#"<div class="alert alert-error" role="alert">{}</div>"#,
                msg
            ));
        }
    }
    if banners.is_empty() {
        String::new()
    } else {
        format!(
            r#"<div tabindex="-1" autofocus role="status">{}</div>"#,
            banners.join("\n")
        )
    }
}

fn bookings_counts(all_rows: &[crate::db::BookingListRow]) -> String {
    let mut held = 0i64;
    let mut confirmed = 0i64;
    let mut expired = 0i64;
    for r in all_rows {
        match r.6.as_str() {
            "held" => held += 1,
            "confirmed" => confirmed += 1,
            "expired" => expired += 1,
            _ => {}
        }
    }
    let total = all_rows.len() as i64;
    let cards = format!(
        r#"<section aria-labelledby="bookings-counts-heading"><h2 id="bookings-counts-heading">Status counts</h2><div class="grid-stats">
<div class="panel"><div class="stat"><span class="stat-value">{held}</span><span class="stat-label">Held</span></div></div>
<div class="panel"><div class="stat"><span class="stat-value">{confirmed}</span><span class="stat-label">Confirmed</span></div></div>
<div class="panel"><div class="stat"><span class="stat-value">{expired}</span><span class="stat-label">Expired</span></div></div>
<div class="panel"><div class="stat"><span class="stat-value">{total}</span><span class="stat-label">Total</span></div></div>
</div></section>"#,
        held = held,
        confirmed = confirmed,
        expired = expired,
        total = total
    );
    cards
}

fn bookings_filter_nav(state: &WebState, status_filter: &String) -> String {
    // Filter chips (the orders_page idiom): real links, the active one marked
    // aria-current="page". Filter state lives in the URL only (§3.1).
    let statuses = [
        "all",
        "held",
        "confirmed",
        "expired",
        "declined",
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
                state.url(&format!("/bookings?status={}", s)),
                aria,
                s
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let filter_nav = format!(
        r#"<nav aria-label="Filter bookings by status" class="inline-form">{}</nav>"#,
        filter_links
    );
    filter_nav
}

fn bookings_table_section(
    state: &WebState,
    scope: Option<i64>,
    may_list: bool,
    status_filter: &String,
    all_rows: &[crate::db::BookingListRow],
    is_admin: bool,
) -> (String, bool) {
    // Table rows: SQL-level status filter + LIMIT via the Db call (§10.1 —
    // filter-after-LIMIT would hide matching rows), capped at 200.
    let rows_html: String;
    let truncated: bool;
    if status_filter == "all" {
        truncated = all_rows.len() > BOOKINGS_ROW_CAP;
        rows_html = bookings_rows_html(state, all_rows, is_admin);
    } else {
        let filtered = if may_list {
            state
                .db
                .list_bookings_filtered(scope, Some(status_filter), BOOKINGS_ROW_CAP as i64)
        } else {
            Vec::new()
        };
        truncated = all_rows.iter().filter(|r| r.6 == *status_filter).count() > BOOKINGS_ROW_CAP;
        rows_html = bookings_rows_html(state, &filtered, is_admin);
    }

    let table_or_empty = if rows_html.is_empty() {
        if status_filter == "all" {
            // §8.2.1 — no bookings in scope at all.
            r#"<p class="empty"><b>No bookings yet.</b> Buyers book a slot with /book &lt;service_name&gt; in the bot — add slots with /slotadd.</p>"#.to_string()
        } else {
            // §8.2.2 — the filter just matches nothing; short, with a way out.
            format!(
                r#"<p class="empty">No {} bookings. <a href="{}">Show all bookings</a></p>"#,
                esc(status_filter),
                state.url("/bookings")
            )
        }
    } else {
        let head = if is_admin {
            "<tr><th scope=\"col\">Booking</th><th scope=\"col\">Seller</th><th scope=\"col\">Buyer</th><th scope=\"col\">Service</th><th scope=\"col\">Slot</th><th scope=\"col\">Status</th><th scope=\"col\">Expires</th><th scope=\"col\">Actions</th></tr>"
        } else {
            "<tr><th scope=\"col\">Booking</th><th scope=\"col\">Buyer</th><th scope=\"col\">Service</th><th scope=\"col\">Slot</th><th scope=\"col\">Status</th><th scope=\"col\">Expires</th><th scope=\"col\">Actions</th></tr>"
        };
        format!(
            r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Bookings"><table aria-label="Bookings"><caption class="sr-only">Bookings with status, slot time and response deadline</caption><thead>{}</thead><tbody>{}</tbody></table></div>"#,
            head, rows_html
        )
    };
    (table_or_empty, truncated)
}

fn bookings_services_panel(
    state: &WebState,
    scope: Option<i64>,
    may_list: bool,
    is_admin: bool,
) -> String {
    // Services panel (read-only, D8): no CRUD affordances anywhere — the
    // help line is the pointer to the bot commands. Same scope rule as the
    // table: an unbound seller session lists nothing.
    let services = if may_list {
        state.db.list_services(scope)
    } else {
        Vec::new()
    };
    let services_content = if services.is_empty() {
        r#"<p class="empty"><b>No services yet.</b> Add one with /serviceadd in the bot, then add slots with /slotadd.</p>"#.to_string()
    } else {
        let now = chrono::Utc::now();
        let head = if is_admin {
            "<tr><th scope=\"col\">Service</th><th scope=\"col\">Price</th><th scope=\"col\">Duration</th><th scope=\"col\" class=\"num\">Available slots</th><th scope=\"col\">Seller</th></tr>"
        } else {
            "<tr><th scope=\"col\">Service</th><th scope=\"col\">Price</th><th scope=\"col\">Duration</th><th scope=\"col\" class=\"num\">Available slots</th></tr>"
        };
        let rows = services
            .iter()
            .map(|s| {
                // Available slots: status 'available' AND starts in the
                // future (spec §3.6); unparseable times are not counted.
                let available = state
                    .db
                    .list_slots(s.0)
                    .iter()
                    .filter(|slot| {
                        slot.4 == "available"
                            && chrono::DateTime::parse_from_rfc3339(&slot.1)
                                .map(|dt| dt.with_timezone(&chrono::Utc) > now)
                                .unwrap_or(false)
                    })
                    .count();
                let seller_cell = if is_admin {
                    format!(r#"<td class="mono">{}</td>"#, s.1)
                } else {
                    String::new()
                };
                format!(
                    r#"<tr><td>{}</td><td class="mono">{} {}</td><td>{} min</td><td class="num">{}</td>{}</tr>"#,
                    esc(&s.2),
                    esc(&s.3),
                    esc(&s.4),
                    s.5,
                    available,
                    seller_cell
                )
            })
            .collect::<Vec<_>>()
            .join("");
        format!(
            r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Services"><table aria-label="Services"><caption class="sr-only">Services</caption><thead>{}</thead><tbody>{}</tbody></table></div>"#,
            head, rows
        )
    };
    let services_panel = format!(
        r#"<article class="panel"><h3>Services</h3>
<p class="help">Services are managed in the bot: /serviceadd to add a service, /slotadd to add slots. Buyers see /services.</p>
{}</article>"#,
        services_content
    );
    services_panel
}

pub(super) fn bookings_body(state: &WebState, session: &Session, q: &BookingsQuery) -> String {
    let is_admin = Role::from_role_str(&session.role).has_level(Role::Admin);
    let (scope, may_list) = match booking_seller_scope(session) {
        Some(s) => (s, true),
        None => (None, false),
    };
    let status_filter = normalize_booking_status(q.status.as_deref());
    let banner_slot = bookings_banner_slot(q);
    let all_rows = if may_list {
        state
            .db
            .list_bookings_filtered(scope, None, BOOKINGS_COUNT_LIMIT)
    } else {
        Vec::new()
    };
    let cards = bookings_counts(&all_rows);
    let filter_nav = bookings_filter_nav(state, &status_filter);
    let (table_or_empty, truncated) =
        bookings_table_section(state, scope, may_list, &status_filter, &all_rows, is_admin);
    let services_panel = bookings_services_panel(state, scope, may_list, is_admin);
    // Footer: the scoping note (§3.7) plus the truncation note when the cap
    // clipped the table (§10.5).
    let scope_note = if is_admin {
        "Showing all sellers' bookings."
    } else {
        "Showing your bookings only."
    };
    let truncation_note = if truncated {
        format!(
            " {}",
            esc(&format!("Showing the first {} bookings.", BOOKINGS_ROW_CAP))
        )
    } else {
        String::new()
    };
    format!(
        r#"<h1>Bookings <span class="sub">service appointments and responses</span></h1>
{banner_slot}
{cards}
{filter_nav}
{table_or_empty}
{services_panel}
<p class="help">{scope_note}{truncation_note}</p>"#,
        banner_slot = banner_slot,
        cards = cards,
        filter_nav = filter_nav,
        table_or_empty = table_or_empty,
        services_panel = services_panel,
        scope_note = scope_note,
        truncation_note = truncation_note,
    )
}

/// One table row per booking (§3.3): the public 8-char token as the id, the
/// Seller column only in the admin/owner view, "—" for the NULL slot and
/// expiry of legacy rows, badge text from the §3.4 map, actions per the
/// §3.5 status × role matrix. Every dynamic cell passes through `esc()`.
pub(super) fn bookings_rows_html(
    state: &WebState,
    rows: &[crate::db::BookingListRow],
    is_admin: bool,
) -> String {
    rows.iter()
        .map(|r| {
            let actions = match r.6.as_str() {
                // held → the seller-response pair; expired → the same pair,
                // where Confirm is the D3 resurrection attempt (the slot
                // race is decided server-side, §3.5).
                "held" | "expired" => format!(
                    r#"<form method="post" action="{action}" class="inline-form"><input type="hidden" name="booking_id" value="{id}"><input type="hidden" name="booking_ref" value="{token}"><input type="hidden" name="action" value="confirm"><button type="submit" class="btn btn-sm" aria-label="Confirm booking {token}">Confirm</button></form>
<form method="post" action="{action}" class="inline-form"><input type="hidden" name="booking_id" value="{id}"><input type="hidden" name="booking_ref" value="{token}"><input type="hidden" name="action" value="decline"><button type="submit" class="btn btn-danger btn-sm" aria-label="Decline booking {token}" onclick="return confirm('Decline booking {token}? The buyer is notified and the slot is freed.')">Decline</button></form>"#,
                    action = state.url("/bookings/status"),
                    id = r.0,
                    token = esc(&r.1),
                ),
                // confirmed → Cancel is the only affordance (destructive,
                // confirm-first). Terminal rows carry no actions.
                "confirmed" => format!(
                    r#"<form method="post" action="{action}" class="inline-form"><input type="hidden" name="booking_id" value="{id}"><input type="hidden" name="booking_ref" value="{token}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Cancel booking {token}" onclick="return confirm('Cancel booking {token}? The buyer is notified and the slot is freed.')">Cancel</button></form>"#,
                    action = state.url("/bookings/cancel"),
                    id = r.0,
                    token = esc(&r.1),
                ),
                _ => String::new(),
            };
            let seller_cell = if is_admin {
                format!(
                    r#"<td class="mono">{}</td>"#,
                    r.8.map(|sid| sid.to_string())
                        .unwrap_or_else(|| "—".to_string())
                )
            } else {
                String::new()
            };
            format!(
                r#"<tr><td class="mono">{}</td>{}<td class="mono">{}</td><td>{}</td><td class="mono">{}</td><td><span class="{}">{}</span></td><td class="mono">{}</td><td>{}</td></tr>"#,
                esc(&r.1),
                seller_cell,
                r.2,
                if r.3.is_empty() { "—".to_string() } else { esc(&r.3) },
                esc(r.4.as_deref().unwrap_or("—")),
                booking_badge(&r.6),
                esc(&r.6),
                esc(r.7.as_deref().unwrap_or("—")),
                actions,
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

#[derive(Deserialize)]
pub(super) struct BookingActionForm {
    /// The integer booking ROW id — what the Db methods act on. The 8-char
    /// public token rides along as `booking_ref` for the flash banner and
    /// the audit detail (§10.6); it is escaped on render and never trusted
    /// for authorization.
    pub(super) booking_id: String,
    pub(super) action: String,
    pub(super) booking_ref: Option<String>,
}

/// PRG redirect back to the page with a flash key; `b` carries the booking
/// token (qenc'd, §8.1).
pub(super) fn booking_redirect(state: &WebState, key: &str, b: Option<&str>) -> Response {
    let query = match b {
        Some(token) => format!("{}&b={}", key, qenc(token)),
        None => key.to_string(),
    };
    Redirect::to(&state.url(&format!("/bookings?{}", query))).into_response()
}

/// Classify a refused scoped mutation. The Db returns a bare bool by design
/// (no oracle), so the follow-up read keys off the scoped list: a row the
/// session cannot see is `notfound` — one message for foreign and unknown
/// ids, the §4.5 bot vocabulary; a still-legal row the confirm lost is the
/// D3 slot race (§3.5) → `slot`; everything else is terminal → `state`.
pub(super) fn booking_failure_redirect(
    state: &WebState,
    scope: Option<i64>,
    id: i64,
    confirm: bool,
) -> Response {
    let rows = state
        .db
        .list_bookings_filtered(scope, None, BOOKINGS_COUNT_LIMIT);
    match rows.iter().find(|r| r.0 == id) {
        None => booking_redirect(state, "error=notfound", None),
        Some(r) => {
            let legal_now = r.6 == "held" || r.6 == "expired";
            if confirm && legal_now {
                booking_redirect(state, "error=slot", Some(&r.1))
            } else {
                booking_redirect(state, "error=state", Some(&r.1))
            }
        }
    }
}

/// POST /bookings/status — the seller-response pair (§3.5): `action ∈
/// confirm|decline` mapped onto set_booking_status, which only accepts the
/// held/expired → confirmed/declined transitions inside the seller's scope.
#[tracing::instrument(skip_all)]
pub(super) async fn bookings_status(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<BookingActionForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let scope = match booking_seller_scope(&session) {
        Some(s) => s,
        None => return booking_redirect(&state, "error=forbidden", None),
    };
    let id = match f.booking_id.trim().parse::<i64>() {
        Ok(id) => id,
        Err(_) => return booking_redirect(&state, "error=invalid", None),
    };
    // Allow-list in code: anything else is a bad form, not a state change.
    let (target, audit_action, confirm) = match f.action.as_str() {
        "confirm" => ("confirmed", "booking_confirm", true),
        "decline" => ("declined", "booking_decline", false),
        _ => return booking_redirect(&state, "error=invalid", None),
    };
    if state.db.set_booking_status(id, scope, target) {
        state.db.log_action(
            session.account_id,
            &session.role,
            audit_action,
            "booking",
            f.booking_ref.as_deref().unwrap_or(&f.booking_id),
            None,
        );
        booking_redirect(
            &state,
            &format!("done={}", f.action),
            f.booking_ref.as_deref(),
        )
    } else {
        booking_failure_redirect(&state, scope, id, confirm)
    }
}

/// POST /bookings/cancel — admin/owner any row, seller own (§1): the Db's
/// cancel_booking_scoped accepts held|confirmed → cancelled and frees the
/// slot only when the hold is this booking's own.
#[tracing::instrument(skip_all)]
pub(super) async fn bookings_cancel(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<BookingActionForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let scope = match booking_seller_scope(&session) {
        Some(s) => s,
        None => return booking_redirect(&state, "error=forbidden", None),
    };
    let id = match f.booking_id.trim().parse::<i64>() {
        Ok(id) => id,
        Err(_) => return booking_redirect(&state, "error=invalid", None),
    };
    if state.db.cancel_booking_scoped(id, scope) {
        state.db.log_action(
            session.account_id,
            &session.role,
            "booking_cancel",
            "booking",
            f.booking_ref.as_deref().unwrap_or(&f.booking_id),
            None,
        );
        booking_redirect(&state, "done=cancel", f.booking_ref.as_deref())
    } else {
        booking_failure_redirect(&state, scope, id, false)
    }
}

// The old POST /orders/refund handler was removed: it moved real money with
// no approval step, no audit event and a swallowed transfer failure
// (`let _ = cp.transfer`), and no page ever linked to it. The audited money
// path is the Finance page: Orders links each live order to the Finance
// refund-create form, and finance_refund_execute records
// refund_create/approve/execute + moves the transfer through Crypto Pay.
