// First-run setup, skip, and the claim flow (incl. the loopback/HIGH-5
// peer semantics).
use super::*;
use axum::body::Body;
use axum::http::StatusCode;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

/// A loopback or remote peer, injected the way
/// `into_make_service_with_connect_info` would (as the ConnectInfo
/// extension, which the PeerIp extractor reads).
fn with_peer(req: Request<Body>, ip: [u8; 4]) -> Request<Body> {
    let (mut parts, body) = req.into_parts();
    parts
        .extensions
        .insert(ConnectInfo(SocketAddr::new(IpAddr::from(ip), 40000)));
    Request::from_parts(parts, body)
}

/// A state where first-run setup is still OPEN (no setup_done flag, no
/// configured snapshot) so /setup renders and accepts POSTs.
#[tracing::instrument(skip_all)]
async fn make_setup_state() -> (WebState, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Db::new(db_path.to_str().unwrap()).unwrap();
    let cfg = Config {
        owner_id: 0,
        db_path: db_path.to_str().unwrap().to_string(),
        crypto_token: None,
        ai_enabled: false,
        mfa_recovery_codes_enabled: true,
        ai_provider: "openai".to_string(),
        ai_api_style: "openai".to_string(),
        ai_base_url: "https://api.openai.com/v1".to_string(),
        ai_api_key: None,
        ai_model: "gpt-4o-mini".to_string(),
        ai_daily_token_budget: None,
        mcp_owner_token: None,
        telegram_api_id: None,
        telegram_api_hash: None,
        bots: Vec::new(),
        bots_legacy: true,
    };
    let db = Arc::new(db);
    let bots = Arc::new(crate::bots::BotManager::placeholder(
        db.clone(),
        Arc::new(cfg.clone()),
        crate::identity::IdentityHandle::unconnected(),
    ));
    let state = WebState {
        db: db.clone(),
        cfg,
        bot: teloxide::Bot::new("0:dummy"),
        bots,
        identity: crate::identity::IdentityHandle::unconnected(),
        login_attempts: Arc::new(StdMutex::new(HashMap::new())),
        ai_test_inflight: Arc::new(tokio::sync::Mutex::new(())),
        base_path: String::new(),
    };
    (state, dir)
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn skip_sets_setup_done_and_creates_no_account() {
    let (state, _dir) = make_unclaimed_state().await;
    let db = state.db.clone();
    let app = router(state);
    let resp = app.oneshot(post_req("/setup/skip", "")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/login",
        "skip lands on the (claim) front door"
    );
    // The B2 close-on-leave invariant: the gate is closed for good...
    assert_eq!(db.get_config("setup_done").as_deref(), Some("1"));
    // ...but nothing else happened: no account, no configured snapshot.
    assert!(db.get_web_account_by_username("owner").is_none());
    assert!(!db.owner_account_exists());
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn skip_after_done_redirects_to_login() {
    let (state, _dir) = make_unclaimed_state().await;
    state.db.set_config("setup_done", "1");
    let app = router(state);
    let resp = app.oneshot(post_req("/setup/skip", "")).await.unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/login");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn setup_page_shows_steps_and_honest_skip() {
    let (state, _dir) = make_setup_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/setup")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    // F1.3: the wizard-style step line under the h1.
    assert!(
        body.contains("Two steps. This page is step 1 — step 2 is restarting the app."),
        "{body}"
    );
    // F1.3: optional fields live in a collapsed disclosure.
    assert!(body.contains("Optional — add later in Settings"), "{body}");
    assert!(!body.contains("<details open><summary>Optional"), "{body}");
    // F1.5: the skip is honest — confirm dialog + "won't come back" copy.
    assert!(body.contains("This page won't come back."), "{body}");
    assert!(
        body.contains("Finish setup later in Settings → Bot connection?"),
        "{body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn setup_submit_rerenders_in_place_preserving_typed_fields() {
    let (state, _dir) = make_setup_state().await;
    let app = router(state);
    // Two local typos: a non-numeric owner ID and a short password. Local
    // validation must catch BOTH in one focused alert BEFORE any Telegram
    // call, and the typed non-secret values must survive (F1.1/F1.2).
    let resp = app
        .oneshot(post_req(
            "/setup",
            "bot_token=123456:REALTOKEN&owner_id=12abc&default_channel=@myshop&crypto_token=&web_username=chief&web_password=short",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "re-render, not a redirect");
    let body = body_string(resp).await;
    assert!(body.contains("data-focus-on-load"), "{body}");
    assert!(
        body.contains(
            "Owner ID must be the number @userinfobot replies with — for example 123456789."
        ),
        "{body}"
    );
    assert!(
        body.contains("Choose a dashboard password of at least 6 characters."),
        "{body}"
    );
    // Typed non-secrets survive...
    assert!(body.contains(r#"value="12abc""#), "{body}");
    assert!(body.contains(r#"value="@myshop""#), "{body}");
    assert!(body.contains(r#"value="chief""#), "{body}");
    // ...secrets never re-render (never-re-render-a-secret rule).
    assert!(!body.contains(r#"name="bot_token" value="#), "{body}");
    assert!(!body.contains(r#"name="web_password" value="#), "{body}");
    assert!(!body.contains(r#"name="crypto_token" value="#), "{body}");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn setup_submit_reports_all_local_problems_at_once() {
    let (state, _dir) = make_setup_state().await;
    let app = router(state);
    // Empty token + bad owner + short password: all three problems in one
    // alert, and no network dependency in the test (local checks first).
    let resp = app
        .oneshot(post_req(
            "/setup",
            "bot_token=&owner_id=oops&web_password=x",
        ))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Paste the bot token from @BotFather"),
        "{body}"
    );
    assert!(body.contains("Owner ID must be the number"), "{body}");
    assert!(body.contains("Choose a dashboard password"), "{body}");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn claim_page_renders_when_no_owner() {
    let (state, _dir) = make_unclaimed_state().await;
    let app = router(state);
    let resp = app.oneshot(get_req("/login")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("Claim the console"), "{body}");
    assert!(
        body.contains(r#"autocomplete="new-password""#),
        "claim form asks for a NEW password:\n{body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn claim_blocked_off_loopback_without_code() {
    let (state, _dir) = make_unclaimed_state().await;
    let db = state.db.clone();
    // A live code exists on the server; the attacker does not have it.
    db.set_config("claim_code", "code-12345");
    // Remote peer, no code: refused, nothing created, no claim hint leak.
    let req = with_peer(
        post_req(
            "/login",
            "username=admin&password=hunter2&confirm_password=hunter2",
        ),
        [203, 0, 113, 7],
    );
    let resp = router(state.clone()).oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("Claiming requires the one-time code shown in the server console."),
        "{body}"
    );
    assert!(!body.contains("hunter2"), "form input echoed back: {body}");
    assert!(
        !body.contains("code-12345"),
        "the server's claim code must never be echoed: {body}"
    );
    assert!(!db.owner_account_exists());

    // A wrong code from a remote peer: same refusal, still no echo.
    let req = with_peer(
        post_req(
            "/login",
            "username=admin&password=hunter2&confirm_password=hunter2&claim_code=wrong",
        ),
        [203, 0, 113, 7],
    );
    let resp = router(state).oneshot(req).await.unwrap();
    let body = body_string(resp).await;
    assert!(
        body.contains("Claiming requires the one-time code"),
        "{body}"
    );
    assert!(
        !body.contains("code-12345"),
        "real code must not appear when a wrong one is tried: {body}"
    );
    assert!(!db.owner_account_exists());
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn claim_with_code_works_and_creates_owner_session() {
    let (state, _dir) = make_unclaimed_state().await;
    // The claim code is generated at startup and printed to the console;
    // tests seed it the same place production stores it.
    state.db.set_config("claim_code", "code-12345");
    let db = state.db.clone();
    let app = router(state);
    // No peer extension (unknown, i.e. remote) — the code alone must be
    // sufficient.
    let resp = app
        .oneshot(post_req(
            "/login",
            "username=root&password=secret1&confirm_password=secret1&claim_code=code-12345",
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(resp.headers().get("location").unwrap(), "/dashboard");
    let cookie = resp
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.contains("session="), "{cookie}");
    let (acc_id, _u, _h, role, seller_id) =
        db.get_web_account_by_username("root").expect("claimed");
    assert_eq!(role, "owner");
    assert_eq!(seller_id, None);
    // The presented session works and belongs to the new owner account.
    let token = cookie
        .split(';')
        .next()
        .unwrap()
        .trim_start_matches("session=");
    assert_eq!(
        db.get_web_session(token),
        Some((acc_id, "owner".to_string(), None, None))
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn claim_from_loopback_needs_no_code() {
    // The desktop default (WEB_BIND unset) is a loopback bind, so a
    // loopback peer is genuinely local; pin the env so a developer's
    // exported WEB_BIND cannot flip the outcome.
    let _env = ENV_LOCK.lock().await;
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::remove_var("WEB_BIND") };
    let (state, _dir) = make_unclaimed_state().await;
    let db = state.db.clone();
    let app = router(state);
    let req = with_peer(
        post_req(
            "/login",
            "username=root&password=secret1&confirm_password=secret1",
        ),
        [127, 0, 0, 1],
    );
    let resp = app.oneshot(req).await.unwrap();
    assert!(resp.status().is_redirection(), "loopback claims freely");
    assert_eq!(resp.headers().get("location").unwrap(), "/dashboard");
    assert!(db.owner_account_exists());
}

/// HIGH-5 regression: behind a reverse proxy on the same host
/// (WEB_BIND=0.0.0.0) every remote browser arrives with a loopback peer
/// address — the peer alone must not waive the one-time claim code.
#[tokio::test]
#[tracing::instrument(skip_all)]
async fn claim_loopback_behind_wildcard_bind_requires_code() {
    let _env = ENV_LOCK.lock().await;
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::set_var("WEB_BIND", "0.0.0.0") };
    let (state, _dir) = make_unclaimed_state().await;
    let db = state.db.clone();
    let app = router(state);
    let req = with_peer(
        post_req(
            "/login",
            "username=root&password=secret1&confirm_password=secret1",
        ),
        [127, 0, 0, 1],
    );
    let resp = app.oneshot(req).await.unwrap();
    assert!(
        !resp.status().is_redirection(),
        "a loopback peer behind a wildcard bind is NOT the machine itself"
    );
    let body = body_string(resp).await;
    assert!(
        body.contains("Claiming requires the one-time code"),
        "the claim form must demand the code: {body}"
    );
    assert!(!db.owner_account_exists(), "no account may be created");
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::remove_var("WEB_BIND") };
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn claim_mismatched_confirmation_refused() {
    // Loopback-only default bind (HIGH-5): pin the env so the loopback
    // peer keeps waiving the claim code here.
    let _env = ENV_LOCK.lock().await;
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::remove_var("WEB_BIND") };
    let (state, _dir) = make_unclaimed_state().await;
    let db = state.db.clone();
    let app = router(state);
    let req = with_peer(
        post_req(
            "/login",
            "username=root&password=secret1&confirm_password=secret2",
        ),
        [127, 0, 0, 1],
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("Passwords do not match"), "{body}");
    assert!(!db.owner_account_exists());
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn claim_impossible_once_owner_exists() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let app = router(state);
    // GET: the front door is the sign-in form again — claim mode never
    // returns.
    let resp = app.clone().oneshot(get_req("/login")).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("Sign in"), "{body}");
    assert!(!body.contains("Claim the console"), "{body}");
    // POST with claim-shaped fields: treated as a plain login attempt.
    let resp = app
        .oneshot(post_req(
            "/login",
            "username=root&password=secret1&confirm_password=secret1&claim_code=code-12345",
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("Invalid credentials"), "{body}");
    assert!(db.get_web_account_by_username("root").is_none());
    assert!(db.owner_account_exists());
}
