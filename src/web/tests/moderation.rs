// Moderation capability pages (IA amendment §4, foundation plan Step 4.3).
// Web-surface coverage only: gates, tenancy, states, and feature packaging.
// The data-layer precedence resolver is core's unit-test territory.
use super::*;
use axum::http::StatusCode;
use tower::ServiceExt;

async fn buyer_session(state: &WebState) -> String {
    state
        .db
        .create_web_account("buyer", "secret", "user", None)
        .await
        .unwrap();
    let (id, _u, _h, _r, _s) = state.db.get_web_account_by_username("buyer").unwrap();
    state.db.create_web_session(id, "user", None, 7)
}

/// Admin sessions deliberately vary by seller binding: the security LOW finding
/// was that an admin WITHOUT one got a 403 on Blocked words even though Admin
/// outranks Seller. Both variants share one helper; username stays UNIQUE.
async fn admin_session(state: &WebState, username: &str, seller_id: Option<i64>) -> String {
    state
        .db
        .create_web_account(username, "secret", "admin", seller_id)
        .await
        .unwrap();
    let (id, _u, _h, _r, _s) = state.db.get_web_account_by_username(username).unwrap();
    state.db.create_web_session(id, "admin", seller_id, 7)
}

// ---- (a) owner floor ------------------------------------------------------

#[cfg(feature = "moderation")]
#[tokio::test]
async fn content_policy_is_owner_only_and_crud_works() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let app = router(state.clone());

    let resp = app
        .clone()
        .oneshot(authed_req("/content-policy", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("cannot switch these off or loosen them"),
        "{body}"
    );
    assert!(body.contains("No floor terms yet."), "{body}");
    assert!(body.contains(r#"action="/content-policy/add""#), "{body}");

    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/content-policy/add",
            "term=phishing&mode=hide",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("phishing"), "{body}");
    assert!(body.contains("Hide"), "{body}");
    assert!(body.contains(r#"role="status""#), "{body}");
    let floor = state.db.list_moderation_global_floor();
    assert_eq!(floor.len(), 1);
    assert_eq!(floor[0].term, "phishing");
    assert_eq!(floor[0].mode, "hide");

    // Preserves input and offers Retry when validation fails.
    let long_term = "x".repeat(201);
    let form = format!("term={long_term}&mode=warn");
    let resp = app
        .clone()
        .oneshot(post_authed_req("/content-policy/add", &form, &owner))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("too long"), "{body}");
    assert!(body.contains(&long_term), "{body}");
    assert!(body.contains(">Retry</button>"), "{body}");
    assert_eq!(state.db.list_moderation_global_floor().len(), 1);

    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/content-policy/remove",
            "term=phishing",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(state.db.list_moderation_global_floor().is_empty());

    // A forged seller id is a 403 even on the owner-only page.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/content-policy/add",
            "term=x&mode=warn&seller_id=42",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(state.db.list_moderation_global_floor().is_empty());

    // Seller and buyer never reach the floor page or its writes.
    let seller = seller_session(&state, 42).await;
    for uri in ["/content-policy", "/content-policy?user_id=1"] {
        let resp = app.clone().oneshot(authed_req(uri, &seller)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/content-policy/add",
            "term=x&mode=warn",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let buyer = buyer_session(&state).await;
    let resp = app
        .oneshot(authed_req("/content-policy", &buyer))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

// ---- (b) seller blocked words --------------------------------------------

#[cfg(feature = "moderation")]
#[tokio::test]
async fn blocked_words_roles_and_forged_seller_ids() {
    let (state, _dir) = make_state().await;
    let app = router(state.clone());
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 42).await;
    let buyer = buyer_session(&state).await;

    for token in [&owner, &seller] {
        let resp = app
            .clone()
            .oneshot(authed_req("/blocked-words", token))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains(">Add a word</button>"), "{body}");
        assert!(body.contains(">Add an allowed word</button>"), "{body}");
        assert!(body.contains("No blocked words yet."), "{body}");
        assert!(body.contains("Request review"), "{body}");
    }
    let resp = app
        .clone()
        .oneshot(authed_req("/blocked-words", &buyer))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Forged seller id on both the read and write paths.
    let resp = app
        .clone()
        .oneshot(authed_req("/blocked-words?seller_id=43", &seller))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = app
        .oneshot(post_authed_req(
            "/blocked-words/add",
            "term=x&mode=warn&seller_id=43",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(state.db.list_blocked_words(43).is_empty());
}

/// security LOW functional finding: an admin provisioned without a seller id used
/// to get 403 on Blocked words. Admin outranks Seller, so the role gate now
/// passes; with no seller binding the data scope is the owner's global 0.
#[cfg(feature = "moderation")]
#[tokio::test]
async fn blocked_words_admin_without_seller_uses_global_scope() {
    let (state, _dir) = make_state().await;
    let app = router(state.clone());
    let admin = admin_session(&state, "admin-global", None).await;

    let resp = app
        .clone()
        .oneshot(authed_req("/blocked-words", &admin))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains(">Add a word</button>"), "{body}");

    // A write from the scopeless admin lands in namespace 0, the same floor
    // the owner uses — not in an arbitrary seller's list.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/blocked-words/add",
            "term=global&mode=hide",
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        state.db.get_blocked_word(0, "global").map(|w| w.mode),
        Some("hide".to_string())
    );
    assert!(state.db.get_blocked_word(42, "global").is_none());

    // Forged seller_id stays a 403 for admin on both read and write paths.
    let resp = app
        .clone()
        .oneshot(authed_req("/blocked-words?seller_id=42", &admin))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/blocked-words/add",
            "term=x&mode=warn&seller_id=42",
            &admin,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(state.db.get_blocked_word(42, "x").is_none());

    // A seller without a seller binding still fails closed: no widening.
    state
        .db
        .create_web_account("scopeless-seller", "secret", "seller", None)
        .await
        .unwrap();
    let (sid, _u, _h, _r, _s) = state
        .db
        .get_web_account_by_username("scopeless-seller")
        .unwrap();
    let scopeless = state.db.create_web_session(sid, "seller", None, 7);
    let resp = app
        .oneshot(authed_req("/blocked-words", &scopeless))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// Content policy stays owner-only after the ordered-role change: an admin,
/// with or without a seller binding, cannot reach the global floor.
#[cfg(feature = "moderation")]
#[tokio::test]
async fn content_policy_remains_owner_only_for_admin() {
    let (state, _dir) = make_state().await;
    let app = router(state.clone());
    for (name, seller) in [("admin-global", None), ("admin-scoped", Some(42))] {
        let admin = admin_session(&state, name, seller).await;
        let resp = app
            .clone()
            .oneshot(authed_req("/content-policy", &admin))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{name}");
        let resp = app
            .clone()
            .oneshot(post_authed_req(
                "/content-policy/add",
                "term=x&mode=warn",
                &admin,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{name}");
    }
    assert!(state.db.list_moderation_global_floor().is_empty());
}

#[cfg(feature = "moderation")]
#[tokio::test]
async fn blocked_words_scope_is_cross_tenant_isolated() {
    let (state, _dir) = make_state().await;
    let app = router(state.clone());
    // Two distinct accounts in one state: web_accounts.username is UNIQUE,
    // so each tenant needs its own name (the generic seller_session helper
    // always writes "seller" and can only be called once per state).
    let a = seller_session_named(&state, 42, "seller42").await;
    let b = seller_session_named(&state, 43, "seller43").await;

    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/blocked-words/add",
            "term=spam&mode=hide",
            &a,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        state
            .db
            .get_blocked_word(42, "spam")
            .map(|w| (w.mode, w.enabled)),
        Some(("hide".to_string(), true))
    );
    assert!(state.db.get_blocked_word(43, "spam").is_none());

    // The other tenant cannot see, re-mode or remove this row.
    let resp = app
        .clone()
        .oneshot(authed_req("/blocked-words", &b))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(!body.contains(">spam<"), "{body}");
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/blocked-words/mode",
            "term=spam&mode=warn",
            &b,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(post_authed_req("/blocked-words/remove", "term=spam", &b))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        state.db.get_blocked_word(42, "spam").map(|w| w.mode),
        Some("hide".to_string())
    );

    // Allow-list rows are seller-scoped too.
    let resp = app
        .clone()
        .oneshot(post_authed_req("/blocked-words/allow", "term=sale", &a))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(state.db.get_allow_word(42, "sale").is_some());
    assert!(state.db.get_allow_word(43, "sale").is_none());
    let resp = app
        .oneshot(post_authed_req(
            "/blocked-words/allow/remove",
            "term=sale",
            &b,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(state.db.get_allow_word(42, "sale").is_some());
}

#[cfg(feature = "moderation")]
#[tokio::test]
async fn blocked_words_states_validation_and_appeal_queue() {
    let (state, _dir) = make_state().await;
    let app = router(state.clone());
    let seller = seller_session(&state, 42).await;

    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/blocked-words/add",
            "term=&mode=warn",
            &seller,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Enter a word before adding it."), "{body}");
    assert!(body.contains(r#"role="alert""#), "{body}");

    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/blocked-words/add",
            "term=spam&mode=hide",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/blocked-words/add",
            "term=spam&mode=warn",
            &seller,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("already in your blocked list"), "{body}");
    assert!(body.contains(r#"value="spam""#), "{body}");
    assert!(body.contains(">Retry</button>"), "{body}");
    assert_eq!(
        state.db.get_blocked_word(42, "spam").map(|w| w.mode),
        Some("hide".to_string())
    );

    let resp = app
        .clone()
        .oneshot(authed_req("/blocked-words", &seller))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("How to handle spam"), "{body}");
    assert!(
        body.contains(r#"aria-describedby="blocked-mode-help""#),
        "{body}"
    );
    for mode in ["hide", "warn", "review"] {
        assert!(body.contains(&format!("value=\"{mode}\"")), "{body}");
    }
    assert!(body.contains(r#"role="status""#), "{body}");

    // Appeal queue: opaque ref only, status as dot + word, decision writes.
    let appeal_id = state
        .db
        .create_moderation_appeal(42, None, None, "opaque-ref-abc", Some("reason"))
        .unwrap();
    let resp = app
        .clone()
        .oneshot(authed_req("/blocked-words", &seller))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Request review"), "{body}");
    assert!(body.contains("Pending"), "{body}");
    assert!(body.contains(r#"class="badge badge-warning""#), "{body}");
    assert!(!body.contains("opaque-ref-abc"), "{body}");

    let form = format!("appeal_id={appeal_id}&status=approved");
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/blocked-words/appeal/status",
            &form,
            &seller,
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Approved"), "{body}");
    assert_eq!(
        state
            .db
            .get_moderation_appeal(42, appeal_id)
            .map(|a| a.status),
        Some("approved".to_string())
    );
    // Cross-tenant decision cannot touch another seller's appeal.
    let other = state
        .db
        .create_moderation_appeal(43, None, None, "other-ref", None)
        .unwrap();
    let form = format!("appeal_id={other}&status=upheld");
    let resp = app
        .oneshot(post_authed_req(
            "/blocked-words/appeal/status",
            &form,
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        state.db.get_moderation_appeal(43, other).map(|a| a.status),
        Some("pending".to_string())
    );
}

// ---- (c) per-bot override -------------------------------------------------

/// security residual gap: a content-bearing console broadcast must be decided by
/// the central outbound gate BEFORE it can hit a Telegram transport. The mock
/// API server records zero requests on a hold; a clean broadcast still sends.
#[cfg(feature = "moderation")]
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn broadcast_is_gated_before_any_transport_hit() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let (mut state, _dir) = make_state().await;
    // A real audience: without the gate, the send loop WOULD hit the mock.
    state.db.upsert_user(4242, Some("buyer"));
    let owner = owner_session(&state).await;
    assert!(
        state
            .db
            .add_moderation_global_floor_term("bannedword", "hide")
    );

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 1, "date": 0, "chat": { "id": 42, "type": "private" } }
        })))
        .mount(&server)
        .await;
    let bot = teloxide::Bot::new("9:placeholder")
        .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());
    let ctx = Arc::new(crate::bots::BotContext {
        id: 9,
        key: "broadcast".to_string(),
        bot,
        owner_id: 0,
        shared: Arc::new(state.cfg.clone()),
        db: state.db.clone(),
    });
    let mut manager = crate::bots::BotManager::placeholder(
        state.db.clone(),
        Arc::new(state.cfg.clone()),
        state.identity.clone(),
    );
    manager.bots = vec![ctx.clone()];
    manager.primary = ctx;
    state.bots = Arc::new(manager);

    let app = router(state.clone());
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/broadcast/send",
            "text=please%20read%20bannedword&segment=all",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/broadcast?held=1",
        "the gate must refuse before the send loop"
    );
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "a held broadcast must never reach the transport"
    );
    let decisions = state.db.list_moderation_decisions(0, 10);
    assert!(
        decisions.iter().any(|d| d.action == "outbound_hide"),
        "{decisions:?}"
    );

    // Feedback is a live status region and never echoes the held text.
    let resp = app
        .oneshot(authed_req("/broadcast?held=1", &owner))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("held by the content policy"), "{body}");
    assert!(body.contains(r#"role="status""#), "{body}");
    assert!(!body.contains("bannedword"), "{body}");
}

#[cfg(feature = "moderation")]
#[tokio::test]
async fn per_bot_override_defaults_to_inheritance_and_is_scoped() {
    let (state, _dir) = make_state().await;
    let app = router(state.clone());
    let owner = owner_session(&state).await;
    let bot_id = state.db.create_bot("modbot", 1, None, "Moderation bot");

    let resp = app
        .clone()
        .oneshot(authed_req("/bots", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains(r#"<details class="advanced""#), "{body}");
    assert!(body.contains("Using your shop's rules."), "{body}");
    assert!(body.contains(">Customize</button>"), "{body}");
    assert!(!body.contains("Add word for this bot"), "{body}");

    // The Customize action flips only this bot's sentinel in the owner
    // namespace (seller 0); another seller's namespace stays untouched.
    let inherit_url = format!("/bots/{bot_id}/moderation/inherit");
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            &inherit_url,
            &format!("bot_id={bot_id}&inherits=false"),
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(!state.db.bot_override_inherits(0, bot_id));
    assert!(state.db.bot_override_inherits(42, bot_id));

    let add_url = format!("/bots/{bot_id}/moderation/add");
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            &add_url,
            "term=botspam&mode=review",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let terms = state.db.list_bot_override_terms(0, bot_id);
    assert_eq!(terms.len(), 1);
    assert_eq!(terms[0].term, "botspam");
    assert_eq!(terms[0].mode, "review");
    assert!(state.db.list_bot_override_terms(42, bot_id).is_empty());

    let resp = app
        .clone()
        .oneshot(authed_req("/bots", &owner))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Custom rules for this bot"), "{body}");
    assert!(body.contains("botspam"), "{body}");
    assert!(body.contains("Add word for this bot"), "{body}");
    assert!(body.contains("never looser"), "{body}");

    let remove_url = format!("/bots/{bot_id}/moderation/remove");
    let resp = app
        .clone()
        .oneshot(post_authed_req(&remove_url, "term=botspam", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(state.db.list_bot_override_terms(0, bot_id).is_empty());

    let resp = app
        .clone()
        .oneshot(post_authed_req(
            &inherit_url,
            &format!("bot_id={bot_id}&inherits=true"),
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert!(state.db.bot_override_inherits(0, bot_id));

    // Forged seller id and non-owner writes are rejected before any mutation.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            &inherit_url,
            &format!("bot_id={bot_id}&inherits=false&seller_id=42"),
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let seller = seller_session(&state, 42).await;
    let resp = app
        .oneshot(post_authed_req(
            &inherit_url,
            &format!("bot_id={bot_id}&inherits=false"),
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(state.db.bot_override_inherits(0, bot_id));
}

/// M2: a web post edit replaces message text, so it is a content-bearing
/// outbound send. A matching floor term must hold it BEFORE the edit reaches
/// Telegram — the mock server records zero requests on a hold.
#[cfg(feature = "moderation")]
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn post_edit_is_gated_before_any_transport_hit() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let (mut state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    assert!(
        state
            .db
            .add_moderation_global_floor_term("bannedword", "hide")
    );
    // `moderation_decisions.bot_id` is an FK to bots(id): the mock context
    // must use a bot row that actually exists or the audit insert is dropped.
    let bot_id = state.db.create_bot("postedit", 0, None, "Mock bot");

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": { "message_id": 1, "date": 0, "chat": { "id": 42, "type": "private" } }
        })))
        .mount(&server)
        .await;
    let bot = teloxide::Bot::new("9:placeholder")
        .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());
    let ctx = Arc::new(crate::bots::BotContext {
        id: bot_id,
        key: "postedit".to_string(),
        bot,
        owner_id: 0,
        shared: Arc::new(state.cfg.clone()),
        db: state.db.clone(),
    });
    let mut manager = crate::bots::BotManager::placeholder(
        state.db.clone(),
        Arc::new(state.cfg.clone()),
        state.identity.clone(),
    );
    manager.bots = vec![ctx.clone()];
    manager.primary = ctx;
    state.bots = Arc::new(manager);

    let app = router(state.clone());
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/post/edit",
            "chat_id=-100123&message_id=42&text=please%20read%20bannedword",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/posts?held=1",
        "the gate must refuse before the edit transport"
    );
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "a held post edit must never reach the transport"
    );
    assert!(
        state
            .db
            .list_moderation_decisions(0, 10)
            .iter()
            .any(|d| d.action == "outbound_hide"),
        "the hold must be audited"
    );

    // Feedback is a live status region and never echoes the held text.
    let resp = app
        .oneshot(authed_req("/posts?held=1", &owner))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("held by the content policy"), "{body}");
    assert!(body.contains(r#"role="status""#), "{body}");
    assert!(!body.contains("bannedword"), "{body}");
}

/// M5: the warning is about POLICY STATE only. It must surface both signals,
/// stay content-free, and be silent when enforcement is healthy with a floor set.
#[cfg(feature = "moderation")]
#[test]
fn moderation_notice_is_content_free_and_covers_both_signals() {
    use super::super::moderation::moderation_notice_html;

    let healthy = moderation_notice_html(false, false);
    assert!(healthy.is_empty(), "{healthy}");

    let empty_floor = moderation_notice_html(true, false);
    assert!(empty_floor.contains("Content checks are limited right now."));
    assert!(empty_floor.contains("No platform minimum words are set yet"));
    assert!(!empty_floor.contains("temporarily unavailable"));

    let degraded = moderation_notice_html(false, true);
    assert!(degraded.contains("Content checks are limited right now."));
    assert!(degraded.contains("temporarily unavailable"));
    assert!(!degraded.contains("No platform minimum words"));

    let both = moderation_notice_html(true, true);
    assert!(both.contains("No platform minimum words are set yet"));
    assert!(both.contains("temporarily unavailable"));

    // A screen reader hears a polite status; no transcript/term/presence
    // language ever reaches the surface.
    for notice in [&empty_floor, &degraded, &both] {
        assert!(notice.contains(r#"role="status""#), "{notice}");
        assert!(notice.contains("aria-live=\"polite\""), "{notice}");
        assert!(!notice.contains("bannedword"), "{notice}");
        assert!(!notice.contains("message from"), "{notice}");
    }
}

/// M5: a freshly installed floor is empty, so the Content policy and Blocked
/// words pages must render the plain-language warning to the operator.
#[cfg(feature = "moderation")]
#[tokio::test]
async fn empty_floor_warning_is_rendered_on_both_moderation_pages() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 51).await;
    let app = router(state);

    for (path, token) in [("/content-policy", &owner), ("/blocked-words", &seller)] {
        let resp = app.clone().oneshot(authed_req(path, token)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{path}");
        let body = body_string(resp).await;
        assert!(
            body.contains("Content checks are limited right now."),
            "{path}: {body}"
        );
        assert!(
            body.contains("No platform minimum words are set yet"),
            "{path}: {body}"
        );
    }
}

/// Alignment fixes: add forms share the `.form-row` label-above-field row,
/// word rows share the `.moderation-row` grid (actions in column 3), and the
/// request-review empty state is two lines instead of a run-on sentence.
#[cfg(feature = "moderation")]
#[tokio::test]
async fn blocked_words_uses_shared_alignment_utilities() {
    let (state, _dir) = make_state().await;
    let owner = owner_session(&state).await;
    state.db.add_blocked_word(0, "spamword", "warn", true);
    let app = router(state);
    let resp = app
        .oneshot(authed_req("/blocked-words", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains(r#"class="form-row mod-form""#), "{body}");
    assert!(body.contains(r#"class="moderation-row""#), "{body}");
    assert!(body.contains(r#"class="row-actions""#), "{body}");
    assert!(body.contains("No review requests yet."), "{body}");
    assert!(
        body.contains(r#"<span class="empty-detail">Requests from your bots"#),
        "{body}"
    );
    assert!(!body.contains(r#"class="mod-row""#), "{body}");
    // The mode select keeps its accessible name and Update stay in one form.
    assert!(body.contains("How to handle spamword"), "{body}");
    assert!(body.contains("blocked-mode-help"), "{body}");
}
