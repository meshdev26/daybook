use super::*;
/// Styled 404 page.
#[tracing::instrument(skip_all)]
pub(super) async fn not_found(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
) -> impl IntoResponse {
    let session = session_from_headers(&state, &headers);
    let body = format!(
        r#"<h1>404 — Page not found</h1><div class="alert alert-error" role="alert">The page you requested does not exist.</div><p><a href="{}">Back to dashboard</a></p>"#,
        state.url("/dashboard")
    );
    (
        StatusCode::NOT_FOUND,
        Html(html_page(&state, session.as_ref(), "Not found", &body)),
    )
        .into_response()
}
/// 403-style refusal page in the console shell: names what is restricted and
/// stops there — no partial content renders.
pub(super) fn forbidden_page(state: &WebState, page: &str, what: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Html(html_page(
            state,
            None,
            page,
            &format!(
                r#"<h1>{}</h1><div class="alert alert-error" role="alert">{} is restricted to the console owner.</div><p><a href="{}">Back to the dashboard</a></p>"#,
                esc(page),
                esc(what),
                state.url("/dashboard")
            ),
        )),
    )
        .into_response()
}

// ---- Privacy / compliance (Phase 6) ----

#[derive(Deserialize)]
pub(super) struct PrivacyQuery {
    pub(super) seller_id: Option<String>,
    pub(super) user_id: Option<String>,
    pub(super) deleted: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn privacy_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<PrivacyQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !is_admin_role(&session.role) {
        return forbidden_page(&state, "Privacy", "Buyer data export and deletion");
    }
    let seller_val = q.seller_id.clone().unwrap_or_default();
    let user_val = q.user_id.clone().unwrap_or_default();
    let banner = if q.deleted.as_deref() == Some("1") {
        r#"<div class="alert alert-success" role="status" aria-live="polite">Buyer data deleted.</div>"#.to_string()
    } else {
        String::new()
    };
    let body = format!(
        r#"{banner}<h1>Privacy <span class="sub">buyer data export and deletion</span></h1>
<article class="panel"><h3>Export buyer data</h3>
<p class="help">Download a JSON export of all stored data for one buyer (owner/admin).</p>
<form method="get" action="{}">
<label for="export-seller-id">Seller ID</label><input id="export-seller-id" type="number" name="seller_id" value="{}" required>
<label for="export-user-id">Buyer user ID</label><input id="export-user-id" type="number" name="user_id" value="{}" required>
<button type="submit">Download export</button>
</form></article>
<article class="panel"><h3>Delete buyer data</h3>
<p class="help">Permanently removes all stored data for one buyer. Owner only. Requires confirmation.</p>
<form method="post" action="{}">
<label for="delete-seller-id">Seller ID</label><input id="delete-seller-id" type="number" name="seller_id" value="{}" required>
<label for="delete-user-id">Buyer user ID</label><input id="delete-user-id" type="number" name="user_id" value="{}" required>
<label>Type DELETE to confirm<input type="text" name="confirm" required autocomplete="off"></label>
<button type="submit" class="btn btn-danger" onclick="return confirm('Delete all buyer data? This cannot be undone.')">Delete buyer data</button>
</form></article>"#,
        state.url("/privacy/export"),
        esc(&seller_val),
        esc(&user_val),
        state.url("/privacy/delete"),
        esc(&seller_val),
        esc(&user_val),
    );
    Html(html_page(&state, Some(&session), "Privacy", &body)).into_response()
}

#[derive(Deserialize)]
pub(super) struct PrivacyExportQuery {
    pub(super) seller_id: String,
    pub(super) user_id: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn privacy_export(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<PrivacyExportQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !is_admin_role(&session.role) {
        return forbidden_page(&state, "Privacy", "Buyer data export").into_response();
    }
    let seller_id: i64 = match q.seller_id.trim().parse() {
        Ok(v) => v,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "invalid seller_id".to_string()).into_response();
        }
    };
    let user_id: i64 = match q.user_id.trim().parse() {
        Ok(v) => v,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "invalid user_id".to_string()).into_response();
        }
    };
    let data = state.db.export_buyer_data(seller_id, user_id);
    let filename = format!("buyer-{}-seller-{}.json", user_id, seller_id);
    (
        StatusCode::OK,
        AppendHeaders([
            (
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename),
            ),
        ]),
        data,
    )
        .into_response()
}

#[derive(Deserialize)]
pub(super) struct PrivacyDeleteForm {
    pub(super) seller_id: String,
    pub(super) user_id: String,
    pub(super) confirm: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn privacy_delete(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PrivacyDeleteForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if session.role != "owner" {
        return forbidden_page(&state, "Privacy", "Buyer data deletion");
    }
    if f.confirm.as_deref() != Some("DELETE") {
        return Html(html_page(
            &state,
            Some(&session),
            "Privacy",
            r#"<div class="alert alert-error" role="alert">Confirmation required: type DELETE.</div><p><a href="/privacy">Back</a></p>"#,
        ))
        .into_response();
    }
    let seller_id: i64 = match f.seller_id.trim().parse() {
        Ok(v) => v,
        Err(_) => {
            return Html(html_page(
                &state,
                Some(&session),
                "Privacy",
                r#"<div class="alert alert-error" role="alert">Invalid seller_id.</div>"#,
            ))
            .into_response();
        }
    };
    let user_id: i64 = match f.user_id.trim().parse() {
        Ok(v) => v,
        Err(_) => {
            return Html(html_page(
                &state,
                Some(&session),
                "Privacy",
                r#"<div class="alert alert-error" role="alert">Invalid user_id.</div>"#,
            ))
            .into_response();
        }
    };
    let _ = state.db.delete_buyer_data(seller_id, user_id);
    Redirect::to(&state.url("/privacy?deleted=1")).into_response()
}
