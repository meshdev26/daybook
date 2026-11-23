use super::tests::{
    body_string, get_req, make_state, owner_session, seller_session_named, with_csrf,
};
use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

/// Build a POST with full control over the cookie and the raw body, so tests
/// can omit or forge the token (the shared helpers always add a valid one).
fn raw_post(uri: &str, body: &str, cookie: Option<&str>, bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(t) = cookie {
        b = b.header("cookie", format!("session={t}"));
    }
    if let Some(t) = bearer {
        b = b.header("authorization", format!("Bearer {t}"));
    }
    b.body(Body::from(body.to_string())).unwrap()
}

async fn assert_forbidden(resp: axum::http::Response<Body>) {
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_string(resp).await;
    assert!(body.contains("Forbidden"), "{body}");
    assert!(body.contains("No change was made"), "{body}");
}

#[tokio::test]
async fn csrf_valid_token_allows_and_missing_or_wrong_rejects_without_mutation() {
    let (state, _dir) = make_state().await;
    let seller = seller_session_named(&state, 42, "s42").await;
    let app = router(state.clone());

    // No token: 403, no payout row created.
    let resp = app
        .clone()
        .oneshot(raw_post(
            "/finance/payout/create",
            "seller_id=&amount=10&asset=TON",
            Some(&seller),
            None,
        ))
        .await
        .unwrap();
    assert_forbidden(resp).await;
    assert!(state.db.list_payouts(Some(42)).is_empty());

    // Wrong token: same outcome.
    let resp = app
        .clone()
        .oneshot(raw_post(
            "/finance/payout/create",
            &with_csrf("seller_id=&amount=10&asset=TON", "deadbeef"),
            Some(&seller),
            None,
        ))
        .await
        .unwrap();
    assert_forbidden(resp).await;
    assert!(state.db.list_payouts(Some(42)).is_empty());

    // Valid token: the handler runs and the row is created.
    let token = csrf_token_for_session(&seller);
    let resp = app
        .oneshot(raw_post(
            "/finance/payout/create",
            &with_csrf("seller_id=&amount=10&asset=TON", &token),
            Some(&seller),
            None,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection(), "{:?}", resp.status());
    assert_eq!(state.db.list_payouts(Some(42)).len(), 1);
}

#[tokio::test]
async fn csrf_token_is_per_session_and_cross_session_use_is_rejected() {
    let (state, _dir) = make_state().await;
    let a = seller_session_named(&state, 42, "a-seller").await;
    let b = seller_session_named(&state, 43, "b-seller").await;
    let token_a = csrf_token_for_session(&a);
    let token_b = csrf_token_for_session(&b);
    assert_ne!(
        token_a, token_b,
        "distinct sessions must get distinct tokens"
    );

    let app = router(state.clone());
    // Session B's cookie with session A's token: 403, no mutation for B.
    let resp = app
        .oneshot(raw_post(
            "/finance/payout/create",
            &with_csrf("seller_id=&amount=10&asset=TON", &token_a),
            Some(&b),
            None,
        ))
        .await
        .unwrap();
    assert_forbidden(resp).await;
    assert!(state.db.list_payouts(Some(43)).is_empty());
}

#[tokio::test]
async fn csrf_guards_settings_bots_and_identity_forms() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let token = csrf_token_for_session(&owner);
    let bot_id = state.db.create_bot("secondary", 1, None, "");
    let app = router(state.clone());

    // Settings: a missing token leaves the config untouched.
    let resp = app
        .clone()
        .oneshot(raw_post(
            "/settings/sidebar",
            "section=general&show_ai=on",
            Some(&owner),
            None,
        ))
        .await
        .unwrap();
    assert_forbidden(resp).await;
    assert!(state.db.get_config("sidebar_items").is_none());
    // Valid token writes it.
    let resp = app
        .clone()
        .oneshot(raw_post(
            "/settings/sidebar",
            &with_csrf("section=general&show_ai=on", &token),
            Some(&owner),
            None,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_success() || resp.status().is_redirection());
    assert!(state.db.get_config("sidebar_items").is_some());

    // Bots: a missing token leaves the session's selection untouched.
    let resp = app
        .clone()
        .oneshot(raw_post(
            "/bot/select",
            &format!("bot_id={bot_id}"),
            Some(&owner),
            None,
        ))
        .await
        .unwrap();
    assert_forbidden(resp).await;
    assert_eq!(state.db.get_web_session(&owner).unwrap().3, None);
    // Valid token changes it.
    let resp = app
        .clone()
        .oneshot(raw_post(
            "/bot/select",
            &with_csrf(&format!("bot_id={bot_id}"), &token),
            Some(&owner),
            None,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(state.db.get_web_session(&owner).unwrap().3, Some(bot_id));

    // Identity: a missing token never reaches the unlock throttle.
    let key = format!(
        "identity-unlock:{}",
        state.db.get_web_session(&owner).unwrap().0
    );
    let resp = app
        .clone()
        .oneshot(raw_post(
            "/identity/unlock",
            "passphrase=wrong",
            Some(&owner),
            None,
        ))
        .await
        .unwrap();
    assert_forbidden(resp).await;
    assert!(!state.login_attempts.lock().unwrap().contains_key(&key));
    // Valid token reaches the handler and records the attempt.
    let resp = app
        .oneshot(raw_post(
            "/identity/unlock",
            &with_csrf("passphrase=wrong", &token),
            Some(&owner),
            None,
        ))
        .await
        .unwrap();
    assert!(!resp.status().is_client_error() || resp.status() == StatusCode::BAD_REQUEST);
    assert!(state.login_attempts.lock().unwrap().contains_key(&key));
}

#[tokio::test]
async fn csrf_hidden_field_renders_on_every_post_form_across_modules() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let token = csrf_token_for_session(&owner);
    let app = router(state);

    // Representative pages covering finance, settings, bots, bookings,
    // identity, plus other form-heavy surfaces.
    for uri in [
        "/dashboard",
        "/settings",
        "/bots",
        "/bookings",
        "/identity",
        "/finance",
        "/codes",
        "/cart",
    ] {
        let req = Request::builder()
            .uri(uri)
            .header("cookie", format!("session={owner}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        let body = body_string(resp).await;
        let post_forms = body.matches(r#"method="post""#).count();
        let tokens = body.matches(r#"name="csrf_token""#).count();
        assert!(post_forms > 0, "{uri} rendered no POST form");
        assert_eq!(
            post_forms, tokens,
            "{uri}: a POST form is missing the token\n{body}"
        );
        assert!(
            body.contains(&format!(r#"name="csrf_token" value="{token}""#)),
            "{uri} did not render the session token"
        );
    }
}

#[tokio::test]
async fn csrf_preauth_door_renders_anonymous_token_and_rejects_a_missing_one() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.clone().oneshot(get_req("/login")).await.unwrap();
    let body = body_string(resp).await;
    let anon = anonymous_csrf_token();
    assert!(
        body.contains(&format!(r#"name="csrf_token" value="{anon}""#)),
        "login form must carry the anonymous token"
    );

    // Missing token on the login POST is refused before credential check.
    let resp = app
        .oneshot(raw_post(
            "/login",
            "username=owner&password=secret",
            None,
            None,
        ))
        .await
        .unwrap();
    assert_forbidden(resp).await;
}

#[tokio::test]
async fn csrf_bearer_clients_are_exempt() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let app = router(state);
    // A Bearer-authenticated request (machine client) is not a cookie form;
    // the middleware leaves it to the handler, so no 403 from the CSRF gate.
    let resp = app
        .oneshot(raw_post("/cart/clear", "", None, Some(&owner)))
        .await
        .unwrap();
    assert_ne!(resp.status(), StatusCode::FORBIDDEN);
}
