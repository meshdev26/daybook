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
        .body(Body::from("username=owner&password=secret"))
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
        .body(Body::from("username=owner&password=wrong"))
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
