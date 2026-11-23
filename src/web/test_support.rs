use super::*;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use std::sync::Arc;
use tempfile::TempDir;

#[tracing::instrument(skip_all)]
pub async fn make_state() -> (WebState, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let db = Db::new(db_path.to_str().unwrap()).unwrap();
    db.create_web_account("owner", "secret", "owner", None)
        .await
        .unwrap();
    let cfg = Config {
        owner_id: 1,
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
        login_attempts: Arc::new(StdMutex::new(HashMap::new())),
        ai_test_inflight: Arc::new(tokio::sync::Mutex::new(())),
        base_path: String::new(),
    };
    (state, dir)
}

#[tracing::instrument(skip_all)]
pub async fn make_state_with_recovery_codes(enabled: bool) -> (WebState, TempDir) {
    let (mut state, dir) = make_state().await;
    state.cfg.mfa_recovery_codes_enabled = enabled;
    (state, dir)
}

pub fn pending_post_req(uri: &str, body: &str, raw: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", format!("mfa_pending={raw}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub fn pending_get_req(uri: &str, raw: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("cookie", format!("mfa_pending={raw}"))
        .body(Body::empty())
        .unwrap()
}

#[tracing::instrument(skip_all)]
pub async fn body_string(resp: axum::http::Response<Body>) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).to_string()
}

/// Serializes tests that touch the process environment (ENV_FILE): env
/// is global and cargo runs tests in parallel threads, so env-mutating
/// tests take this lock — held across the awaited request, which is why
/// it is async-aware (a std guard would poison the runtime's threads).
pub static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Point the shared env writer at a test-owned file. SAFETY: ENV_FILE is
/// written only while ENV_LOCK is held and no other code in the test
/// binary reads it concurrently (single-writer mutation).
pub fn set_env_file_var(path: &std::path::Path) {
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::set_var("ENV_FILE", path) };
}

pub fn clear_env_file_var() {
    // SAFETY: serialized by ENV_LOCK.
    unsafe { std::env::remove_var("ENV_FILE") };
}

pub fn get_req(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

pub fn post_req(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from(body.to_string()))
        .unwrap()
}

pub fn authed_req(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("cookie", format!("session={}", token))
        .body(Body::empty())
        .unwrap()
}

pub fn post_authed_req(uri: &str, body: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", format!("session={}", token))
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tracing::instrument(skip_all)]
pub async fn owner_session(state: &WebState) -> String {
    let (acc_id, _u, _h, _r, _s) = state
        .db
        .get_web_account_by_username("owner")
        .expect("owner account exists");
    state.db.create_web_session(acc_id, "owner", None, 7)
}

#[tracing::instrument(skip_all)]
pub async fn seller_session(state: &WebState, seller_id: i64) -> String {
    seller_session_named(state, seller_id, "seller").await
}

/// Like `seller_session`, but with an explicit username. `web_accounts.username`
/// is UNIQUE, so cross-tenant tests that need two sellers in the same state
/// must give each account a distinct name.
#[tracing::instrument(skip_all)]
pub async fn seller_session_named(state: &WebState, seller_id: i64, username: &str) -> String {
    state
        .db
        .create_web_account(username, "secret", "seller", Some(seller_id))
        .await
        .unwrap();
    let (acc_id, _u, _h, _r, _s) = state
        .db
        .get_web_account_by_username(username)
        .expect("seller account exists");
    state
        .db
        .create_web_session(acc_id, "seller", Some(seller_id), 7)
}
