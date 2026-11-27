//! Local adversarial integration tests driven through the merged production
//! router on loopback only. Each case sends a request a hostile caller could
//! send and checks the victim tenant's row afterwards. The database lives in
//! a TempDir and the MCP token is a placeholder.
//!
//! This is self-validation, not an independent penetration test. It makes no
//! claim about a deployed system.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode};
use daybook::config::Config;
use daybook::db::Db;
use daybook::{api, mcp, web, web::WebState};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const A: i64 = 91_001;
const B: i64 = 91_002;
const OWNER_TOKEN: &str = "local-only-owner-token-not-a-secret";
const B_MARK: &str = "advBetaMarkerK6T4";
const A_CHANNEL: &str = "@adv_alpha_channel";
const B_CHANNEL: &str = "@adv_beta_channel";
const B_PRODUCT: &str = "adv_beta_product";
const B_INVOICE: &str = "adv_inv_beta";

struct Fixture {
    _dir: tempfile::TempDir,
    db: Arc<Db>,
    state: WebState,
    app: Router,
    buyer_session: String,
    a_session: String,
    a_mcp_token: String,
}

#[rustfmt::skip]
async fn bot_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "ok": true,
        "result": {"message_id":1,"date":1_700_000_000,
                   "chat":{"id":42,"type":"private","first_name":"T"},"text":"sent"}
    }))).mount(&server).await;
    server
}

#[rustfmt::skip]
async fn make_fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("adversarial_local.db");
    let db = Arc::new(Db::new(db_path.to_str().unwrap()).unwrap());
    for (name, role, sid) in [("seller_alpha", "seller", Some(A)), ("buyer", "user", None)] {
        db.create_web_account(name, "local-pw", role, sid).await.unwrap();
    }
    db.set_config("setup_done", "1");
    db.set_config("channel", "@adv_global_channel");
    db.upsert_seller(A, Some("seller_alpha"), Some(A_CHANNEL));
    let cfg = Config {
        owner_id: 1, db_path: db_path.to_str().unwrap().to_string(),
        crypto_token: None, crypto_api_base: None, ai_enabled: false,
        mfa_recovery_codes_enabled: true,
        ai_provider: "openai".into(), ai_api_style: "openai".into(),
        ai_base_url: "https://api.openai.com/v1".into(), ai_api_key: None,
        ai_model: "gpt-4o-mini".into(), ai_daily_token_budget: None,
        mcp_owner_token: Some(OWNER_TOKEN.to_string()),
        telegram_api_id: None, telegram_api_hash: None,
        bots: Vec::new(), bots_legacy: true,
    };
    let server = bot_server().await;
    let shared = Arc::new(cfg.clone());
    let bot = teloxide::Bot::new("0:local-placeholder")
        .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());
    let primary = Arc::new(daybook::bots::BotContext {
        id: 0, key: "primary".into(), bot, owner_id: 0, shared, db: db.clone(),
    });
    let bots = Arc::new(daybook::bots::BotManager {
        bots: vec![primary.clone()], primary, db: db.clone(),
        identity: daybook::identity::IdentityHandle::unconnected(),
    });
    let state = WebState {
        db: db.clone(), cfg, bot: teloxide::Bot::new("0:local-placeholder"), bots,
        identity: daybook::identity::IdentityHandle::unconnected(),
        login_attempts: Arc::new(Mutex::new(HashMap::new())),
        ai_test_inflight: Arc::new(tokio::sync::Mutex::new(())), base_path: String::new(),
    };
    let app = web::mount("", web::router(state.clone())
        .merge(api::router(state.clone())).merge(mcp::router(state.clone())));
    let (buyer_acc, ..) = db.get_web_account_by_username("buyer").unwrap();
    let (alpha_acc, ..) = db.get_web_account_by_username("seller_alpha").unwrap();
    let (_, a_mcp_token) = db.create_mcp_token(A, Some("adv-a"));
    Fixture {
        _dir: dir, db: db.clone(), state, app,
        buyer_session: db.create_web_session(buyer_acc, "user", None, 7),
        a_session: db.create_web_session(alpha_acc, "seller", Some(A), 7),
        a_mcp_token,
    }
}

struct Seed {
    b_product_id: i64,
    b_review: i64,
    b_ticket: i64,
    b_post: i64,
    a_plan: i64,
}

#[rustfmt::skip]
fn seed(db: &Db) -> Seed {
    db.add_plan(A, "adv alpha plan", "10", "USDT", 30, Some(A_CHANNEL));
    db.add_plan(B, "adv beta plan", "20", "USDT", 30, Some(B_CHANNEL));
    let a_plan = db.get_plan(A, "adv alpha plan").unwrap().0;
    db.save_invoice(B_INVOICE, B, "20", "USDT", Some(B_MARK), "active", None, None, None, None);
    let b_product_id = db.add_product(
        B, B_PRODUCT, "20", "USDT", "digital_link", Some("beta-delivery"), Some(9)).unwrap();
    let b_review = db.create_review(5_002, B, b_product_id, 5, Some(B_MARK)).unwrap();
    let b_ticket = db.create_ticket(B, 5_002, B_MARK);
    db.log_post(B, B_CHANNEL, 22, "text", Some(B_MARK), "bot").unwrap();
    let b_post = db.list_posts(Some(B), 10)[0].0;
    Seed { b_product_id, b_review, b_ticket, b_post, a_plan }
}

#[rustfmt::skip]
fn victim_snapshot(db: &Db) -> String {
    format!("{:?}", (db.list_plans(Some(B)), db.list_codes(Some(B), 100),
        db.list_invoices(Some(B), 100), db.get_product(B, B_PRODUCT)))
}

async fn body_text(resp: axum::http::Response<Body>) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).to_string()
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    (status, body_text(resp).await)
}

#[rustfmt::skip]
async fn rest(app: &Router, tok: &str, m: &str, p: &str, body: Value) -> (StatusCode, String) {
    let payload = if body.is_null() { String::new() } else { body.to_string() };
    let auth = format!("Bearer {tok}");
    let req = Request::builder().method(m).uri(p).header("authorization", auth)
        .header("content-type", "application/json").body(Body::from(payload)).unwrap();
    send(app, req).await
}

#[rustfmt::skip]
async fn console(app: &Router, sess: &str, m: &str, p: &str, form: &str) -> (StatusCode, String) {
    let t = daybook::auth::csrf_token_for_session(sess);
    let payload = format!("{form}&csrf_token={t}");
    let ct = if m == "POST" { "application/x-www-form-urlencoded" } else { "application/json" };
    let cookie = format!("session={sess}");
    let req = Request::builder().method(m).uri(p).header("cookie", cookie)
        .header("content-type", ct).body(Body::from(payload)).unwrap();
    send(app, req).await
}

async fn mcp_call(app: &Router, tok: &str, tool: &str, args: Value) -> Value {
    let rpc = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
    let (code, text) = rest(app, tok, "POST", "/api/v1/mcp", rpc).await;
    assert_eq!(code, StatusCode::OK, "tool {tool}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

fn is_denied(code: StatusCode) -> bool {
    code == StatusCode::FORBIDDEN || code == StatusCode::NOT_FOUND
}

#[tokio::test]
#[rustfmt::skip]
async fn buyer_bearer_cannot_forge_invoice_status_executed() {
    let f = make_fixture().await;
    let _s = seed(&f.db);
    let before = f.db.get_invoice(B_INVOICE);
    let p = format!("/api/v1/invoices/{B_INVOICE}/status");
    // "executed" is not a legal transition; "expired" is. Scope must refuse
    // both before the status vocabulary is consulted.
    for status in ["executed", "expired"] {
        let (code, text) = rest(&f.app, &f.buyer_session, "POST", &p, json!({"status":status})).await;
        assert!(is_denied(code), "buyer invoice {status}: {code} {text}");
        assert_eq!(f.db.get_invoice(B_INVOICE), before, "B invoice mutated: {status}");
    }
}

#[tokio::test]
#[rustfmt::skip]
async fn buyer_cannot_approve_foreign_review_or_ticket() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let approved = || f.db.list_reviews(B, s.b_product_id, true).iter().any(|r| r.0 == s.b_review);
    assert!(!approved(), "fixture review already approved");
    let ticket_before = f.db.get_ticket(s.b_ticket).unwrap();
    let (code, body) = console(&f.app, &f.buyer_session, "POST", "/reviews/approve",
        &format!("id={}", s.b_review)).await;
    assert!(is_denied(code), "buyer approved B review: {code} {body}");
    assert!(!approved(), "B review approved by buyer");
    let (code, body) = console(&f.app, &f.buyer_session, "POST", "/tickets/status",
        &format!("ticket_id={}&status=resolved", s.b_ticket)).await;
    assert!(is_denied(code), "buyer changed B ticket: {code} {body}");
    assert_eq!(f.db.get_ticket(s.b_ticket), Some(ticket_before));
}

#[tokio::test]
#[rustfmt::skip]
async fn seller_cannot_delete_foreign_post_via_mcp_direct() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let principal = daybook::mcp::tools::Principal {
        role: daybook::roles::Role::Seller, seller_id: Some(A),
    };
    let err = daybook::mcp::tools::execute_sensitive(
        f.state.clone(), &principal, "delete_post", json!({"post_id":s.b_post}))
        .await.expect_err("seller A deleted B's post directly");
    assert_eq!(err, "post not found or not yours");
    assert_eq!(f.db.get_post_seller(s.b_post), Some(B), "B post deleted by A");
}

#[tokio::test]
#[rustfmt::skip]
async fn forged_seller_id_in_body_does_not_widen() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let before = victim_snapshot(&f.db);
    let codes_before: Vec<String> = f.db.list_codes(Some(A), 100).into_iter().map(|c| c.0).collect();
    let invs_before: Vec<String> = f.db.list_invoices(Some(A), 100).into_iter().map(|i| i.0).collect();
    let body = json!({"name":"adv forged plan","price":"1","asset":"USDT","duration_days":3,"channel_id":A_CHANNEL,"seller_id":B});
    let (code, text) = rest(&f.app, &f.a_session, "POST", "/api/v1/plans", body).await;
    assert!(code.is_success(), "plan create: {code} {text}");
    assert_eq!(f.db.get_plan_global("adv forged plan").unwrap().0, A, "plan under B");
    let body = json!({"plan_id":s.a_plan,"value_days":3,"seller_id":B});
    let (code, text) = rest(&f.app, &f.a_session, "POST", "/api/v1/codes", body).await;
    assert!(code.is_success(), "code create: {code} {text}");
    let row = f.db.list_codes(Some(A), 100).into_iter()
        .find(|c| !codes_before.contains(&c.0)).expect("no forged code in A scope");
    assert_eq!(row.5, A, "code under B");
    let body = json!({"amount":"1","asset":"USDT","user_id":5_002,"seller_id":B,"description":"adv forged invoice"});
    let (code, text) = rest(&f.app, &f.a_session, "POST", "/api/v1/invoices", body).await;
    assert!(code.is_success(), "invoice create: {code} {text}");
    let inv = f.db.list_invoices(Some(A), 100).into_iter()
        .find(|i| !invs_before.contains(&i.0)).expect("no forged invoice in A scope");
    assert_eq!(f.db.get_invoice(&inv.0).unwrap().6, A, "invoice under B");
    assert_eq!(victim_snapshot(&f.db), before, "tenant B rows changed");
}

#[tokio::test]
#[rustfmt::skip]
async fn tenantless_session_cannot_read_console_global_pages() {
    let f = make_fixture().await;
    let _s = seed(&f.db);
    for p in ["/dashboard", "/plans", "/subs", "/customers"] {
        let (code, body) = console(&f.app, &f.buyer_session, "GET", p, "").await;
        assert!(is_denied(code), "buyer read {p}: {code}");
        assert!(!body.contains(B_MARK) && !body.contains(B_CHANNEL), "{p} leaked tenant B");
    }
}

#[tokio::test]
#[rustfmt::skip]
async fn unbound_seller_session_cannot_authenticate() {
    let f = make_fixture().await;
    let (account_id, ..) = f.db.get_web_account_by_username("seller_alpha").unwrap();
    // create_web_account refuses seller+NULL, but the session row is what the
    // loader reads. Forge that legacy shape through the public mint API.
    let scopeless = f.db.create_web_session(account_id, "seller", None, 7);
    let mut headers = HeaderMap::new();
    headers.insert(axum::http::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {scopeless}")).unwrap());
    assert!(daybook::auth::session_from_headers(&f.state, &headers).is_none(), "scopeless bearer resolved");
    let mut headers = HeaderMap::new();
    headers.insert(axum::http::header::COOKIE,
        HeaderValue::from_str(&format!("session={scopeless}")).unwrap());
    assert!(daybook::auth::session_from_headers(&f.state, &headers).is_none(), "scopeless cookie resolved");
    let (code, _) = rest(&f.app, &scopeless, "GET", "/api/v1/plans", Value::Null).await;
    assert_eq!(code, StatusCode::UNAUTHORIZED, "scopeless session reached REST");
}

#[tokio::test]
#[rustfmt::skip]
async fn mcp_seller_token_cannot_park_sensitive_delete_post() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let before = f.db.mcp_list_approvals(None, 100).len();
    let v = mcp_call(&f.app, &f.a_mcp_token, "delete_post",
        json!({"post_id":s.b_post,"seller_id":B})).await;
    assert_eq!(v["error"]["code"].as_i64().unwrap_or(0), -32001, "seller parked: {v}");
    assert_eq!(f.db.mcp_list_approvals(None, 100).len(), before, "seller parked approval");
    assert_eq!(f.db.get_post_seller(s.b_post), Some(B), "B post deleted via MCP");
    assert!(f.db.mcp_list_audit(100).iter().any(|a| a.2 == "seller" && !a.3), "no failed seller audit");
}

/// A denied scope must never be spellable as a target id. `-1` is the sentinel
/// for "no tenant", so matching it as a seller id would let a buyer own a
/// phantom namespace.
#[tokio::test]
async fn denied_sentinel_cannot_be_used_as_a_seller_id() {
    let f = make_fixture().await;
    assert!(
        f.db.get_seller(-1).is_none(),
        "no phantom seller to begin with"
    );

    let (status, body) = rest(
        &f.app,
        &f.buyer_session,
        "POST",
        "/api/v1/sellers/-1/ai-settings",
        json!({"daily_token_budget": 999999999, "rpm_limit": 999999}),
    )
    .await;
    assert!(
        is_denied(status),
        "buyer must not write seller -1: {status} {body}"
    );
    assert!(
        f.db.get_seller(-1).is_none(),
        "phantom seller -1 was created"
    );
}
