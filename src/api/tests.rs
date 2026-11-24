use super::*;
use crate::{config::Config, db::Db};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use proptest::prelude::*;
use std::sync::Arc;
use tower::ServiceExt;

/// Arbitrary JSON generator (depth-bounded) for parser property tests.
fn arb_json(depth: u32) -> BoxedStrategy<serde_json::Value> {
    use serde_json::Value;
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        any::<f64>()
            .prop_filter("finite", |v| v.is_finite())
            .prop_map(Value::from),
        ".*".prop_map(Value::String),
    ];
    if depth == 0 {
        return leaf.boxed();
    }
    prop_oneof![
        leaf,
        proptest::collection::vec(arb_json(depth - 1), 0..4).prop_map(Value::Array),
        proptest::collection::hash_map(".*", arb_json(depth - 1), 0..4)
            .prop_map(|m| Value::Object(m.into_iter().collect())),
    ]
    .boxed()
}

proptest! {
    /// The invoice parser must never panic on arbitrary JSON.
    #[test]
    fn prop_extract_invoice_items_never_panics(v in arb_json(3)) {
        let _ = extract_invoice_items(&v);
    }

    /// All three known Crypto Pay response shapes decode to the same items.
    #[test]
    fn prop_extract_invoice_items_shapes(
        items in proptest::collection::vec(arb_json(1), 0..5)
    ) {
        let arr = serde_json::Value::Array(items.clone());
        for wrapped in [
            arr.clone(),
            serde_json::json!({ "items": arr.clone() }),
            serde_json::json!({ "result": { "items": arr } }),
        ] {
            prop_assert_eq!(extract_invoice_items(&wrapped), items.clone());
        }
    }

    /// Signature checking: a correct HMAC hex digest passes ct_eq and any
    /// single-character mutation fails — for every key/body pair.
    #[test]
    fn prop_signature_check(
        key in proptest::collection::vec(any::<u8>(), 0..64),
        body in proptest::collection::vec(any::<u8>(), 0..256),
        idx in any::<usize>(),
    ) {
        let mac = crate::util::hmac_sha256(&key, &body);
        let hex: String = mac.iter().map(|b| format!("{:02x}", b)).collect();
        prop_assert!(crate::util::ct_eq(&hex, &hex));
        let mut chars: Vec<char> = hex.chars().collect();
        let i = idx % chars.len().max(1);
        chars[i] = if chars[i] == '0' { '1' } else { '0' };
        let bad: String = chars.into_iter().collect();
        prop_assert!(!crate::util::ct_eq(&hex, &bad));
    }
}

async fn make_state() -> (WebState, tempfile::TempDir) {
    // reuse web test helper pattern
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db").to_str().unwrap().to_string();
    let db = Db::new(&db_path).unwrap();
    db.create_web_account("owner", "secret", "owner", None)
        .await
        .unwrap();
    let cfg = Config {
        owner_id: 1,
        db_path: db_path.clone(),
        crypto_token: None,
        crypto_api_base: None,
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
    let bot = teloxide::Bot::new("0:setup-placeholder".to_string());
    let db = Arc::new(db);
    let bots = Arc::new(crate::bots::BotManager::placeholder(
        db.clone(),
        Arc::new(cfg.clone()),
        crate::identity::IdentityHandle::unconnected(),
    ));
    let state = WebState {
        db: db.clone(),
        cfg,
        bot,
        bots,
        identity: crate::identity::IdentityHandle::unconnected(),
        login_attempts: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
        ai_test_inflight: Arc::new(tokio::sync::Mutex::new(())),
        base_path: String::new(),
    };
    (state, dir)
}

#[tokio::test]
async fn health_ok() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_login_and_me() {
    // Privileged accounts: a correct password is NOT enough — the web
    // console forces TOTP enrollment before any session, so REST refuses
    // (BLOCK-1 parity).
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"owner","password":"secret"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"].as_str().unwrap(), "mfa_required");

    // Non-privileged accounts without TOTP still get a session token.
    let (state, _dir) = make_seller_state().await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"seller","password":"secret"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let token = v["token"].as_str().unwrap().to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/me")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

fn api_pending(db: &Db, account_id: i64, raw: &str) {
    let expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    db.create_pending_mfa(sha256_hex(raw), account_id, "verify", None::<&str>, expires)
        .unwrap();
}

fn mfa_json_req(pending: &str, code: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/v1/auth/mfa/verify")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({"pending_token": pending, "code": code}).to_string(),
        ))
        .unwrap()
}

#[tokio::test]
async fn api_mfa_verify_rejects_stored_recovery_code_when_disabled() {
    const RAW: &str = "api-pending-recovery-off";
    const PLAIN: &str = "echo-foxtrot-golf-hotel";
    let (mut state, _dir) = make_state().await;
    state.cfg.mfa_recovery_codes_enabled = false;
    let db = state.db.clone();
    let (acc_id, _, _, _, _) = db.get_web_account_by_username("owner").unwrap();
    db.enable_totp(acc_id, "JBSWY3DPEHPK3PXP").unwrap();
    let hashed = bcrypt::hash(PLAIN, bcrypt::DEFAULT_COST).unwrap_or_default();
    db.create_recovery_codes(acc_id, [hashed]).unwrap();
    api_pending(&db, acc_id, RAW);

    let app = router(state.clone());
    let resp = app.clone().oneshot(mfa_json_req(RAW, PLAIN)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["error"]["code"].as_str(), Some("invalid_code"));
    assert_eq!(
        v["error"]["message"].as_str(),
        Some("invalid authenticator code")
    );
    assert_eq!(
        db.list_unused_recovery_codes(acc_id),
        1,
        "code stays dormant"
    );

    state.cfg.mfa_recovery_codes_enabled = true;
    let app = router(state);
    let resp = app.oneshot(mfa_json_req(RAW, PLAIN)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(db.list_unused_recovery_codes(acc_id), 0, "code revives");
}

#[tokio::test]
async fn api_mfa_verify_accepts_totp_when_recovery_disabled() {
    const SECRET: &str = "JBSWY3DPEHPK3PXP";
    const RAW: &str = "api-pending-totp-off";
    let (mut state, _dir) = make_state().await;
    state.cfg.mfa_recovery_codes_enabled = false;
    let db = state.db.clone();
    let (acc_id, _, _, _, _) = db.get_web_account_by_username("owner").unwrap();
    db.enable_totp(acc_id, SECRET).unwrap();
    api_pending(&db, acc_id, RAW);
    let now = chrono::Utc::now().timestamp() as u64;
    let code = crate::util::totp_generate(SECRET, now, 6usize, 30u64);
    let app = router(state);
    let resp = app.oneshot(mfa_json_req(RAW, &code)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(v["token"].as_str().is_some());
}

/// F2: the REST login must record failures under the same namespaced key as
/// the web login (`login:{username}`), never under the raw username (which
/// would create a second bucket and could collide with `mfa:` /
/// `identity-unlock:` entries in the shared map).
#[tokio::test]
async fn api_login_uses_namespaced_throttle_key() {
    assert_eq!(crate::auth::login_throttle_key("owner"), "login:owner");
    let (state, _dir) = make_state().await;
    let attempts = state.login_attempts.clone();
    let app = router(state);
    // Wrong password: the attempt is counted and not cleared by login_ok.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"username":"owner","password":"wrong-password"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let map = attempts.lock().unwrap();
    assert!(
        map.contains_key("login:owner"),
        "API login must record under the shared login: key, got keys: {:?}",
        map.keys().collect::<Vec<_>>()
    );
    assert!(
        !map.contains_key("owner"),
        "API login must not create a raw-username bucket"
    );
}

/// F2 cross-surface proof: attempts recorded through the shared helper (as
/// the web login does) are enforced by the REST login, even with a correct
/// password — the throttle check runs before credential verification.
#[tokio::test]
async fn api_login_sees_web_throttle_attempts() {
    let (state, _dir) = make_state().await;
    let key = crate::auth::login_throttle_key("owner");
    let mut locked = false;
    for _ in 0..5 {
        locked = crate::auth::login_throttle(&state.login_attempts, &key, 5, 900);
    }
    assert!(locked, "five shared-helper failures must lock the bucket");
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"username":"owner","password":"secret"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"].as_str().unwrap(), "too_many_attempts");
}

#[tokio::test]
async fn api_unauth_returns_401() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dashboard/summary")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---- helpers ----
async fn make_seller_state() -> (WebState, tempfile::TempDir) {
    let (state, dir) = make_state().await;
    state
        .db
        .create_web_account("seller", "secret", "seller", Some(42))
        .await
        .unwrap();
    state
        .db
        .upsert_seller(42, Some("seller42"), Some("@chan42"));
    (state, dir)
}
fn owner_token(state: &WebState) -> String {
    let (id, _, _, _, _) = state.db.get_web_account_by_username("owner").unwrap();
    state.db.create_web_session(id, "owner", None, 7)
}
fn seller_token(state: &WebState) -> String {
    let (id, _, _, _, _) = state.db.get_web_account_by_username("seller").unwrap();
    state.db.create_web_session(id, "seller", Some(42), 7)
}

// ---- Auth ----
#[tokio::test]
async fn api_me_returns_role() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/me")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["role"].as_str().unwrap(), "owner");
}

#[tokio::test]
async fn api_logout_revokes() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/logout")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/me")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ---- Plans ----
#[tokio::test]
async fn api_plans_create_list_get() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let db = state.db.clone();
    let app = router(state);
    let resp = app.clone().oneshot(Request::builder().method("POST").uri("/api/v1/plans")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {}", token))
            .body(Body::from(r#"{"name":"Plan1","price":"10","asset":"USDT","duration_days":30,"channel_id":"@chan42"}"#)).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/plans")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["total"].as_i64().unwrap(), 1);
    let plan_id = db.get_plan(42, "Plan1").unwrap().0;
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/plans/{}", plan_id))
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["name"].as_str().unwrap(), "Plan1");
}

#[tokio::test]
async fn api_plans_update_delete() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let db = state.db.clone();
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/plans")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(
                    r#"{"name":"PlanU","price":"10","asset":"USDT","duration_days":30}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let plan_id = db.get_plan(42, "PlanU").unwrap().0;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/plans/{}", plan_id))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"price":"20"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let plan_id = db.get_plan(42, "PlanU").unwrap().0;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/plans/{}", plan_id))
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/plans")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["total"].as_i64().unwrap(), 0);
}

// ---- Subscriptions ----
#[tokio::test]
async fn api_subs_create_list_get_patch() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let owner = owner_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/subscriptions")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(
                    r#"{"user_id":7,"channel_id":"@chan42","days":10}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let sub_id = v["id"].as_i64().unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/subscriptions")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/subscriptions/{}", sub_id))
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // PATCH is an owner-only mutation: a seller is refused even for its own row.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/subscriptions/{}", sub_id))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"status":"expired"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    // The owner still has the successful path.
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/subscriptions/{}", sub_id))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", owner))
                .body(Body::from(r#"{"status":"expired"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---- Invoices ----
#[tokio::test]
async fn api_invoices_create_list_get_status() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/invoices")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","description":"test"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let inv_id = v["invoice_id"].as_str().unwrap().to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/invoices")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/invoices/{}", inv_id))
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/invoices/{}/status", inv_id))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"status":"paid"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // Manual 'paid' transitions are rejected: paid is a verified state
    // reachable only via the webhook or claim flow.
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/invoices/{}/status", inv_id))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"status":"expired"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/invoices/{}", inv_id))
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"].as_str().unwrap(), "expired");
}

// ---- Codes ----
#[tokio::test]
async fn api_codes_create_list_get_redeem() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/codes")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"value_days":7,"max_uses":1,"count":1}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let code = v["codes"][0].as_str().unwrap().to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/codes")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/codes/{}", code))
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/codes/{}/redeem", code))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"user_id":99}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---- Posts ----
#[tokio::test]
async fn api_posts_create_list() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/posts")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"text":"hello","channel":"@chan42"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/posts")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["total"].as_i64().unwrap() >= 1);
}

// ---- Users ----
#[tokio::test]
async fn api_users_list_upsert() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/users/upsert")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"user_id":555,"username":"foo"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/users")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = v["items"].as_array().unwrap();
    assert!(items.iter().any(|x| x["user_id"].as_i64() == Some(555)));
}

// ---- Stats ----
#[tokio::test]
async fn api_dashboard_summary() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/dashboard/summary")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_revenue() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/revenue")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---- Channels ----
#[tokio::test]
async fn api_channels_list() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/channels")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---- Config ----
#[tokio::test]
async fn api_config_get_set() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/config/channel")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/config/channel")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"channel":"@cfg"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/config/channel")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["channel"].as_str().unwrap(), "@cfg");
}

// ---- AI ----
#[tokio::test]
async fn api_ai_templates_crud() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/ai/templates")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"name":"t1","user_prompt":"hello {{x}}"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/ai/templates")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = v["items"].as_array().unwrap();
    assert!(items.iter().any(|x| x["name"].as_str() == Some("t1")));
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/ai/templates/t1")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/ai/templates/t1")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"description":"d"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/ai/templates/t1")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---- Finance ----
#[tokio::test]
async fn api_finance_summary() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/finance/summary")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---- Admin (owner) ----
#[tokio::test]
async fn api_admin_stats() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/stats/global")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_admin_sellers() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/sellers")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_admin_web_accounts_crud() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let db = state.db.clone();
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/admin/web-accounts")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(
                    r#"{"username":"ac1","password":"secret123","role":"seller","seller_id":42}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/web-accounts")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = v["items"].as_array().unwrap();
    let acct = items
        .iter()
        .find(|x| x["username"].as_str() == Some("ac1"))
        .unwrap();
    let id = acct["id"].as_i64().unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/admin/web-accounts/{}", id))
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let _ = db;
}

// ---- Role gating ----
#[tokio::test]
async fn api_seller_cannot_access_admin() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/stats/global")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// Ordered-role gate semantics: an Admin-level session satisfies an Admin
/// gate but must not satisfy an Owner-only gate.
#[tokio::test]
async fn api_admin_passes_admin_gate_but_fails_owner_gate() {
    let (state, _dir) = make_state().await;
    state
        .db
        .create_web_account("admin", "secret", "admin", None)
        .await
        .unwrap();
    let (aid, _, _, _, _) = state.db.get_web_account_by_username("admin").unwrap();
    let token = state.db.create_web_session(aid, "admin", None, 7);
    let app = router(state);

    // Admin gate: /api/v1/admin/sellers accepts Admin or Owner.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/sellers")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "admin must pass an Admin-level gate"
    );

    // Owner gate: /api/v1/admin/admins accepts Owner only.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/admins")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "admin must fail an Owner-level gate"
    );
}

#[tokio::test]
async fn api_unauth_all_protected() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    for uri in [
        "/api/v1/plans",
        "/api/v1/users",
        "/api/v1/dashboard/summary",
    ] {
        let resp = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "uri {}", uri);
    }
}

// ---- Server-side additions (2026-10-25): auth_password, codes_delete, /health,
// /metrics. Reuse the module's state helpers above.

async fn change_password(app: &Router, token: &str, body: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/password")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn delete_code(app: &Router, token: &str, code: &str) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/codes/{}", code).as_str())
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn auth_password_non_owner_requires_current() {
    let (state, _dir) = make_seller_state().await;
    let app = router(state.clone());
    let token = seller_token(&state);
    // Missing current password -> rejected, nothing changes.
    let resp = change_password(&app, &token, r#"{"new_password":"brandnewpw"}"#).await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    // Wrong current password -> rejected.
    let resp = change_password(
        &app,
        &token,
        r#"{"new_password":"brandnewpw","current_password":"wrong"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(
        state
            .db
            .verify_web_password("seller", "secret")
            .await
            .is_some()
    );
    // Correct current password -> the hash rotates (same bcrypt path the
    // login uses) and the old password stops verifying.
    let resp = change_password(
        &app,
        &token,
        r#"{"new_password":"brandnewpw","current_password":"secret"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        state
            .db
            .verify_web_password("seller", "brandnewpw")
            .await
            .is_some()
    );
    assert!(
        state
            .db
            .verify_web_password("seller", "secret")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn auth_password_invalidates_other_sessions() {
    let (state, _dir) = make_seller_state().await;
    let app = router(state.clone());
    let token_a = seller_token(&state);
    let token_b = seller_token(&state);
    let resp = change_password(
        &app,
        &token_a,
        r#"{"new_password":"brandnewpw","current_password":"secret"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    // The caller's session survives the rotation...
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/me")
                .header("authorization", format!("Bearer {}", token_a))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // ...every OTHER session of the account is gone.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/me")
                .header("authorization", format!("Bearer {}", token_b))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_password_owner_changes_without_current() {
    let (state, _dir) = make_state().await;
    let app = router(state.clone());
    let token = owner_token(&state);
    let resp = change_password(&app, &token, r#"{"new_password":"newownerpw"}"#).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        state
            .db
            .verify_web_password("owner", "newownerpw")
            .await
            .is_some()
    );
    assert!(
        state
            .db
            .verify_web_password("owner", "secret")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn codes_delete_is_seller_scoped_and_real() {
    let (state, _dir) = make_seller_state().await;
    let own = state.db.create_code(42, None, Some(30), 1, None, 12);
    let other = state.db.create_code(43, None, Some(30), 1, None, 12);
    let app = router(state.clone());
    let seller = seller_token(&state);

    // Own code: the row is actually gone.
    let resp = delete_code(&app, &seller, &own).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        state.db.get_code(&own).is_none(),
        "code row must be deleted"
    );

    // Cross-seller: rejected, the row survives untouched.
    let resp = delete_code(&app, &seller, &other).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        state.db.get_code(&other).is_some(),
        "cross-seller delete must not touch the code"
    );

    // Admin/owner may delete any seller's code.
    let owner = owner_token(&state);
    let resp = delete_code(&app, &owner, &other).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(state.db.get_code(&other).is_none());
}

#[tokio::test]
async fn codes_delete_unknown_returns_404() {
    let (state, _dir) = make_seller_state().await;
    let app = router(state.clone());
    let token = owner_token(&state);
    let resp = delete_code(&app, &token, "NOPE_NOPE_NOPE").await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn health_reports_components() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "ok");
    assert_eq!(v["db"], "ok");
    assert_eq!(v["bot"], "configured");
    assert_eq!(v["mcp"], "disabled"); // make_state: mcp_owner_token = None
    assert!(v["uptime_seconds"].is_u64());
}

#[tokio::test]
async fn metrics_scrape_ok_and_no_raw_path_labels() {
    let (state, _dir) = make_seller_state().await;
    // Mirror main.rs: metrics middleware over the API router.
    let app = router(state).layer(axum::middleware::from_fn(crate::metrics::track_http));
    let secret = "sub_tok_9f8e7d6c5b4a";
    // Hit a real route, then an UNMATCHED route whose path AND query
    // string carry a token-like secret — the house-lesson attack shape:
    // a metrics endpoint that labels by raw path leaks it verbatim.
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/v1/codes/{}/nope?token={}", secret, secret).as_str())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ct.starts_with("text/plain"), "content-type {ct}");
    let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body).to_string();
    assert!(!text.contains(secret), "metrics leaked raw path: {text}");
    assert!(text.contains("http_requests_total{route=\"health\",status_class=\"2xx\"}"));
    // The unmatched token-bearing route lands in the fixed catch-all
    // category, never the raw path.
    assert!(text.contains("http_requests_total{route=\"other\",status_class=\"4xx\"}"));
    assert!(text.contains("# TYPE uptime_seconds gauge"));
    assert!(text.contains("bot_messages_sent_total "));
}

// ---- Products ----
#[tokio::test]
async fn api_products_create_list_delete() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/products")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(
                    r#"{"name":"Widget","price":"5","asset":"USDT","kind":"physical","stock":3}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/products")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["total"].as_i64().unwrap(), 1);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/products/Widget")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/products")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["total"].as_i64().unwrap(), 1);
    assert_eq!(v["items"][0]["active"].as_i64().unwrap(), 0);
}

#[tokio::test]
async fn api_products_digital_requires_delivery() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/products")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(
                    r#"{"name":"Dig","price":"5","asset":"USDT","kind":"digital_link"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn api_products_delete_not_found() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/products/Nope")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_shop_browse() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state.clone());
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/products")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(
                    r#"{"name":"ShopItem","price":"5","asset":"USDT","kind":"physical"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/shop")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let items = v["items"].as_array().unwrap();
    assert!(items.iter().any(|x| x["name"].as_str() == Some("ShopItem")));
}

// ---- Admin seller detail ----
#[tokio::test]
async fn api_admin_seller_detail() {
    let (state, _dir) = make_seller_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/sellers/42")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/admin/sellers/42")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"default_channel":"@chan42"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---- API keys ----
#[tokio::test]
async fn api_api_keys_create_list_revoke() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/auth/api-keys")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(r#"{"name":"k1","scopes":"read"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = v["id"].as_i64().unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/auth/api-keys")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["total"].as_i64().unwrap() >= 1);
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v1/auth/api-keys/{}", id))
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---- Invoices claim / webhook ----
#[tokio::test]
async fn api_invoices_claim_no_crypto() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let db = state.db.clone();
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/invoices")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","description":"t","user_id":42}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let inv_id = v["invoice_id"].as_str().unwrap().to_string();
    // make invoice active (already) then claim
    db.update_invoice_status(&inv_id, "active");
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/invoices/{}/claim", inv_id))
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn api_webhook_cryptopay_no_token() {
    let (state, _dir) = make_state().await;
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/webhooks/cryptopay")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"payload":{"invoice_id":1,"status":"paid"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn api_webhook_cryptopay_missing_signature() {
    let (mut state, _dir) = make_state().await;
    state.cfg.crypto_token = Some("secret".to_string());
    state.db.set_config("crypto_token", "secret");
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/webhooks/cryptopay")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"payload":{"invoice_id":1,"status":"paid"}}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Deterministic end-to-end slice of the payment path: signed webhook ->
/// invoice queued -> atomic claim -> verified against a local provider stub ->
/// exactly one subscription -> replay does not re-grant. No real network:
/// the provider is a wiremock server and the Telegram side effects are not
/// driven here (the grant row is the durable, auditable outcome).
#[tokio::test]
async fn signed_webhook_to_single_grant_path() {
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let (mut state, _dir) = make_state().await;
    const TOKEN: &str = "webhook-secret";
    state.cfg.crypto_token = Some(TOKEN.to_string());
    let db = state.db.clone();
    db.upsert_seller(7, None, None);
    db.add_plan(7, "e2e-plan", "5", "USDT", 7, Some("@e2e"));
    db.save_invoice(
        "e2e-inv",
        7,
        "5",
        "USDT",
        Some("e2e-plan"),
        "pending",
        None,
        Some("e2e-plan"),
        Some(42),
        None,
    );

    let app = router(state);
    let post_webhook = |app: axum::Router| {
        let app = app.clone();
        async move {
            let body = r#"{"payload":{"invoice_id":"e2e-inv","status":"paid"}}"#;
            let mac = crate::util::hmac_sha256(TOKEN.as_bytes(), body.as_bytes());
            let sig: String = mac.iter().map(|b| format!("{:02x}", b)).collect();
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/webhooks/cryptopay")
                    .header("content-type", "application/json")
                    .header("x-crypto-pay-api-signature", sig)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };

    // 1. Signed webhook enqueues the invoice (never marks it paid directly).
    let resp = post_webhook(app.clone()).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(db.get_invoice("e2e-inv").unwrap().2, "active");

    // 2. Granter claim is atomic: one winner, no second claim.
    assert!(db.claim_invoice_for_grant("e2e-inv"));
    assert!(!db.claim_invoice_for_grant("e2e-inv"));
    assert_eq!(db.get_invoice("e2e-inv").unwrap().2, "granting");

    // 3. Provider stub returns paid; verification is the real client + parser.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/getInvoices"))
        .and(body_partial_json(
            serde_json::json!({"invoice_ids": "e2e-inv"}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {"items": [{"invoice_id": "e2e-inv", "status": "paid"}]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let cp = crate::crypto::CryptoPay::with_base_url(TOKEN.to_string(), &server.uri());
    let verified = cp.get_invoice("e2e-inv").await.unwrap();
    assert!(
        extract_invoice_items(&verified)
            .iter()
            .any(|x| x.get("status").and_then(|s| s.as_str()) == Some("paid"))
    );

    // 4. Exactly-once grant using the same primitives as the granter:
    // subscription_exists_for_invoice guard + UNIQUE(invoice_id) index.
    if !db.subscription_exists_for_invoice("e2e-inv") {
        db.create_subscription(7, 42, None, "@e2e", 7, Some("e2e-inv"), None);
    }
    assert_eq!(db.subscriptions_for_invoice("e2e-inv").len(), 1);
    db.finalize_invoice_paid("e2e-inv");
    assert_eq!(db.get_invoice("e2e-inv").unwrap().2, "paid");

    // 5. Replay: the webhook must not resurrect a paid invoice into the
    // grant queue, and no second subscription may appear.
    let resp = post_webhook(app).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(db.get_invoice("e2e-inv").unwrap().2, "paid");
    assert!(db.pending_invoices().iter().all(|x| x.0 != "e2e-inv"));
    assert_eq!(db.subscriptions_for_invoice("e2e-inv").len(), 1);
}

/// The console crypto form writes through the secret setter: a saved token
/// is never persisted as plaintext in the config table, and a failed save
/// renders a terse error banner instead of a success flash.
#[tokio::test]
async fn crypto_form_uses_secret_setter() {
    use rusqlite::OptionalExtension;

    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let db_path = state.cfg.db_path.clone();
    let app = crate::web::router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/crypto/setup")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", format!("session={token}"))
                .body(Body::from(format!(
                    "token=form-token-123&csrf_token={}",
                    crate::auth::csrf_token_for_session(&token)
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SEE_OTHER);
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let raw: Option<String> = {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.query_row(
            "SELECT value FROM config WHERE key='crypto_token'",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
    };
    if std::env::var_os("MASTER_KEY").is_none() {
        // No key: the form refuses the write and says so.
        assert!(location.contains("error=secret"), "{location}");
        assert!(raw.is_none(), "plaintext token was persisted");
    } else {
        assert!(location.contains("saved=1"));
        assert!(raw.as_deref().is_some_and(|v| v.starts_with("enc:v1:")));
    }

    // The terse error banner is rendered from the fixed query value.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/crypto?error=secret")
                .header("cookie", format!("session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("Token not saved"));
    assert!(!html.contains("form-token-123"));
}

/// Mixed-asset revenue must never appear as one summed float in the REST
/// stat responses: per-asset lines are present and the total is a marker.
#[tokio::test]
async fn api_revenue_mixed_assets_never_a_float() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    state.db.save_invoice(
        "mix-b", 1, "0.5", "BTC", None, "paid", None, None, None, None,
    );
    state.db.save_invoice(
        "mix-u", 1, "50", "USDT", None, "paid", None, None, None, None,
    );
    let app = router(state);
    for uri in [
        "/api/v1/dashboard/summary",
        "/api/v1/revenue",
        "/api/v1/finance/summary",
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "{uri}");
        let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let total = v["revenue_total"].as_str().or_else(|| v["total"].as_str());
        assert_eq!(total, Some("mixed assets"), "{uri}: {v}");
        let lines = v["revenue_by_asset"]
            .as_array()
            .or_else(|| v["by_asset"].as_array())
            .unwrap();
        assert_eq!(lines.len(), 2, "{uri}: {v}");
    }
}

// ---- Finance payouts ----
#[tokio::test]
async fn api_finance_payouts_list_create() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/payouts")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","destination":"123"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/finance/payouts")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["total"].as_i64().unwrap() >= 1);
}

#[tokio::test]
async fn api_finance_payouts_approve_process() {
    let (state, _dir) = make_seller_state().await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/payouts")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", seller))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","destination":"123"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = v["id"].as_i64().unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/payouts/{}/approve", id))
                .header("authorization", format!("Bearer {}", owner))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/payouts/{}/process", id))
                .header("authorization", format!("Bearer {}", owner))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_finance_payouts_patch() {
    let (state, _dir) = make_seller_state().await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/payouts")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", seller))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","destination":"123"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = v["id"].as_i64().unwrap();
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/payouts/{}/approve", id))
                .header("authorization", format!("Bearer {}", owner))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/finance/payouts/{}", id))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", owner))
                .body(Body::from(r#"{"status":"failed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_finance_payouts_execute_no_crypto() {
    let (state, _dir) = make_seller_state().await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/payouts")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", seller))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","destination":"123"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = v["id"].as_i64().unwrap();
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/payouts/{}/approve", id))
                .header("authorization", format!("Bearer {}", owner))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/payouts/{}/execute", id))
                .header("authorization", format!("Bearer {}", owner))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ---- Finance refunds ----

#[tokio::test]
async fn api_finance_refunds_list_create() {
    let (state2, _dir2) = make_seller_state().await;
    let token2 = seller_token(&state2);
    let app2 = router(state2.clone());
    let db2 = state2.db.clone();
    let resp = app2
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/invoices")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token2))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","description":"t"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let inv_id = v["invoice_id"].as_str().unwrap().to_string();
    db2.update_invoice_status(&inv_id, "paid");
    let resp = app2
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/refunds")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token2))
                .body(Body::from(format!(r#"{{"invoice_id":"{}"}}"#, inv_id)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app2
        .oneshot(
            Request::builder()
                .uri("/api/v1/finance/refunds")
                .header("authorization", format!("Bearer {}", token2))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["total"].as_i64().unwrap() >= 1);
}

#[tokio::test]
async fn api_finance_refunds_approve_process() {
    let (state, _dir) = make_seller_state().await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let db = state.db.clone();
    let app = router(state);
    // create paid invoice
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/invoices")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", seller))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","description":"t"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let inv_id = v["invoice_id"].as_str().unwrap().to_string();
    db.update_invoice_status(&inv_id, "paid");
    // create refund
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/refunds")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", seller))
                .body(Body::from(format!(r#"{{"invoice_id":"{}"}}"#, inv_id)))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = v["id"].as_i64().unwrap();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{}/approve", id))
                .header("authorization", format!("Bearer {}", owner))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{}/process", id))
                .header("authorization", format!("Bearer {}", owner))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_finance_refunds_execute_no_crypto() {
    let (state, _dir) = make_seller_state().await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let db = state.db.clone();
    let app = router(state);
    // create paid invoice bound to user 99
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/invoices")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", seller))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","description":"t","user_id":99}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let inv_id = v["invoice_id"].as_str().unwrap().to_string();
    db.update_invoice_status(&inv_id, "paid");
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/refunds")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", seller))
                .body(Body::from(format!(r#"{{"invoice_id":"{}"}}"#, inv_id)))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let id = v["id"].as_i64().unwrap();
    let _ = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{}/approve", id))
                .header("authorization", format!("Bearer {}", owner))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{}/execute", id))
                .header("authorization", format!("Bearer {}", owner))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ---- Finance other ----
#[tokio::test]
async fn api_finance_revenue_by_seller() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/finance/revenue-by-seller")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_finance_export() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/finance/export")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// ---- Admin audit log ----
#[tokio::test]
async fn api_admin_audit_log() {
    let (state, _dir) = make_state().await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/admin/audit-log")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_products_duplicate_conflict() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let body = r#"{"name":"Dup","price":"5","asset":"USDT","kind":"physical"}"#;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/products")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/products")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn api_finance_refunds_requires_paid_invoice() {
    let (state, _dir) = make_seller_state().await;
    let token = seller_token(&state);
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/invoices")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","description":"t"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let inv_id = v["invoice_id"].as_str().unwrap().to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/refunds")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", token))
                .body(Body::from(format!(r#"{{"invoice_id":"{}"}}"#, inv_id)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---- Production-blocker regressions (security REST audit / core ops audit) ----

/// Point the effective bot at a wiremock Bot API so channel/post mutations in
/// these tests never reach the real Telegram transport. The returned server
/// lets a test prove that a gate held BEFORE any transport work (zero hits).
async fn attach_mock_bot(state: &mut WebState) -> wiremock::MockServer {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": true
            })),
        )
        .mount(&server)
        .await;
    let bot = teloxide::Bot::new("9:placeholder")
        .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());
    let ctx = Arc::new(crate::bots::BotContext {
        id: 9,
        key: "mock".to_string(),
        bot,
        owner_id: 0,
        shared: Arc::new(state.cfg.clone()),
        db: state.db.clone(),
    });
    state.bots = Arc::new(crate::bots::BotManager {
        bots: vec![ctx.clone()],
        primary: ctx,
        db: state.db.clone(),
        identity: crate::identity::IdentityHandle::unconnected(),
    });
    server
}

// H1: subs_patch must be owner-only and must resolve the target inside the
// caller's tenant scope before mutating it.
#[tokio::test]
async fn subs_patch_rejects_non_owner_and_leaves_row_untouched() {
    let (state, _dir) = make_seller_state().await;
    let sub_id = state
        .db
        .create_subscription(42, 4242, None, "@chan42", 7, None, None);
    let token = seller_token(&state);
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/subscriptions/{sub_id}"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"status":"expired"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let row = state
        .db
        .list_subscriptions(Some(42), 1000)
        .into_iter()
        .find(|s| s.0 == sub_id)
        .expect("subscription must still exist");
    assert_ne!(
        row.4, "expired",
        "a seller must not expire any subscription"
    );
}

#[tokio::test]
async fn subs_patch_owner_expires_scoped_id_and_404s_unknown_id() {
    let (state, _dir) = make_state().await;
    let sub_id = state
        .db
        .create_subscription(42, 4242, None, "@chan42", 7, None, None);
    let token = owner_token(&state);
    let app = router(state.clone());

    // Unknown id fails closed: nothing outside the resolved scope is touched.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/subscriptions/999999")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"status":"expired"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // In-scope id succeeds for the owner.
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/subscriptions/{sub_id}"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"status":"expired"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let row = state
        .db
        .list_subscriptions(None, 1000)
        .into_iter()
        .find(|s| s.0 == sub_id)
        .expect("subscription must exist");
    assert_eq!(row.4, "expired");
}

// H2: the GLOBAL default channel is owner-only; a seller must get 403.
#[tokio::test]
async fn config_set_channel_is_owner_only() {
    let (state, _dir) = make_seller_state().await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let app = router(state.clone());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/config/channel")
                .header("authorization", format!("Bearer {seller}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"channel":"@hijacked"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_ne!(state.db.get_config("channel").as_deref(), Some("@hijacked"));

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/config/channel")
                .header("authorization", format!("Bearer {owner}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"channel":"@ownerchan"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        state.db.get_config("channel").as_deref(),
        Some("@ownerchan")
    );
}

// H3: channels_patch must apply the same tenant gate as the web console.
#[tokio::test]
async fn channels_patch_blocks_cross_tenant_deface_before_transport() {
    let (mut state, _dir) = make_seller_state().await;
    state.db.add_channel("@victim", 99).unwrap();
    let server = attach_mock_bot(&mut state).await;
    let token = seller_token(&state);
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/channels/@victim")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"title":"PWNED"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "a cross-tenant channel edit must never reach the transport"
    );
}

#[tokio::test]
async fn channels_patch_allows_own_channel_and_default_channel() {
    let (mut state, _dir) = make_seller_state().await;
    state.db.add_channel("@mine", 42).unwrap();
    let _server = attach_mock_bot(&mut state).await;
    let token = seller_token(&state);
    let app = router(state.clone());

    for channel in ["@mine", "@chan42"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!("/api/v1/channels/{channel}"))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"title":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "channel {channel}");
    }
}

#[tokio::test]
async fn channels_patch_owner_may_edit_any_channel() {
    let (mut state, _dir) = make_state().await;
    state.db.add_channel("@victim", 99).unwrap();
    let _server = attach_mock_bot(&mut state).await;
    let token = owner_token(&state);
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri("/api/v1/channels/@victim")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"title":"owner edit"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

// M2: posts_update must call gate_outbound before edit_message_text.
#[tokio::test]
async fn posts_update_holds_matching_term_before_transport() {
    let (mut state, _dir) = make_seller_state().await;
    state
        .db
        .log_post(42, "@chan42", 55, "text", Some("original"), "bot")
        .unwrap();
    let post_id = state.db.list_posts(Some(42), 1000)[0].0;
    assert!(state.db.add_blocked_word(42, "floorterm", "hide", true));
    let server = attach_mock_bot(&mut state).await;
    let token = seller_token(&state);
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/posts/{post_id}"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"content":"please read floorterm now"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"].as_str(), Some("moderation_hold"));
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "the matching term must hold the edit before any transport work"
    );
    let content = state.db.list_posts(Some(42), 1000)[0].5.clone();
    assert_eq!(
        content.as_deref(),
        Some("original"),
        "held edit must not update the row"
    );
}

#[tokio::test]
async fn posts_update_allows_clean_content_after_gate() {
    let (mut state, _dir) = make_seller_state().await;
    state
        .db
        .log_post(42, "@chan42", 56, "text", Some("original"), "bot")
        .unwrap();
    let post_id = state.db.list_posts(Some(42), 1000)[0].0;
    let server = attach_mock_bot(&mut state).await;
    let token = seller_token(&state);
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/v1/posts/{post_id}"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"content":"clean edited text"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    let content = state.db.list_posts(Some(42), 1000)[0].5.clone();
    assert_eq!(content.as_deref(), Some("clean edited text"));
}

// F1: the scheduled-post worker resolves its sender from the TARGET channel
// (BotManager::for_channel), so a foreign destination would post attacker text
// under the victim bot's identity. The gate must fire before any transport work
// and before the row is written.
#[tokio::test]
async fn posts_schedule_blocks_cross_tenant_channel_before_transport_and_db_write() {
    let (mut state, _dir) = make_seller_state().await;
    state.db.add_channel("@victim", 99).unwrap();
    let server = attach_mock_bot(&mut state).await;
    let token = seller_token(&state);
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/posts/schedule")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"channel":"@victim","text":"attacker text","scheduled_at":"2030-01-01T00:00:00+00:00"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "a cross-tenant scheduled post must never reach the transport"
    );
    assert!(
        state.db.list_scheduled_posts(None).is_empty(),
        "a refused scheduled post must leave no row in any tenant scope"
    );
}

#[tokio::test]
async fn posts_schedule_allows_own_default_and_owner_targets() {
    let (mut state, _dir) = make_seller_state().await;
    state.db.add_channel("@mine", 42).unwrap();
    state.db.add_channel("@victim", 99).unwrap();
    let _server = attach_mock_bot(&mut state).await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let app = router(state.clone());

    for (token, ch) in [
        (&seller, "@mine"),
        (&seller, "@chan42"),
        (&owner, "@victim"),
    ] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/posts/schedule")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"channel":"{ch}","text":"scheduled text","scheduled_at":"2030-01-01T00:00:00+00:00"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED, "channel {ch}");
    }
    let rows = state.db.list_scheduled_posts(None);
    assert_eq!(rows.len(), 3);
    for ch in ["@mine", "@chan42", "@victim"] {
        assert!(
            rows.iter().any(|r| r.2 == ch),
            "expected a scheduled row for {ch}"
        );
    }
}

// Sibling check: immediate post creation must not accept a foreign channel
// target either. Its send uses the caller's bot, but the destination is still
// client-supplied and logged, so the console's composer gate applies here too.
#[tokio::test]
async fn posts_create_blocks_cross_tenant_channel_before_transport() {
    let (mut state, _dir) = make_seller_state().await;
    state.db.add_channel("@victim", 99).unwrap();
    let server = attach_mock_bot(&mut state).await;
    let token = seller_token(&state);
    let app = router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/posts")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"text":"attacker text","channel":"@victim"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "a cross-tenant immediate post must never reach the transport"
    );
    assert!(state.db.list_posts(Some(42), 1000).is_empty());
}

#[tokio::test]
async fn posts_create_allows_own_default_and_global_channels() {
    let (mut state, _dir) = make_seller_state().await;
    state.db.add_channel("@mine", 42).unwrap();
    state.db.add_channel("@global", 0).unwrap();
    let _server = attach_mock_bot(&mut state).await;
    let token = seller_token(&state);
    let app = router(state.clone());

    for ch in ["@mine", "@chan42", "@global"] {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/posts")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"text":"hello {ch}","channel":"{ch}"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED, "channel {ch}");
    }
    assert_eq!(state.db.list_posts(Some(42), 1000).len(), 3);
}

// ---- Money safety: decision 2 owner gates / creator separation ----

fn admin_token(state: &WebState) -> String {
    let (id, _, _, _, _) = state.db.get_web_account_by_username("admin").unwrap();
    state.db.create_web_session(id, "admin", None, 7)
}

/// Create a paid invoice and a refund through the REST surface as `seller`.
async fn seed_refund(
    app: &axum::Router,
    db: &std::sync::Arc<Db>,
    seller: &str,
    idem: &str,
) -> (String, i64) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/invoices")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", seller))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","description":"t"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let inv = v["invoice_id"].as_str().unwrap().to_string();
    // Bind a buyer so the refund is executable (transfer target).
    db.save_invoice(
        &inv,
        42,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(99),
        None,
    );
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/refunds")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", seller))
                .body(Body::from(
                    serde_json::json!({
                        "invoice_id": inv, "idem_key": idem,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (inv, v["id"].as_i64().unwrap())
}

#[tokio::test]
async fn refund_approve_is_owner_only_and_separated_from_creator() {
    let (state, _dir) = make_seller_state().await;
    state
        .db
        .create_web_account("admin", "secret", "admin", None)
        .await
        .unwrap();
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let admin = admin_token(&state);
    let db = state.db.clone();
    let app = router(state);
    let (_inv, id) = seed_refund(&app, &db, &seller, "api:owner-gate").await;

    // Admin is refused with the spec's owner_required code.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{id}/approve"))
                .header("authorization", format!("Bearer {admin}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "owner_required");

    // The seller who created the refund is also refused (not a seller role
    // gate — creator separation fires after the owner check).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{id}/approve"))
                .header("authorization", format!("Bearer {seller}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Owner (not the creator) succeeds.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{id}/approve"))
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(db.get_refund(id).unwrap().7, "approved");
}

#[tokio::test]
async fn refund_creator_cannot_self_approve_even_as_owner() {
    let (state, _dir) = make_seller_state().await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let db = state.db.clone();
    let app = router(state);
    // Owner is allowed to CREATE (role >= Seller), which records them as the
    // creator; they must not then approve their own money row (no break-glass).
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/invoices")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {seller}"))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","description":"t"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let inv = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["invoice_id"]
        .as_str()
        .unwrap()
        .to_string();
    db.update_invoice_status(&inv, "paid");
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/refunds")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::from(format!(
                    r#"{{"invoice_id":"{inv}","idem_key":"api:self"}}"#
                )))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{id}/approve"))
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "creator_forbidden");
}

#[tokio::test]
async fn refund_execute_owner_gate_and_idem_audit() {
    let (state, _dir) = make_seller_state().await;
    state
        .db
        .create_web_account("admin", "secret", "admin", None)
        .await
        .unwrap();
    let seller = seller_token(&state);
    let admin = admin_token(&state);
    let owner = owner_token(&state);
    let db = state.db.clone();
    let app = router(state);
    let (_inv, id) = seed_refund(&app, &db, &seller, "api:exec-gate").await;
    db.set_refund_status(id, "approved");

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{id}/execute"))
                .header("authorization", format!("Bearer {admin}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"]["code"],
        "owner_required"
    );

    // Owner clears the gate; with no token configured the transfer is
    // unavailable (503) — proof execution reached the money path.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{id}/execute"))
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

    // Replaying the same refund key emits no second create audit event.
    let create_audits = db
        .list_audit_log(1000)
        .into_iter()
        .filter(|(_, _, _, action, tt, tid, _, _)| {
            action == "refund_create"
                && tt == "refund"
                && tid.as_deref() == Some(id.to_string().as_str())
        })
        .count();
    assert_eq!(create_audits, 1);
}

// ---- Money safety: decision 5 reconciliation endpoints ----

#[tokio::test]
async fn reconciliation_endpoints_owner_only_cas_and_resolve() {
    let (state, _dir) = make_seller_state().await;
    state
        .db
        .create_web_account("admin", "secret", "admin", None)
        .await
        .unwrap();
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let admin = admin_token(&state);
    let db = state.db.clone();
    let app = router(state);
    let (_inv, id) = seed_refund(&app, &db, &seller, "api:recon").await;
    db.set_refund_status(id, "approved");
    assert!(db.begin_refund_execution(id));
    // Move to needs_reconciliation with a cutoff in the future (injected time).
    let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    assert!(db.mark_needs_reconciliation("refund", id, &future));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/finance/reconciliation")
                .header("authorization", format!("Bearer {admin}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/finance/reconciliation")
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(v["items"].as_array().unwrap().iter().any(|r| r["id"] == id));

    // Empty reason is rejected before any state write.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/finance/reconciliation/refund/{id}/resolve"
                ))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::from(
                    r#"{"outcome":"void","reason":"   "}"#.to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/finance/reconciliation/refund/{id}/resolve"
                ))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::from(
                    r#"{"outcome":"void","reason":"receipt checked: never landed"}"#.to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(db.get_refund(id).unwrap().7, "void");

    // Append-only: a second resolution is rejected.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/finance/reconciliation/refund/{id}/resolve"
                ))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::from(
                    r#"{"outcome":"executed","reason":"second try"}"#.to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

// ---- security F1/F2/F5 remediation tests ----

/// Create a paid invoice bound to buyer 99 through the REST surface as
/// `seller`, with an explicit amount (used for full vs partial refunds).
async fn seed_paid_invoice(
    app: &axum::Router,
    db: &std::sync::Arc<Db>,
    seller: &str,
    amount: &str,
) -> String {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/invoices")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {seller}"))
                .body(Body::from(
                    serde_json::json!({
                        "amount": amount,
                        "asset": "USDT",
                        "description": "t",
                        "user_id": 99,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let inv = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["invoice_id"]
        .as_str()
        .unwrap()
        .to_string();
    db.save_invoice(
        &inv,
        42,
        amount,
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(99),
        None,
    );
    inv
}

/// F1: manual `process` on a PARTIAL refund must write status `executed`
/// (never legacy `processed`) and must leave the invoice paid with the
/// subscription untouched — no expiry, no kick.
#[tokio::test]
async fn finance_refunds_process_partial_keeps_invoice_paid_and_subscription_active() {
    let (state, _dir) = make_seller_state().await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let db = state.db.clone();
    let app = router(state);
    let inv = seed_paid_invoice(&app, &db, &seller, "5").await;
    // A live subscription bound to the invoice — must NOT be expired by a
    // partial settlement. Empty channel avoids a real Bot API call.
    let sub_id = db.create_subscription(42, 99, None, "", 30, Some(&inv), None);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/refunds")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {seller}"))
                .body(Body::from(
                    serde_json::json!({
                        "invoice_id": inv, "amount": "2", "idem_key": "api:f1-partial",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_eq!(db.get_refund(id).unwrap().4, "2"); // partial amount
    assert!(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/finance/refunds/{id}/approve"))
                    .header("authorization", format!("Bearer {owner}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
            .is_success()
    );

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{id}/process"))
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["status"], "executed");
    // No legacy status and no legacy side-effects.
    assert_eq!(db.get_refund(id).unwrap().7, "executed");
    assert_eq!(db.get_invoice(&inv).unwrap().2, "paid");
    let status = db
        .list_subscriptions(Some(42), 100)
        .into_iter()
        .find(|s| s.0 == sub_id)
        .map(|s| s.4);
    assert_eq!(status.as_deref(), Some("active"));
}

/// F1 counterpart: a FULL manual settlement still flips the invoice and
/// revokes the subscription, and the row is `executed`.
#[tokio::test]
async fn finance_refunds_process_full_settles_and_marks_executed() {
    let (state, _dir) = make_seller_state().await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let db = state.db.clone();
    let app = router(state);
    let inv = seed_paid_invoice(&app, &db, &seller, "5").await;
    let sub_id = db.create_subscription(42, 99, None, "", 30, Some(&inv), None);
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/refunds")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {seller}"))
                .body(Body::from(
                    serde_json::json!({
                        "invoice_id": inv, "idem_key": "api:f1-full",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert!(
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/finance/refunds/{id}/approve"))
                    .header("authorization", format!("Bearer {owner}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
            .is_success()
    );
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/finance/refunds/{id}/process"))
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(db.get_refund(id).unwrap().7, "executed");
    assert_eq!(db.get_invoice(&inv).unwrap().2, "refunded");
    let status = db
        .list_subscriptions(Some(42), 100)
        .into_iter()
        .find(|s| s.0 == sub_id)
        .map(|s| s.4);
    assert_eq!(status.as_deref(), Some("refunded"));
}

/// F2: REST reconciliation-resolve must enforce the same creator separation
/// as the web form for `refund` + `executed`.
#[tokio::test]
async fn reconciliation_execute_resolve_enforces_creator_separation() {
    let (state, _dir) = make_seller_state().await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let owner_id = state.db.get_web_account_by_username("owner").unwrap().0;
    let db = state.db.clone();
    let app = router(state);

    // Case A — the Owner created the refund: they must not be able to settle
    // their own money row through reconciliation.
    let inv_a = seed_paid_invoice(&app, &db, &seller, "5").await;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/refunds")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::from(
                    serde_json::json!({
                        "invoice_id": inv_a, "idem_key": "api:f2-owner-created",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let a_id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_eq!(db.refund_creator(a_id).unwrap().0, owner_id);
    db.set_refund_status(a_id, "approved");
    assert!(db.begin_refund_execution(a_id));
    let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    assert!(db.mark_needs_reconciliation("refund", a_id, &future));
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/finance/reconciliation/refund/{a_id}/resolve"
                ))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::from(
                    r#"{"outcome":"executed","reason":"owner attests receipt"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["error"]["code"], "creator_forbidden");
    // Nothing was resolved by the refused attempt.
    assert_eq!(db.get_refund(a_id).unwrap().7, "needs_reconciliation");

    // Case B — the Seller created the refund: a DIFFERENT owner may resolve.
    let inv_b = seed_paid_invoice(&app, &db, &seller, "5").await;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/finance/refunds")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {seller}"))
                .body(Body::from(
                    serde_json::json!({
                        "invoice_id": inv_b, "idem_key": "api:f2-seller-created",
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let b_id = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["id"]
        .as_i64()
        .unwrap();
    assert_ne!(db.refund_creator(b_id).unwrap().0, owner_id);
    db.set_refund_status(b_id, "approved");
    assert!(db.begin_refund_execution(b_id));
    assert!(db.mark_needs_reconciliation("refund", b_id, &future));
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v1/finance/reconciliation/refund/{b_id}/resolve"
                ))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::from(
                    r#"{"outcome":"executed","reason":"receipt verified on chain"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(db.get_refund(b_id).unwrap().7, "executed");
}

/// F5: operator-forced `failed` is Owner-only, needs a non-empty reason,
/// persists audit `invoice_manual_failed`, and refuses paid/paid_ungranted.
#[tokio::test]
async fn invoice_manual_failed_owner_reason_audit_and_refuses_paid() {
    let (state, _dir) = make_seller_state().await;
    let seller = seller_token(&state);
    let owner = owner_token(&state);
    let db = state.db.clone();
    let app = router(state);

    // Fresh (unpaid) invoice.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/invoices")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {seller}"))
                .body(Body::from(
                    r#"{"amount":"5","asset":"USDT","description":"t"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let inv = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["invoice_id"]
        .as_str()
        .unwrap()
        .to_string();

    let post_status = |token: &str, body: &'static str| {
        let app = app.clone();
        let inv = inv.clone();
        let token = token.to_string();
        async move {
            app.oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/v1/invoices/{inv}/status"))
                    .header("content-type", "application/json")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap()
        }
    };

    // Non-owner is refused.
    let resp = post_status(&seller, r#"{"status":"failed"}"#).await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["error"]["code"],
        "owner_required"
    );

    // Owner without a usable reason is refused (missing + whitespace).
    for body in [
        r#"{"status":"failed"}"#,
        r#"{"status":"failed","reason":"   "}"#,
    ] {
        let resp = post_status(&owner, body).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["error"]["code"],
            "reason_required"
        );
    }

    // System-only state cannot be set manually.
    let resp = post_status(&owner, r#"{"status":"paid_ungranted"}"#).await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["error"]["code"],
        "invalid_status"
    );

    // Valid forced failure: 200, persisted, audited with the reason.
    let resp = post_status(
        &owner,
        r#"{"status":"failed","reason":"definitive non-payment proof"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(db.get_invoice(&inv).unwrap().2, "failed");
    let audited = db.list_audit_log(100).into_iter().any(|row| {
        row.3 == "invoice_manual_failed"
            && row.5.as_deref() == Some(inv.as_str())
            && row
                .6
                .as_deref()
                .is_some_and(|m| m.contains("definitive non-payment proof"))
    });
    assert!(audited, "missing invoice_manual_failed audit row");

    // A paid invoice must never be forced to failed.
    let paid_inv = seed_paid_invoice(&app, &db, &seller, "5").await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/v1/invoices/{paid_inv}/status"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {owner}"))
                .body(Body::from(
                    r#"{"status":"failed","reason":"late reversal attempt"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(db.get_invoice(&paid_inv).unwrap().2, "paid");
}

// ---- REST /claim: provider verification and seller-bot side effects -------

/// Point the API's session at a mock Telegram bot that answers invite-link
/// and DM calls locally, so the claim happy path exercises the real Bot
/// transport code without reaching the network.
async fn attach_claim_bot(state: &mut WebState) -> wiremock::MockServer {
    use wiremock::matchers::{method, path_regex};
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(method("POST"))
        .and(path_regex(r".*/CreateChatInviteLink"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {
                    "invite_link": "https://t.me/+claim-invite",
                    "creator": {"id": 42, "is_bot": true, "first_name": "Seller"},
                    "creates_join_request": false,
                    "is_primary": false,
                    "is_revoked": false
                }
            })),
        )
        .mount(&server)
        .await;
    wiremock::Mock::given(method("POST"))
        .and(path_regex(r".*/SendMessage"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {
                    "message_id": 1,
                    "date": 1_700_000_000,
                    "chat": {"id": 42, "type": "private", "first_name": "T"},
                    "text": "x"
                }
            })),
        )
        .mount(&server)
        .await;
    let bot = teloxide::Bot::new("42:placeholder")
        .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());
    let ctx = Arc::new(crate::bots::BotContext {
        id: 9,
        key: "mock".to_string(),
        bot,
        owner_id: 0,
        shared: Arc::new(state.cfg.clone()),
        db: state.db.clone(),
    });
    state.bots = Arc::new(crate::bots::BotManager {
        bots: vec![ctx.clone()],
        primary: ctx,
        db: state.db.clone(),
        identity: crate::identity::IdentityHandle::unconnected(),
    });
    server
}

async fn claim_request(
    app: axum::Router,
    token: &str,
    invoice_id: &str,
) -> axum::response::Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!("/api/v1/invoices/{invoice_id}/claim"))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn claim_provider(server: &wiremock::MockServer, invoice_id: &str, status: &str) {
    use wiremock::matchers::{body_partial_json, method, path};
    wiremock::Mock::given(method("POST"))
        .and(path("/getInvoices"))
        .and(body_partial_json(
            serde_json::json!({"invoice_ids": invoice_id}),
        ))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"items": [{"invoice_id": invoice_id, "status": status}]}
            })),
        )
        .expect(1)
        .mount(server)
        .await;
}

/// A seller can claim their own paid invoice: the real claim handler calls
/// the mocked provider once, grants exactly one subscription, and drives the
/// seller's mock bot for the invite link and DM.
#[tokio::test]
async fn claim_api_valid_seller_grants_once_with_provider_and_bot_side_effects() {
    use wiremock::MockServer;
    let provider = MockServer::start().await;
    claim_provider(&provider, "inv-claim", "paid").await;

    let (mut state, _dir) = make_seller_state().await;
    state.cfg.crypto_token = Some("claim-token".to_string());
    state.cfg.crypto_api_base = Some(provider.uri());
    let db = state.db.clone();
    db.add_plan(42, "plan", "5", "USDT", 7, Some("@chan42"));
    db.save_invoice(
        "inv-claim",
        42,
        "5",
        "USDT",
        Some("plan"),
        "active",
        None,
        Some("plan"),
        Some(42),
        None,
    );
    let telegram = attach_claim_bot(&mut state).await;
    let token = seller_token(&state);
    let resp = claim_request(router(state), &token, "inv-claim").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["granted"], true);
    assert_eq!(v["invoice_id"], "inv-claim");

    assert_eq!(db.get_invoice("inv-claim").unwrap().2, "paid");
    assert_eq!(db.subscriptions_for_invoice("inv-claim").len(), 1);
    let paths: Vec<String> = provider
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(paths.len(), 1, "exactly one provider call: {paths:?}");
    let telegram_paths: Vec<String> = telegram
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert!(
        telegram_paths
            .iter()
            .any(|p| p.ends_with("/CreateChatInviteLink")),
        "{telegram_paths:?}"
    );
    assert!(
        telegram_paths.iter().any(|p| p.ends_with("/SendMessage")),
        "{telegram_paths:?}"
    );
}

/// A session scoped to seller A cannot claim seller B's invoice, and the
/// provider is never contacted.
#[tokio::test]
async fn claim_api_foreign_seller_is_forbidden_without_provider_call() {
    use wiremock::MockServer;
    let provider = MockServer::start().await;
    let (mut state, _dir) = make_seller_state().await;
    state.cfg.crypto_token = Some("claim-token".to_string());
    state.cfg.crypto_api_base = Some(provider.uri());
    let db = state.db.clone();
    db.save_invoice(
        "inv-foreign",
        99,
        "5",
        "USDT",
        None,
        "active",
        None,
        None,
        Some(42),
        None,
    );
    let token = seller_token(&state);
    let resp = claim_request(router(state), &token, "inv-foreign").await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(db.get_invoice("inv-foreign").unwrap().2, "active");
    assert!(provider.received_requests().await.unwrap().is_empty());
}

/// Without a crypto token the claim is released back to active and answered
/// 503; nothing is granted.
#[tokio::test]
async fn claim_api_no_token_returns_503_and_releases_claim() {
    let (state, _dir) = make_seller_state().await;
    let db = state.db.clone();
    db.save_invoice(
        "inv-notoken",
        42,
        "5",
        "USDT",
        None,
        "active",
        None,
        None,
        Some(42),
        None,
    );
    let token = seller_token(&state);
    let resp = claim_request(router(state), &token, "inv-notoken").await;
    assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(db.get_invoice("inv-notoken").unwrap().2, "active");
    assert!(db.subscriptions_for_invoice("inv-notoken").is_empty());
}

/// A provider API error releases the claim and answers 502 (never grant on
/// doubt).
#[tokio::test]
async fn claim_api_provider_error_returns_502_and_releases_claim() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let provider = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/getInvoices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": false,
            "error": "provider down"
        })))
        .expect(1)
        .mount(&provider)
        .await;
    let (mut state, _dir) = make_seller_state().await;
    state.cfg.crypto_token = Some("claim-token".to_string());
    state.cfg.crypto_api_base = Some(provider.uri());
    let db = state.db.clone();
    db.save_invoice(
        "inv-provider-err",
        42,
        "5",
        "USDT",
        None,
        "active",
        None,
        None,
        Some(42),
        None,
    );
    let token = seller_token(&state);
    let resp = claim_request(router(state), &token, "inv-provider-err").await;
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(db.get_invoice("inv-provider-err").unwrap().2, "active");
    assert!(db.subscriptions_for_invoice("inv-provider-err").is_empty());
    assert_eq!(provider.received_requests().await.unwrap().len(), 1);
}

/// A provider answer that is not `paid` releases the claim and answers 400.
#[tokio::test]
async fn claim_api_not_paid_returns_400_and_releases_claim() {
    use wiremock::MockServer;
    let provider = MockServer::start().await;
    claim_provider(&provider, "inv-unpaid", "pending").await;
    let (mut state, _dir) = make_seller_state().await;
    state.cfg.crypto_token = Some("claim-token".to_string());
    state.cfg.crypto_api_base = Some(provider.uri());
    let db = state.db.clone();
    db.save_invoice(
        "inv-unpaid",
        42,
        "5",
        "USDT",
        None,
        "active",
        None,
        None,
        Some(42),
        None,
    );
    let token = seller_token(&state);
    let resp = claim_request(router(state), &token, "inv-unpaid").await;
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert_eq!(db.get_invoice("inv-unpaid").unwrap().2, "active");
    assert!(db.subscriptions_for_invoice("inv-unpaid").is_empty());
}

/// A concurrent claim loses the atomic active-to-granting race and gets 409
/// before any provider work.
#[tokio::test]
async fn claim_api_concurrent_claim_returns_409() {
    use wiremock::MockServer;
    let provider = MockServer::start().await;
    let (mut state, _dir) = make_seller_state().await;
    state.cfg.crypto_token = Some("claim-token".to_string());
    state.cfg.crypto_api_base = Some(provider.uri());
    let db = state.db.clone();
    db.save_invoice(
        "inv-raced",
        42,
        "5",
        "USDT",
        None,
        "active",
        None,
        None,
        Some(42),
        None,
    );
    assert!(db.claim_invoice_for_grant("inv-raced"));
    let token = seller_token(&state);
    let resp = claim_request(router(state), &token, "inv-raced").await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    assert_eq!(db.get_invoice("inv-raced").unwrap().2, "granting");
    assert!(provider.received_requests().await.unwrap().is_empty());
}

/// The claim handler resolves the Telegram sender with BotManager::for_seller:
/// seller B's invoice reaches B's bot only; the primary/A mock servers stay
/// untouched.
#[tokio::test]
async fn claim_api_seller_b_uses_only_bots_b() {
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let provider = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/getInvoices"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "result": {"items": [{"invoice_id": "inv-b", "status": "paid"}]}
        })))
        .expect(1)
        .mount(&provider)
        .await;

    let (mut state, _dir) = make_state().await;
    state.cfg.crypto_token = Some("claim-token".to_string());
    state.cfg.crypto_api_base = Some(provider.uri());
    let db = state.db.clone();
    db.create_web_account("sellerb", "secret", "seller", Some(99))
        .await
        .unwrap();
    db.upsert_seller(99, None, None);
    db.add_plan(99, "plan-b", "5", "USDT", 7, Some("@chan99"));
    db.save_invoice(
        "inv-b",
        99,
        "5",
        "USDT",
        Some("plan-b"),
        "active",
        None,
        Some("plan-b"),
        Some(99),
        None,
    );

    let telegram_a = MockServer::start().await;
    let telegram_b = MockServer::start().await;
    for server in [&telegram_a, &telegram_b] {
        Mock::given(method("POST"))
            .and(path_regex(r".*/CreateChatInviteLink"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {
                    "invite_link": "https://t.me/+claim-b",
                    "creator": {"id": 99, "is_bot": true, "first_name": "B"},
                    "creates_join_request": false,
                    "is_primary": false,
                    "is_revoked": false
                }
            })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/SendMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {
                    "message_id": 1,
                    "date": 1_700_000_000,
                    "chat": {"id": 99, "type": "private", "first_name": "T"},
                    "text": "x"
                }
            })))
            .mount(server)
            .await;
    }
    let shared = Arc::new(state.cfg.clone());
    let ctx_a = Arc::new(crate::bots::BotContext {
        id: 1,
        key: "a".to_string(),
        bot: teloxide::Bot::new("1:placeholder")
            .set_api_url(telegram_a.uri().parse::<reqwest::Url>().unwrap()),
        owner_id: 0,
        shared: shared.clone(),
        db: db.clone(),
    });
    let ctx_b = Arc::new(crate::bots::BotContext {
        id: 9,
        key: "b".to_string(),
        bot: teloxide::Bot::new("9:placeholder")
            .set_api_url(telegram_b.uri().parse::<reqwest::Url>().unwrap()),
        owner_id: 0,
        shared,
        db: db.clone(),
    });
    state.bots = Arc::new(crate::bots::BotManager {
        bots: vec![ctx_a.clone(), ctx_b.clone()],
        primary: ctx_a.clone(),
        db: db.clone(),
        identity: crate::identity::IdentityHandle::unconnected(),
    });
    db.upsert_seller_bot_id(99, Some(9));
    assert_eq!(state.bots.for_seller(99).id, 9);
    let (account_id, _, _, _, _) = db.get_web_account_by_username("sellerb").unwrap();
    let token = db.create_web_session(account_id, "seller", Some(99), 7);

    let resp = claim_request(router(state), &token, "inv-b").await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(db.get_invoice("inv-b").unwrap().2, "paid");
    assert_eq!(db.subscriptions_for_invoice("inv-b").len(), 1);
    assert!(telegram_a.received_requests().await.unwrap().is_empty());
    assert_eq!(telegram_b.received_requests().await.unwrap().len(), 2);
}
