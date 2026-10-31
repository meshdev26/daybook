use super::*;
// ---------- Cart pages ----------

#[derive(Deserialize)]
pub(super) struct CartAddForm {
    pub(super) item_type: String,
    pub(super) item_id: String,
    pub(super) qty: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct CartRemoveForm {
    pub(super) seller_id: String,
    pub(super) item_type: String,
    pub(super) item_id: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn cart_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let buyer_id = session.account_id;
    let items = state.db.cart_get_all_items(buyer_id);
    let mut rows = String::new();
    let mut seen: std::collections::HashSet<(i64, String, i64)> = std::collections::HashSet::new();
    for item in &items {
        let (seller_id, _row_id, item_type, item_id, qty) = item;
        if !seen.insert((*seller_id, item_type.clone(), *item_id)) {
            continue;
        }
        rows.push_str(&format!(
            r#"<tr><td>{}</td><td>{}</td><td>{}</td><td><form method="post" action="{}" class="inline-form"><input type="hidden" name="seller_id" value="{}"><input type="hidden" name="item_type" value="{}"><input type="hidden" name="item_id" value="{}"><button type="submit" class="btn btn-danger btn-sm">Remove</button></form></td></tr>"#,
            esc(item_type),
            item_id,
            qty,
            state.url("/cart/remove"),
            seller_id,
            esc(item_type),
            item_id
        ));
    }
    // Clear-cart renders only when the table has rows (B10.1) — a destructive
    // button pointing at an empty cart is a dead affordance.
    let table = if rows.is_empty() {
        r#"<p class="empty"><b>Your cart is empty.</b> Add an item above or browse /shop in the bot.</p>"#.to_string()
    } else {
        format!(
            r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Cart"><table aria-label="Cart"><thead><tr><th scope="col">Item type</th><th scope="col">Item ID</th><th scope="col">Qty</th><th scope="col">Actions</th></tr></thead><tbody>{}</tbody></table></div>
<form method="post" action="{}" class="inline-form"><button type="submit" class="btn btn-danger btn-sm">Clear cart</button></form>"#,
            rows,
            state.url("/cart/clear")
        )
    };
    let body = format!(
        r#"<h1>Cart <span class="sub">your cart</span></h1>
<article class="panel"><h3>Add item</h3>
<form method="post" action="{}">
<label>Item type <select name="item_type"><option value="product">product</option><option value="plan">plan</option></select></label>
<label>Item ID <input type="number" name="item_id" required></label>
<label>Qty <input type="number" name="qty" value="1" min="1" required></label>
<button type="submit">Add</button>
</form></article>
<article class="panel"><h3>Items</h3>
{}
</article>"#,
        state.url("/cart/add"),
        table
    );
    Html(html_page(&state, Some(&session), "Cart", &body)).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn cart_add(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<CartAddForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let buyer_id = session.account_id;
    let seller_id = session.seller_id.unwrap_or(0);
    let item_type = if f.item_type.trim() == "plan" {
        "plan"
    } else {
        "product"
    };
    let item_id: i64 = match f.item_id.trim().parse() {
        Ok(v) if v > 0 => v,
        _ => return Redirect::to(&state.url("/cart")).into_response(),
    };
    let qty: i64 = f
        .qty
        .as_deref()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1)
        .max(1);
    let _ = state
        .db
        .cart_add_item(buyer_id, seller_id, item_type, item_id, qty);
    Redirect::to(&state.url("/cart")).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn cart_remove(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<CartRemoveForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let buyer_id = session.account_id;
    let seller_id: i64 = f.seller_id.trim().parse().unwrap_or(0);
    let item_type = if f.item_type.trim() == "plan" {
        "plan"
    } else {
        "product"
    };
    if let Ok(item_id) = f.item_id.trim().parse::<i64>() {
        let _ = state
            .db
            .cart_remove_item(buyer_id, seller_id, item_type, item_id);
    }
    Redirect::to(&state.url("/cart")).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn cart_clear_post(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    state.db.cart_clear_all(session.account_id);
    Redirect::to(&state.url("/cart")).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn carts_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !(session.role == "seller" || is_admin_role(&session.role)) {
        return forbidden_page(&state, "Carts", "Active carts").into_response();
    }
    let seller_filter = if session.role == "seller" {
        session.seller_id
    } else {
        None
    };
    let abandoned = state.db.list_abandoned_carts(0);
    let mut rows = String::new();
    let mut seen: std::collections::HashSet<(i64, i64)> = std::collections::HashSet::new();
    for (buyer_id, seller_id, _abandoned_at, _reminded) in abandoned {
        if let Some(sf) = seller_filter
            && seller_id != sf
        {
            continue;
        }
        if !seen.insert((buyer_id, seller_id)) {
            continue;
        }
        let items = state.db.cart_get_all_items(buyer_id);
        let seller_items: Vec<&(i64, i64, String, i64, i64)> =
            items.iter().filter(|i| i.0 == seller_id).collect();
        if seller_items.is_empty() {
            continue;
        }
        let item_count = seller_items.len();
        let total_items: i64 = seller_items.iter().map(|i| i.4).sum();
        rows.push_str(&format!(
            r#"<tr><td class="mono">{}</td><td class="mono">{}</td><td>{}</td><td>{}</td></tr>"#,
            buyer_id, seller_id, item_count, total_items
        ));
    }
    let table = if rows.is_empty() {
        r#"<p class="empty"><b>No active carts.</b> Buyer carts appear here while they shop.</p>"#
            .to_string()
    } else {
        format!(
            r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Active carts"><table aria-label="Active carts"><thead><tr><th scope="col">Buyer ID</th><th scope="col">Seller ID</th><th scope="col">Item count</th><th scope="col">Total items</th></tr></thead><tbody>{}</tbody></table></div>"#,
            rows
        )
    };
    let body = format!(
        r#"<h1>Carts <span class="sub">active buyer carts</span></h1>
<article class="panel">{}</article>"#,
        table
    );
    Html(html_page(&state, Some(&session), "Carts", &body)).into_response()
}
