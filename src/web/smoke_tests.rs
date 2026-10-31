use super::tests::{authed_req, make_state, owner_session, post_authed_req, seller_session};
use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tempfile::TempDir;
use tower::ServiceExt;

fn get_req(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn post_req(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn smoke_finance_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/finance", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_finance_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/finance")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_orders_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/orders", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_orders_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/orders")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_bookings_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/bookings", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_bookings_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/bookings")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_codes_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/codes", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_codes_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/codes")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_crypto_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/crypto", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_crypto_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/crypto")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_admins_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/admins", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_admins_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/admins")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_webusers_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/webusers", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_webusers_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/webusers")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_posts_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/posts", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_posts_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/posts")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_channels_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/channels", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_channels_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/channels")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_settings_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/settings", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_settings_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/settings")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_ai_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/ai", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_ai_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/ai")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_sellers_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/sellers", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_sellers_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/sellers")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_codes_reclaim_authed() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .oneshot(post_authed_req("/codes/reclaim", "code=x", &token))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
}

#[tokio::test]
async fn smoke_codes_reclaim_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app
        .oneshot(post_req("/codes/reclaim", "code=x"))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_channels_add_authed() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .oneshot(post_authed_req("/channels/add", "channel=@x", &token))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
}

#[tokio::test]
async fn smoke_channels_add_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app
        .oneshot(post_req("/channels/add", "channel=@x"))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_post_schedule_authed() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .oneshot(post_authed_req(
            "/post/schedule",
            "chat_id=@c&content=hi&scheduled_at=now",
            &token,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
}

#[tokio::test]
async fn smoke_post_schedule_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app
        .oneshot(post_req(
            "/post/schedule",
            "chat_id=@c&content=hi&scheduled_at=now",
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

// ---- H1: console mutations are tenant-scoped ------------------------

/// Two tenants: seller 42 owns `@mine`, seller 43 owns `@other` — recorded
/// in both places ownership lives (`channels` row and the legacy
/// `sellers.default_channel` column `/setchannel` writes).
async fn two_tenant_state() -> (WebState, TempDir, String, String) {
    let (state, dir) = make_state().await;
    state.db.upsert_seller(42, Some("mine"), Some("@mine"));
    state.db.upsert_seller(43, Some("other"), Some("@other"));
    state.db.add_channel("@mine", 42).unwrap();
    state.db.add_channel("@other", 43).unwrap();
    // A global row (`seller_id` 0) — the shared namespace the composer
    // dropdown offers every seller.
    state.db.add_channel("@global", 0).unwrap();
    let s42 = seller_session(&state, 42).await;
    // `seller_session` always writes the account named "seller", so the
    // second tenant needs its own account explicitly.
    state
        .db
        .create_web_account("seller43", "secret", "seller", Some(43))
        .await
        .unwrap();
    let (acc43, ..) = state
        .db
        .get_web_account_by_username("seller43")
        .expect("seller 43 account exists");
    let s43 = state.db.create_web_session(acc43, "seller", Some(43), 7);
    (state, dir, s42, s43)
}

/// A seller cannot edit, delete, pin or unpin another tenant's post, and
/// the victim's row is untouched afterwards. `message_id=0` keeps the
/// Bot API out of the test: the refusal happens before any Telegram
/// traffic, which is exactly what is asserted.
#[tokio::test]
async fn seller_cannot_mutate_another_tenants_posts() {
    let (state, _dir, s42, _s43) = two_tenant_state().await;
    let db = state.db.clone();
    db.log_post(43, "@other", 1, "text", Some("orig"), "bot")
        .unwrap();
    let rows = db.list_posts(Some(43), 10);
    let victim_pid = rows[0].0;
    let app = router(state);
    let cases = [
        (
            "/post/edit",
            format!("chat_id=@other&message_id=0&text=pwned&post_id={victim_pid}"),
        ),
        (
            "/post/delete",
            format!("chat_id=@other&message_id=0&post_id={victim_pid}"),
        ),
        (
            "/post/pin",
            format!("chat_id=@other&message_id=0&post_id={victim_pid}"),
        ),
        (
            "/post/unpin",
            format!("chat_id=@other&message_id=0&post_id={victim_pid}"),
        ),
    ];
    for (uri, body) in cases {
        let resp = app
            .clone()
            .oneshot(post_authed_req(uri, &body, &s42))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "{uri} must refuse another tenant's channel"
        );
    }
    // The row is untouched: still seller 43's, still the original text.
    assert_eq!(db.get_post_seller(victim_pid), Some(43));
    let rows = db.list_posts(Some(43), 10);
    assert_eq!(rows.len(), 1, "victim post must not be deleted");
    assert_eq!(rows[0].5.as_deref(), Some("orig"));
}

/// The same seller acting on their own channel is not blocked: the
/// mutations apply and the redirect is unchanged.
#[tokio::test]
async fn seller_mutations_on_own_channel_apply() {
    let (state, _dir, s42, _s43) = two_tenant_state().await;
    let db = state.db.clone();
    db.log_post(42, "@mine", 1, "text", Some("orig"), "bot")
        .unwrap();
    let pid = db.list_posts(Some(42), 10)[0].0;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/post/edit",
            &format!("chat_id=@mine&message_id=0&text=edited&post_id={pid}"),
            &s42,
        ))
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "/post/edit must allow the seller's own channel"
    );
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/posts",
        "the happy-path target is unchanged"
    );
    let rows = db.list_posts(Some(42), 10);
    assert_eq!(rows[0].5.as_deref(), Some("edited"));
    // Pin/unpin carry a post_id too: the seller's own row passes.
    for uri in ["/post/pin", "/post/unpin"] {
        let resp = app
            .clone()
            .oneshot(post_authed_req(
                uri,
                &format!("chat_id=@mine&message_id=0&post_id={pid}"),
                &s42,
            ))
            .await
            .unwrap();
        assert!(
            resp.status().is_redirection(),
            "{uri} must allow the seller's own channel"
        );
    }
    let resp = app
        .oneshot(post_authed_req(
            "/post/delete",
            &format!("chat_id=@mine&message_id=0&post_id={pid}"),
            &s42,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(
        db.get_post_seller(pid),
        None,
        "the seller's own post is deletable"
    );
}

/// A seller may schedule only into a channel they own: another tenant's
/// channel is refused with nothing written, their own redirects as before.
#[tokio::test]
async fn seller_can_only_schedule_to_owned_channels() {
    let (state, _dir, s42, _s43) = two_tenant_state().await;
    let db = state.db.clone();
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/post/schedule",
            "chat_id=@other&content=hi&scheduled_at=now",
            &s42,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        db.list_scheduled_posts(Some(42)).is_empty(),
        "a refused schedule writes no row"
    );
    let resp = app
        .oneshot(post_authed_req(
            "/post/schedule",
            "chat_id=@mine&content=hi&scheduled_at=now",
            &s42,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/posts");
    let rows = db.list_scheduled_posts(Some(42));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].2, "@mine");
}

/// Channel retitle/redescribe is tenant-scoped too: the seller's cross
/// attempt is a 403 (no Bot API call is made — the target is refused
/// first), and the owner keeps the whole console.
#[tokio::test]
async fn channel_title_and_desc_are_tenant_scoped() {
    let (state, _dir, s42, _s43) = two_tenant_state().await;
    let app = router(state.clone());
    for (uri, body) in [
        ("/channels/title", "chat_id=@other&title=pwned"),
        ("/channels/desc", "chat_id=@other&description=pwned"),
    ] {
        let resp = app
            .clone()
            .oneshot(post_authed_req(uri, body, &s42))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "{uri} must refuse another tenant's channel"
        );
    }
    // Owner and admin pass the gate on the same target (checked directly:
    // an allowed call would reach the Bot API, which these tests keep
    // offline).
    let owner = Session {
        account_id: 1,
        role: "owner".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    let admin = Session {
        account_id: 2,
        role: "admin".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    assert!(may_mutate_channel(&state, &owner, "@other"));
    assert!(may_mutate_channel(&state, &admin, "@other"));
    assert!(may_mutate_post(&state, &owner, Some(999_999)));
    // A seller session: case-insensitive and `@`-tolerant on their own
    // channel, fail-closed on anything else (unknown names included).
    let seller42 = Session {
        account_id: 3,
        role: "seller".to_string(),
        seller_id: Some(42),
        selected_bot_id: None,
    };
    assert!(may_mutate_channel(&state, &seller42, "@MINE"));
    assert!(may_mutate_channel(&state, &seller42, "mine"));
    assert!(may_mutate_channel(&state, &seller42, "  @Mine "));
    assert!(!may_mutate_channel(&state, &seller42, "@other"));
    assert!(!may_mutate_channel(&state, &seller42, "@ghost"));
    assert!(!may_mutate_channel(&state, &seller42, ""));
    // A session with no seller profile owns nothing.
    let profileless = Session {
        account_id: 4,
        role: "seller".to_string(),
        seller_id: None,
        selected_bot_id: None,
    };
    assert!(!may_mutate_channel(&state, &profileless, "@mine"));
    assert!(!may_mutate_post(&state, &seller42, Some(999_999)));
    assert!(!may_mutate_post(&state, &seller42, Some(0)));
    // The composer gate adds the global namespace the dropdown offers and
    // stops at another seller's channel (review H1).
    assert!(may_post_to_channel(&state, &seller42, "@mine"));
    assert!(may_post_to_channel(&state, &seller42, "@global"));
    assert!(!may_post_to_channel(&state, &seller42, "@other"));
    assert!(!may_post_to_channel(&state, &seller42, "@ghost"));
}

/// Owner cross-tenant mutations stay allowed end to end: both routes
/// answer with their usual redirect (message_id=0 and no post_id keep the
/// test free of Bot API traffic).
#[tokio::test]
async fn owner_cross_tenant_mutations_are_allowed() {
    let (state, _dir, _s42, _s43) = two_tenant_state().await;
    let db = state.db.clone();
    let owner = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/post/schedule",
            "chat_id=@other&content=hi&scheduled_at=now",
            &owner,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    let rows = db.list_scheduled_posts(Some(0));
    assert_eq!(rows.len(), 1, "owner schedules stay in the global scope");
    assert_eq!(rows[0].2, "@other");
    let resp = app
        .oneshot(post_authed_req(
            "/post/edit",
            "chat_id=@other&message_id=0&text=owner-edit",
            &owner,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/posts");
}

/// I1: a double-slash Referer must never yield a protocol-relative PRG
/// target — the path is collapsed to a single leading slash.
#[tokio::test]
async fn referer_path_never_returns_a_protocol_relative_target() {
    let (state, _dir) = make_state().await;
    let hdr = |v: &'static str| {
        let mut h = axum::http::HeaderMap::new();
        h.insert(
            axum::http::header::REFERER,
            axum::http::HeaderValue::from_static(v),
        );
        h
    };
    let got = referer_path(&state, &hdr("http:////evil.example/x")).unwrap();
    assert_eq!(got, "/evil.example/x", "leading slashes are collapsed");
    assert!(!got.starts_with("//"), "never protocol-relative: {got}");
    assert_eq!(
        referer_path(&state, &hdr("http:////evil.example//x")).unwrap(),
        "/evil.example//x",
        "only the leading run is collapsed"
    );
    // The ordinary same-origin cases are unchanged.
    assert_eq!(
        referer_path(&state, &hdr("https://console.local/dashboard?x=1")).unwrap(),
        "/dashboard?x=1"
    );
    assert!(referer_path(&state, &hdr("https://console.local")).is_none());
    assert!(referer_path(&state, &hdr("/dashboard")).is_none());
    // Backslash-authority confusion is rejected outright.
    assert!(referer_path(&state, &hdr("https://host/\\evil.example/x")).is_none());
}

#[tokio::test]
async fn smoke_mfa_enroll_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/mfa/enroll")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_mfa_verify_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/mfa/verify")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_referrals_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/referrals", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_referrals_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/referrals")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_customers_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/customers", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_customers_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/customers")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_tickets_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/tickets", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_tickets_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/tickets")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_reviews_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/reviews", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_reviews_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/reviews")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_analytics_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/analytics", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_analytics_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/analytics")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_privacy_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/privacy", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_privacy_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/privacy")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_promos_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/promos", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_cart_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/cart", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_cart_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/cart")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
async fn smoke_carts_authed_200() {
    let (state, _dir) = make_state().await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/carts", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn smoke_carts_unauth_redirect() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/carts")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}
