// Cross-cutting surfaces: the esc() property, auth shell behavior, the
// dashboard connect banner, privacy smoke, and broadcast scoping (P1-1).
use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use proptest::prelude::*;
use tower::ServiceExt;

proptest! {
    /// esc() output contains no raw HTML metacharacters (`< > " '` — the
    /// escaped forms use entities), every `&` in the output begins one of
    /// the five known entities, and decoding exactly restores the input.
    #[test]
    fn prop_esc_neutralizes_and_roundtrips(s in ".*") {
        let e = esc(&s);
        for c in ['<', '>', '"', '\''] {
            prop_assert!(!e.contains(c), "raw {c} survived escaping");
        }
        let entities = ["&amp;", "&lt;", "&gt;", "&quot;", "&#39;"];
        for (i, _) in e.match_indices('&').collect::<Vec<_>>() {
            let rest = &e[i..];
            prop_assert!(
                entities.iter().any(|ent| rest.starts_with(ent)),
                "stray & at byte {i} in {e:?}"
            );
        }
        // Decode order: &amp; LAST — restoring & early would fabricate
        // new entity sequences (esc("&#39;") == "&amp;#39;").
        let decoded = e
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&");
        prop_assert_eq!(decoded, s);
    }
}

// ---- UI alignment: global CSS + markup contract (owner screenshot fixes) --

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn console_alignment_utilities_are_served_globally() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    state.db.add_blocked_word(0, "gridcheck", "warn", true);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(get_req("/assets/app.css"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let css = body_string(resp).await;
    // The base rule is byte-identical: the alignment fix is purely additive
    // and must never become a global `button { margin-top: 0 }` override.
    assert!(
        css.contains("form button[type=\"submit\"] { margin-top: 16px; }"),
        "base submit margin changed: {css}"
    );
    assert!(
        !css.contains("button { margin-top: 0"),
        "no global destructive button margin override allowed: {css}"
    );
    for selector in [
        // inline-level again so the web-users Actions forms and the channel
        // trailing help flow horizontally; td.actions hugs them to the right.
        ".inline-form { display: inline-flex; align-items: center; gap: 12px",
        ".inline-form label { display: inline-flex; align-items: center; gap: 8px",
        "td.actions .inline-form { justify-content: flex-end",
        ".form-row { display: flex; align-items: flex-end; gap: 12px",
        ".form-row > label { display: flex; flex-direction: column; gap: 6px",
        // Submit buttons in horizontal forms share their input's baseline;
        // stacked forms keep the base 16px gap below.
        ".inline-form button[type=\"submit\"],",
        ".form-row button[type=\"submit\"] { margin-top: 0; }",
        ".inline-form input:not([type=\"hidden\"]),",
        "min-height: 40px",
        "grid-template-columns: repeat(2, minmax(0, 1fr))",
        "ul.recovery-codes > li",
        "li.moderation-row, .rows li.moderation-row {",
        "grid-template-columns: minmax(0, 1fr) auto auto",
        ".moderation-row > .row-actions",
        ".filter-bar { display: flex; flex-direction: column; gap: 12px",
        // flex-basis:100% is what actually wraps the detail in a flex <li>.
        ".empty .empty-detail { display: block; flex: 0 0 100%",
    ] {
        assert!(css.contains(selector), "missing `{selector}` in app.css");
    }
    // Mobile: grids stack; ≥44px moderation targets stay intact.
    assert!(
        css.contains("grid-template-columns: 1fr; justify-items: stretch"),
        "{css}"
    );
    assert!(
        css.contains(".moderation-row button { min-height: 44px"),
        "{css}"
    );
    // The grid selector must be a real selector for the markup the server
    // emits: class on the <li>, parent <ul class="rows"> — not just strings.
    assert!(
        css.contains("li.moderation-row, .rows li.moderation-row {")
            && css.contains("grid-template-columns: minmax(0, 1fr) auto auto"),
        "grid rule missing: {css}"
    );
    let resp = app
        .oneshot(authed_req("/blocked-words", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let html = body_string(resp).await;
    assert!(
        html.contains(r#"<ul class="rows"><li class="moderation-row">"#),
        "grid selector `li.moderation-row, .rows li.moderation-row` must match rendered markup: {html}"
    );
}

#[test]
fn recovery_codes_render_as_escaped_compact_chips() {
    let codes = vec!["AB12-CD34".to_owned(), "<b>&\"x".to_owned()];
    let html = super::super::mfa_users::recovery_codes_html(&codes);
    assert_eq!(html.matches("<li><code class=\"copyable\">").count(), 2);
    assert!(html.contains("AB12-CD34"), "{html}");
    assert!(html.contains("&lt;b&gt;&amp;&quot;x"), "{html}");
    assert!(!html.contains("<b>"), "{html}");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn customers_filter_form_uses_spaced_inline_utilities() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/customers", &owner)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains(r#"<div class="filter-bar">"#), "{body}");
    assert!(
        body.contains(r#"<form method="get" action="/customers" class="inline-form">"#),
        "{body}"
    );
    assert!(body.contains(r#"<label for="customer-tag">Tag"#), "{body}");
    assert!(
        body.contains(r#"<label for="customer-search">Search"#),
        "{body}"
    );
    assert!(
        body.contains(r#"<nav aria-label="Filter customers by status" class="inline-form">"#),
        "{body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn unauth_dashboard_redirects_to_login() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let req = Request::builder()
        .uri("/dashboard")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn login_valid_sets_cookie_and_redirects() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(with_csrf(
            "username=owner&password=secret",
            &crate::auth::anonymous_csrf_token(),
        )))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/mfa/enroll");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn login_invalid_returns_error() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/login")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(with_csrf(
            "username=owner&password=wrong",
            &crate::auth::anonymous_csrf_token(),
        )))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("Invalid credentials"));
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn auth_dashboard_returns_200() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("owner").unwrap();
    let token = db.create_web_session(acc_id, "owner", None, 7);
    let app = router(state);
    let req = Request::builder()
        .uri("/dashboard")
        .header("cookie", format!("session={}", token))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn auth_referrals_returns_200() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("owner").unwrap();
    let token = db.create_web_session(acc_id, "owner", None, 7);
    let app = router(state);
    let req = Request::builder()
        .uri("/referrals")
        .header("cookie", format!("session={}", token))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn logout_clears_session() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("owner").unwrap();
    let token = db.create_web_session(acc_id, "owner", None, 7);
    let app = router(state);
    let req = Request::builder()
        .uri("/logout")
        .header("cookie", format!("session={}", token))
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert!(resp.status().is_redirection());
    let set_cookie = resp.headers().get("set-cookie").unwrap().to_str().unwrap();
    assert!(set_cookie.contains("Max-Age=0"));
    assert!(db.get_web_session(&token).is_none());
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn dashboard_shows_connect_banner_when_unconfigured() {
    let (state, _dir) = make_unclaimed_state().await;
    // A claimed-but-never-configured console: owner account exists, the
    // bot token never landed.
    state
        .db
        .create_web_account("root", "secret1", "owner", None)
        .await
        .unwrap();
    let (acc_id, _u, _h, _r, _s) = state.db.get_web_account_by_username("root").unwrap();
    let token = state.db.create_web_session(acc_id, "owner", None, 7);
    let app = router(state);
    let resp = app.oneshot(authed_req("/dashboard", &token)).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("The bot is not connected yet."), "{body}");
    assert!(
        body.contains(r#"href="/settings""#),
        "banner links to Settings: {body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn privacy_page_smoke() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/privacy", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("Privacy"), "{body}");
}

// ---- broadcast scope (P1-1, fail closed) ----

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn broadcast_fails_closed_for_non_owner_without_seller_scope() {
    let (state, _dir) = make_state().await;
    // A user a fail-open (None = global) scope WOULD have matched: the
    // empty result below must come from the scope, never an empty DB.
    state.db.upsert_user(4242, Some("buyer"));
    let db = state.db.clone();
    db.create_web_account("scopeless", "secret", "admin", None)
        .await
        .unwrap();
    let (acc_id, _u, _h, _r, _s) = db.get_web_account_by_username("scopeless").unwrap();
    let token = db.create_web_session(acc_id, "admin", None, 7);
    let app = router(state);
    // The page shows the honest empty state, not the global audience count.
    let resp = app
        .clone()
        .oneshot(authed_req("/broadcast", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("This account has no seller scope — nothing to send."),
        "{body}"
    );
    assert!(!body.contains("will go to <strong>1</strong>"), "{body}");
    // The send delivers to nobody and reports the failure banner.
    let resp = app
        .oneshot(post_authed_req(
            "/broadcast/send",
            "text=hello&segment=all",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/broadcast?sent=0",
        "a scopeless non-owner delivers to zero users"
    );
}

// ---- EDIT_FLOW_SPEC web pass: additive pins for the flow-break fixes ----

/// E2: POST /product/activate flips an inactive row back on and redirects
/// with the `activated=1` flash (the Deactivate dead-end's counterpart).
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn product_activate_reactivates_and_redirects() {
    let (state, _dir) = make_state().await;
    state
        .db
        .add_product(0, "widget", "10", "TON", "digital_link", None, None)
        .unwrap();
    state.db.set_product_active(0, "widget", false);
    assert!(
        state
            .db
            .list_products(Some(0))
            .iter()
            .any(|p| p.2 == "widget" && p.8 == 0),
        "fixture: product starts inactive"
    );
    let token = owner_session(&state).await;
    let app = router(state.clone());
    let resp = app
        .oneshot(post_authed_req("/product/activate", "name=widget", &token))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    let loc = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(loc.contains("activated=1"), "{loc}");
    assert!(
        state
            .db
            .list_products(Some(0))
            .iter()
            .any(|p| p.2 == "widget" && p.8 == 1),
        "the product is active again"
    );
}

/// E2 scope rule (the seller-namespace 403-class pin): a seller session
/// must never flip another seller's product — the row is addressed by name,
/// so the seller namespace is the isolation boundary.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn product_activate_scoped_to_seller_namespace() {
    let (state, _dir) = make_state().await;
    state
        .db
        .add_product(0, "owners", "5", "TON", "digital_link", None, None)
        .unwrap();
    state.db.set_product_active(0, "owners", false);
    let token = seller_session(&state, 42).await;
    let app = router(state.clone());
    let resp = app
        .oneshot(post_authed_req("/product/activate", "name=owners", &token))
        .await
        .unwrap();
    // Same acceptance as subs_revoke_requires_owner: refused outright (403)
    // or a redirect that carries no mutation of the foreign row.
    assert!(
        resp.status() == StatusCode::FORBIDDEN || resp.status().is_redirection(),
        "{resp:?}"
    );
    assert!(
        state
            .db
            .list_products(Some(0))
            .iter()
            .any(|p| p.2 == "owners" && p.8 == 0),
        "the owner's product stays inactive"
    );
}

/// E10: the bulk revoke (kicks up to 1000 buyers) is POST-only — the GET
/// arm that made it linkable/prefetchable is gone.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn subs_bulkrevoke_get_is_gone() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .oneshot(authed_req("/subs/bulkrevoke?status=active", &token))
        .await
        .unwrap();
    assert!(
        resp.status() == StatusCode::NOT_FOUND || resp.status() == StatusCode::METHOD_NOT_ALLOWED,
        "GET must not reach the bulk revoke: {}",
        resp.status()
    );
}

/// E4: reclaim revokes the old code, mints the replacement, and the
/// response body shows the new code with the one-time help line — the
/// replacement is no longer thrown away.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn codes_reclaim_shows_new_code_once() {
    let (state, _dir) = make_state().await;
    let old = state.db.create_code(0, None, Some(30), 1, None, 12);
    let token = owner_session(&state).await;
    let app = router(state.clone());
    let resp = app
        .oneshot(post_authed_req(
            "/codes/reclaim",
            &format!("code={old}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    let codes = state.db.list_codes(None, 200);
    assert!(
        codes.iter().any(|c| c.0 == old && c.6),
        "the old code is revoked"
    );
    let new = codes
        .iter()
        .find(|c| c.0 != old && !c.6)
        .map(|c| c.0.clone())
        .expect("a replacement code was minted");
    assert!(body.contains(&new), "banner must show the new code: {body}");
    assert!(body.contains("shown once"), "{body}");
    assert!(
        !body.contains(&format!("/codes/reclaim%22{}%22", esc(&new))),
        "the new code never rides a URL"
    );
}

/// E11: a duplicate username used to vanish (`let _ =`); now the honest
/// failure renders as a banner.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn webusers_create_duplicate_shows_taken_banner() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state.clone());
    // A bound seller, so the first create is a legitimate account rather than
    // the scopeless-seller refusal this test is not about.
    const FORM: &str = "username=dup&password=secret1&role=seller&seller_id=42";
    let resp = app
        .clone()
        .oneshot(post_authed_req("/webusers/create", FORM, &token))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    let resp = app
        .clone()
        .oneshot(post_authed_req("/webusers/create", FORM, &token))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    let loc = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(loc.contains("error=taken"), "{loc}");
    let page = app.oneshot(authed_req(loc, &token)).await.unwrap();
    let body = body_string(page).await;
    assert!(body.contains("That username is taken."), "{body}");
}

/// A `seller` account with no seller binding is the fail-open state: every
/// tenant-scoped helper read `seller_id = None` as "unscoped / every tenant".
/// The create form offers an empty seller option and defaults the role to
/// `seller`, so the pair is one click away. Creation must refuse it, and the
/// refusal must be a specific message rather than the generic create failure.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn webusers_create_seller_without_binding_is_refused() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state.clone());
    let resp = app
        .oneshot(post_authed_req(
            "/webusers/create",
            "username=plat&password=secret1&role=seller&seller_id=",
            &token,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    let loc = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(loc.contains("error=seller"), "{loc}");
    assert!(
        state.db.get_web_account_by_username("plat").is_none(),
        "a scopeless seller must not be persisted"
    );
}

/// The same empty binding IS legitimate for the owner tier (the platform view
/// has no tenant), so the guard must be role-specific, not "any empty id".
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn webusers_create_owner_without_binding_is_allowed() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state.clone());
    let resp = app
        .oneshot(post_authed_req(
            "/webusers/create",
            "username=second-owner&password=secretasingle1&role=owner&seller_id=",
            &token,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    let loc = resp.headers().get("location").unwrap().to_str().unwrap();
    assert!(loc.contains("created=1"), "{loc}");
    let (_id, _u, _h, _r, seller_id) = state
        .db
        .get_web_account_by_username("second-owner")
        .expect("owner account exists");
    assert_eq!(seller_id, None, "an owner stays platform-level");
}

/// E17: the preview form renders exactly ONE visible custom-segment text
/// input (inside the native details) even when both a preset and a custom
/// value round-trip through the URL — the dual-input ambiguity is gone.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn broadcast_single_custom_segment_input() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .oneshot(authed_req(
            "/broadcast?segment=all&segment=product:1",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert_eq!(
        body.matches(r#"<input type="text" name="segment""#).count(),
        1,
        "one custom segment text input"
    );
    let details_pos = body.find("<details").expect("a details disclosure exists");
    let input_pos = body
        .find(r#"<input type="text" name="segment""#)
        .expect("the custom input exists");
    assert!(input_pos > details_pos, "the custom input lives in details");
}

/// M4: the inline status select must preselect the row's current status so
/// saving tracking (a separate form) cannot silently reset delivered→pending.
/// m1: the two Save buttons in a row carry distinct accessible names.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn orders_status_select_preselects_current_and_save_buttons_are_labeled() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let oid = state.db.create_order(7, 1, 999, "inv-orders-ui").unwrap();
    assert!(state.db.set_order_status(7, &oid, "delivered"));

    let resp = router(state)
        .oneshot(authed_req("/orders", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains(r#"<option value="delivered" selected>delivered</option>"#),
        "current status must be selected: {body}"
    );
    assert!(
        !body.contains("Change status…"),
        "delivered is offered, no placeholder needed: {body}"
    );
    assert!(
        body.contains(&format!(r#"aria-label="Save status for order {oid}""#)),
        "{body}"
    );
    assert!(
        body.contains(&format!(r#"aria-label="Save tracking for order {oid}""#)),
        "{body}"
    );

    // A state we deliberately do not offer (`refunded`) falls back to a
    // neutral, disabled prompt rather than silently submitting `pending`.
    let (state2, _dir2) = make_state().await;
    let owner2 = owner_session(&state2).await;
    let oid2 = state2
        .db
        .create_order(7, 1, 999, "inv-orders-ui-2")
        .unwrap();
    assert!(state2.db.set_order_status(7, &oid2, "refunded"));
    let resp2 = router(state2)
        .oneshot(authed_req("/orders", &owner2))
        .await
        .unwrap();
    let body2 = body_string(resp2).await;
    assert!(
        body2.contains(r#"<option value="" selected disabled>Change status…</option>"#),
        "{body2}"
    );
}

/// m2: `role="alert"` is already an assertive live region; duplicating
/// `aria-live="polite"` on the same element gives screen readers conflicting
/// instructions.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn bookings_alert_is_not_a_redundant_live_region() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let resp = router(state)
        .oneshot(authed_req("/bookings?error=invalid", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains(r#"<div class="alert alert-error" role="alert">Invalid booking id.</div>"#),
        "{body}"
    );
    assert!(!body.contains(r#"role="alert" aria-live"#), "{body}");
}

// ---- Action placement / detail alignment regression contracts ------------

/// The action column stays right-aligned in tables, inline action forms hug
/// the right edge, the shared `.row-actions` cluster keeps its own rules, and
/// charts no longer escape their panel. Flex rows must not let a long label
/// squeeze the action affordances; on mobile the cluster drops the auto
/// margin so it can wrap under the text instead of overflowing.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn action_placement_and_detail_alignment_css_contract() {
    let (state, _dir) = make_state().await;
    let resp = router(state)
        .oneshot(get_req("/assets/app.css"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let css = body_string(resp).await;
    for needle in [
        "td.actions { text-align: right; }",
        "td.actions .inline-form { justify-content: flex-end; }",
        ".row-actions {",
        ".panel canvas { display: block; width: 100%;",
        ".rows li .row-action, .row-actions { flex: 0 0 auto; }",
    ] {
        assert!(css.contains(needle), "missing `{needle}` in app.css");
    }
    let compact: String = css.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        compact.contains("li.row-action,.row-actions{margin-left:0;}"),
        "mobile detail alignment override missing: {css}"
    );
}

/// Every console table with an Actions header must render its row actions in
/// the right-aligned cell (`td class="actions"`), not in a plain `<td>`.
/// Fixtures are minimal: one row per page is enough to exercise the row
/// template; the empty-state row never carries an action cell.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn console_tables_render_actions_in_dedicated_cells() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let (account_id, _, _, _, _) = state
        .db
        .get_web_account_by_username("owner")
        .expect("owner account exists");

    state
        .db
        .create_order(7, 1, account_id, "inv-actions-ui")
        .unwrap();
    state
        .db
        .create_promo_code(
            1,
            "UIACTIONS",
            "percent",
            10.0,
            None,
            None,
            0.0,
            "all",
            None,
            None,
        )
        .unwrap();
    state.db.create_code(1, None, Some(30), 1, None, 12);
    let service = state
        .db
        .create_service(1, "consult", "10", "USDT", 60, None)
        .expect("service created");
    let slot = state.db.create_slot(
        service,
        1,
        &(chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339(),
        1,
    );
    assert!(
        state
            .db
            .create_booking(service, slot, account_id)
            .unwrap()
            .is_some(),
        "booking fixture claimed the slot"
    );
    let _ = state.db.create_api_key(account_id, Some("ui-actions"));
    state
        .db
        .cart_add_item(account_id, 1, "product", 7, 1)
        .unwrap();

    let app = router(state);
    for path in [
        "/orders",
        "/promos",
        "/codes",
        "/bookings",
        "/webusers",
        "/keys",
        "/cart",
    ] {
        let resp = app.clone().oneshot(authed_req(path, &owner)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        let body = body_string(resp).await;
        let rows = body
            .split("<tbody>")
            .nth(1)
            .and_then(|chunk| chunk.split("</tbody>").next())
            .unwrap_or_else(|| panic!("{path} has no table body: {body}"));
        assert!(
            rows.contains(r#"<td class="actions""#),
            "{path} row actions must live in a right-aligned cell: {rows}"
        );
    }
}
