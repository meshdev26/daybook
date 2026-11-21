use super::*;
// ---------- Promos ----------

/// Promos flash keys (EDIT_FLOW_SPEC E6/E7): create outcome (naming the code
/// — the object key), revoke outcome, and mapped validation errors.
#[derive(Deserialize)]
pub(super) struct PromosQuery {
    pub(super) created: Option<String>,
    pub(super) code: Option<String>,
    pub(super) revoked: Option<String>,
    pub(super) error: Option<String>,
}

fn promos_banner(q: &PromosQuery) -> String {
    if q.created.as_deref() == Some("1") {
        return match q.code.as_deref().filter(|s| !s.is_empty()) {
            Some(code) => format!(
                r#"<div class="alert alert-success" role="status" aria-live="polite">Promo code {} created.</div>"#,
                esc(code)
            ),
            None => r#"<div class="alert alert-success" role="status" aria-live="polite">Promo code created.</div>"#.to_string(),
        };
    }
    if q.revoked.as_deref() == Some("1") {
        return r#"<div class="alert alert-success" role="status" aria-live="polite">Promo code revoked.</div>"#.to_string();
    }
    if let Some(e) = q.error.as_ref() {
        let msg = match e.as_str() {
            "code" => "Enter a code.",
            "type" => "Choose percent or fixed.",
            "value" => "Value must be a number greater than 0.",
            _ => return String::new(),
        };
        return format!(r#"<div class="alert alert-error" role="alert">{msg}</div>"#);
    }
    String::new()
}

#[derive(Deserialize)]
pub(super) struct PromosCreateForm {
    pub(super) code: Option<String>,
    pub(super) promo_type: Option<String>,
    #[serde(rename = "type")]
    pub(super) type_: Option<String>,
    pub(super) value: Option<String>,
    pub(super) max_uses: Option<String>,
    pub(super) min_purchase: Option<String>,
    pub(super) days: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn promos_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<PromosQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let promos = if Role::from_role_str(&session.role) == Role::Seller {
        if let Some(sid) = session.seller_id {
            state.db.list_promo_codes(sid)
        } else {
            vec![]
        }
    } else {
        state.db.list_all_promo_codes()
    };
    let rows_html = if promos.is_empty() {
        r#"<tr><td colspan="8" class="empty"><b>No promo codes yet.</b> Create one above — buyers apply it in /buy.</td></tr>"#.to_string()
    } else {
        promos
            .iter()
            .map(|p| {
                // p: (code, seller_id, promo_type, value, currency, max_uses, uses, min_purchase, scope, scope_id, expires_at, active, created_at)
                let active_badge = if p.11 == 1 {
                    r#"<span class="badge badge-success">Active</span>"#
                } else {
                    r#"<span class="badge badge-danger">Revoked</span>"#
                };
                let max_uses = p
                    .5
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "∞".to_string());
                let redemptions = state
                    .db
                    .list_promo_redemptions(p.1, Some(&p.0))
                    .len();
                let revoke_form = if p.11 == 1 {
                    format!(
                        r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="code" value="{}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Revoke promo code {code}" onclick="return confirm('Revoke code {code}?')">Revoke</button></form>"#,
                        state.url("/promos/revoke"),
                        esc(&p.0),
                        code = esc(&p.0)
                    )
                } else {
                    String::new()
                };
                let expiry = p.10.clone().unwrap_or_else(|| "—".to_string());
                format!(
                    r#"<tr><td class="mono">{}</td><td>{}</td><td class="num">{}</td><td class="num">{}/{}</td><td class="num">{}</td><td>{}</td><td>{}</td><td class="actions">{}</td></tr>"#,
                    esc(&p.0),
                    esc(&p.2),
                    p.3,
                    p.6,
                    max_uses,
                    redemptions,
                    esc(&expiry),
                    active_badge,
                    revoke_form
                )
            })
            .collect::<Vec<_>>()
            .join("")
    };
    let table = format!(
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Promo codes"><table aria-label="Promo codes"><thead><tr><th scope="col">Code</th><th scope="col">Type</th><th scope="col">Value</th><th scope="col">Uses/Max</th><th scope="col">Redemptions</th><th scope="col">Expiry</th><th scope="col">Active</th><th scope="col">Actions</th></tr></thead><tbody>{}</tbody></table></div>"#,
        rows_html
    );
    let form = format!(
        r#"<article class="panel"><h3>Create promo code</h3>
<form method="post" action="{}">
<label for="code">Code</label><input id="code" name="code" required>
<label for="promo_type">Type</label><select id="promo_type" name="promo_type"><option value="percent">percent (% off)</option><option value="fixed">fixed (amount off)</option></select>
<label for="value">Value</label><input id="value" name="value" type="number" step="any" required>
<label for="max_uses">Max uses</label><input id="max_uses" name="max_uses" type="number" min="0" placeholder="unlimited">
<label for="min_purchase">Min purchase</label><input id="min_purchase" name="min_purchase" type="number" step="any" value="0">
<label for="days">Expires after (days)</label><input id="days" name="days" type="number" min="0" value="30"><span class="help">0 = never expires.</span>
<button type="submit">Create</button>
</form></article>"#,
        state.url("/promos")
    );
    let banner = promos_banner(&q);
    let body = format!(
        r#"<h1>Promos <span class="sub">promo codes &amp; discounts</span></h1>
{banner}
{form}
{table}"#,
        banner = banner,
        form = form,
        table = table
    );
    Html(html_page(&state, Some(&session), "Promos", &body)).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn promos_create(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PromosCreateForm>,
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
            None => return Redirect::to(&state.url("/promos")).into_response(),
        }
    };
    let code = f.code.as_deref().map(str::trim).unwrap_or("").to_string();
    // E6: every validation failure used to plain-redirect, silently
    // discarding the whole form — fixed keys with mapped copy now report.
    if code.is_empty() {
        return Redirect::to(&state.url("/promos?error=code")).into_response();
    }
    let promo_type = f
        .promo_type
        .as_deref()
        .or(f.type_.as_deref())
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if promo_type != "percent" && promo_type != "fixed" {
        return Redirect::to(&state.url("/promos?error=type")).into_response();
    }
    let value: f64 = match f.value.as_deref().map(str::trim).unwrap_or("").parse() {
        Ok(v) if v > 0.0 => v,
        _ => return Redirect::to(&state.url("/promos?error=value")).into_response(),
    };
    let max_uses = f
        .max_uses
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&v| v > 0);
    let min_purchase: f64 = f
        .min_purchase
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.0);
    let days: i64 = f
        .days
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let expires_at = if days > 0 {
        Some((chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339())
    } else {
        None
    };
    let expires_ref = expires_at.as_deref();
    let _ = state.db.create_promo_code(
        seller_id,
        &code,
        &promo_type,
        value,
        None,
        max_uses,
        min_purchase,
        "all",
        None,
        expires_ref,
    );
    Redirect::to(&state.url(&format!("/promos?created=1&code={}", qenc(&code)))).into_response()
}

#[derive(Deserialize)]
pub(super) struct PromosRevokeForm {
    pub(super) code: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn promos_revoke(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<PromosRevokeForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let code = f.code.trim().to_string();
    if code.is_empty() {
        return Redirect::to(&state.url("/promos")).into_response();
    }
    if Role::from_role_str(&session.role) == Role::Seller {
        if let Some(sid) = session.seller_id {
            state.db.revoke_promo_code(sid, &code);
        }
    } else {
        state.db.revoke_promo_code_all(&code);
    }
    // E7: the confirm names the code, but the outcome used to be silent.
    Redirect::to(&state.url("/promos?revoked=1")).into_response()
}
