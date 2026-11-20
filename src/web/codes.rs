use super::*;
// ---------- Codes ----------

#[derive(Deserialize)]
pub(super) struct CodesGenerateForm {
    pub(super) plan_name: Option<String>,
    pub(super) count: Option<String>,
    pub(super) days: Option<String>,
    pub(super) max_uses: Option<String>,
    pub(super) length: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn codes_page(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Query(q): Query<SubsQuery>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    let banner = codes_banner(&q);
    let body = codes_body(&state, &session, &banner);
    Html(html_page(&state, Some(&session), "Codes", &body)).into_response()
}

/// The codes page body, parameterized on the banner slot so the reclaim
/// handler can render its one-time replacement code in place (EDIT_FLOW_SPEC
/// E4 — no PRG: the code must not ride a URL).
fn codes_body(state: &WebState, session: &Session, banner: &str) -> String {
    let scope = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id
    } else {
        None
    };
    let bot_scope = bot_scope_sellers(state, session);
    let codes = match &bot_scope {
        Some(set) => state.db.list_codes_for_sellers(set, 200),
        None => state.db.list_codes(scope, 200),
    };
    let mut plans = state.db.list_plans(scope);
    plans.retain(|p| in_bot_scope(&bot_scope, p.6));
    let plan_options = plans
        .iter()
        .map(|p| format!(r#"<option value="{}">{}</option>"#, esc(&p.1), esc(&p.1)))
        .collect::<Vec<_>>()
        .join("");
    let rows_html = codes_rows_html(state, session, &codes);
    let table = format!(
        r#"<div class="table-wrap" tabindex="0" role="region" aria-label="Codes"><table aria-label="Codes"><thead><tr><th scope="col">Code</th><th scope="col">Plan</th><th scope="col">Days</th><th scope="col">Uses</th><th scope="col">Max</th><th scope="col">Actions</th></tr></thead><tbody>{}</tbody></table></div>"#,
        rows_html
    );
    let form = format!(
        r#"<article class="panel"><h3>Generate codes</h3>
<form method="post" action="{}">
<label for="plan_name">Plan</label><select id="plan_name" name="plan_name"><option value="">(none)</option>{}</select>
<label for="count">Count</label><input id="count" type="number" name="count" min="1" value="5" required>
<label for="days">Days</label><input id="days" type="number" name="days" min="1" value="30">
<label for="max_uses">Max uses</label><input id="max_uses" type="number" name="max_uses" min="1" value="1">
<label for="length">Length</label><input id="length" type="number" name="length" min="1" max="32" value="12">
<button type="submit">Generate</button>
</form></article>"#,
        state.url("/codes/generate"),
        plan_options
    );
    format!(
        r#"{banner}<h1>Codes <span class="sub">activation codes</span></h1>
{form}
{table}"#,
        banner = banner,
        form = form,
        table = table
    )
}

type CodeRow = (String, Option<i64>, Option<i64>, i64, i64, i64, bool);

fn codes_rows_html(state: &WebState, session: &Session, codes: &[CodeRow]) -> String {
    if codes.is_empty() {
        return r#"<tr><td colspan="6" class="empty"><b>No codes yet.</b> Generate a batch above — hand them out with /redeem.</td></tr>"#.to_string();
    }
    codes
        .iter()
        .map(|c| {
            let revoked_badge = if c.6 {
                r#" <span class="badge badge-danger">Revoked</span>"#
            } else {
                ""
            };
            let actions = if Role::from_role_str(&session.role).has_level(Role::Owner) {
                format!(
                    r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="code" value="{}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Delete code {}" onclick="return confirm('Delete code {}?')">Delete</button></form>"#,
                    state.url("/codes/delete"),
                    esc(&c.0),
                    esc(&c.0),
                    esc(&c.0)
                )
            } else {
                String::new()
            };
            // EDIT_FLOW_SPEC E4: reclaim revokes the old code and mints a
            // replacement — keys-revoke-weight destructive, so danger + a
            // confirm naming the code and the consequence. The per-row
            // "New length" field is gone (the 12 default stays server-side).
            let reclaim = format!(
                r#"<form method="post" action="{}" class="inline-form"><input type="hidden" name="code" value="{}"><button type="submit" class="btn btn-danger btn-sm" aria-label="Reclaim code {}" onclick="return confirm('Reclaim code {}? The old code stops working and a new one is issued.')">Reclaim</button></form>"#,
                state.url("/codes/reclaim"),
                esc(&c.0),
                esc(&c.0),
                esc(&c.0)
            );
            format!(
                r#"<tr><td class="mono">{}{}</td><td>{}</td><td class="num">{}</td><td class="num">{}</td><td class="num">{}</td><td>{}{}</td></tr>"#,
                esc(&c.0),
                revoked_badge,
                c.1.map(|v| v.to_string()).unwrap_or_else(|| "—".to_string()),
                c.2.map(|v| v.to_string()).unwrap_or_else(|| "—".to_string()),
                c.3,
                c.4,
                actions,
                reclaim
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Codes-page flash keys (EDIT_FLOW_SPEC E4/E5): the generate count and the
/// delete outcome. The former raw `?error=` arm is gone — it had no
/// producer and only invited rendering a raw URL key (the Plans bug).
fn codes_banner(q: &SubsQuery) -> String {
    if let Some(nstr) = q.id.as_ref() {
        format!(
            r#"<div class="alert alert-success" role="status" aria-live="polite">Generated {} code(s).</div>"#,
            esc(nstr)
        )
    } else if q.deleted.as_deref() == Some("1") {
        r#"<div class="alert alert-success" role="status" aria-live="polite">Code deleted.</div>"#
            .to_string()
    } else {
        String::new()
    }
}

#[derive(Deserialize)]
pub(super) struct CodesDeleteForm {
    pub(super) code: String,
}

#[tracing::instrument(skip_all)]
pub(super) async fn codes_generate(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<CodesGenerateForm>,
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
            None => return Redirect::to(&state.url("/codes")).into_response(),
        }
    };
    let _scope = if Role::from_role_str(&session.role) == Role::Seller {
        session.seller_id
    } else {
        None
    };
    let plan_id = if let Some(name) = f.plan_name.as_deref().filter(|s| !s.trim().is_empty()) {
        state.db.get_plan(seller_id, name).map(|r| r.0)
    } else {
        None
    };
    let count: i64 = f
        .count
        .as_deref()
        .unwrap_or("1")
        .parse()
        .unwrap_or(1)
        .clamp(1, 100);
    let days: i64 = f.days.as_deref().unwrap_or("30").parse().unwrap_or(30);
    let max_uses: i64 = f
        .max_uses
        .as_deref()
        .unwrap_or("1")
        .parse()
        .unwrap_or(1)
        .max(1);
    let length: usize = f
        .length
        .as_deref()
        .unwrap_or("12")
        .parse()
        .unwrap_or(12)
        .clamp(1, 32) as usize;
    for _ in 0..count {
        state
            .db
            .create_code(seller_id, plan_id, Some(days), max_uses, None, length);
    }
    Redirect::to(&state.url(&format!("/codes?id={}", count))).into_response()
}

#[tracing::instrument(skip_all)]
pub(super) async fn codes_delete(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<CodesDeleteForm>,
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
    state.db.delete_code_seller_scoped(&f.code, scope);
    Redirect::to(&state.url("/codes?deleted=1")).into_response()
}

#[derive(Deserialize)]
pub(super) struct CodesReclaimForm {
    pub(super) code: String,
    pub(super) length: Option<String>,
}

#[tracing::instrument(skip_all)]
pub(super) async fn codes_reclaim(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<CodesReclaimForm>,
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
    // The inline length field is gone (E4); the 12 default stays here.
    let length: usize = f
        .length
        .as_deref()
        .unwrap_or("12")
        .parse()
        .unwrap_or(12)
        .clamp(1, 32) as usize;
    let banner = match state.db.reclaim_code(&f.code, scope, length) {
        Some(new) => {
            // One-time secret display (keys_setup.rs:145-148 idiom): the
            // replacement code renders here and nowhere else — never logged,
            // never stored in the URL, never re-shown by a later redirect.
            format!(
                r#"<div class="alert alert-success" role="status" aria-live="polite"><p><strong>Code reclaimed.</strong> The old code stops working. New code (shown once): <code class="copyable">{}</code></p><p class="help">You won't see this code again. Hand it out now.</p></div>"#,
                esc(&new)
            )
        }
        None => {
            // Honest nothing-happened: unknown code or another seller's code.
            r#"<div class="alert alert-error" role="alert">Reclaim failed — that code was not found.</div>"#
                .to_string()
        }
    };
    let body = codes_body(&state, &session, &banner);
    Html(html_page(&state, Some(&session), "Codes", &body)).into_response()
}
