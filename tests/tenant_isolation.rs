//! Automated multi-tenant isolation validation on a fresh local database.
//!
//! Two seller principals (A and B) share one SQLite file, one merged router
//! (web + REST + MCP, mounted exactly as production mounts them) and three
//! loopback Bot API mocks: A's mapped bot, B's mapped bot and the primary bot
//! used for unmapped sellers. Every marker is unique per tenant, so an HTML or
//! JSON absence assertion cannot pass by accident. Nothing here contacts the
//! network beyond loopback, uses real credentials or touches a shared DB.
//!
//! This is automated self-validation of seller isolation. It is not an
//! independent multi-tenant penetration test and not a third-party audit.
//! The scratch-deployment verification in issue #1 remains open.
//!
//! Case list (surface | case | expected):
//!
//! | # | Surface | Case | Expected |
//! |---|---------|------|----------|
//! | 1 | REST read | list plans/subs/invoices/codes/posts/products/users/channels | own marker only |
//! | 2 | REST read | dashboard/revenue/finance/AI summaries | own counts and amounts only |
//! | 3 | REST read | single seller/plans/subs/invoice/code/post by foreign id | 403 or 404 |
//! | 4 | REST read | admin and owner-only listings as a seller | 403 |
//! | 5 | REST write | forged seller_id/user_id/bot_id fields | caller owns the new row |
//! | 6 | REST write | foreign plan/sub/invoice/code/post/product/codes reclaim | 403/404, victim row byte-identical |
//! | 7 | REST write | foreign channel post and metadata patch | 403, victim bot gets no request |
//! | 8 | REST write | seller writes global channel config or admin data | 403, config unchanged |
//! | 9 | MCP read | per-seller and owner token read tools | own marker only; owner sees both |
//! | 10 | MCP write | publish_post with bot_id/bot_key/seller_id spoofs | only the caller's bot is contacted |
//! | 11 | MCP write | customer_note and booking status with foreign ids | victim row byte-identical |
//! | 12 | MCP gate | Sensitive and Identity tools with both seller tokens | -32001 before validation, no approvals, failure audit only |
//! | 13 | MCP control | owner token calls the same tools | controls execute or park, so scoping is not a blanket deny |
//! | 14 | console | seller GET pages | own marker only |
//! | 15 | console | seller cross-tenant POSTs | no change to victim rows or bot traffic |
//! | 16 | bot | for_seller/for_channel/unmapped fallback and effective_bot | seller and channel ownership decide the bot |
//! | 17 | REST write | expire-check with a seller token | only that seller's expired rows; owner control still sweeps globally |
//! | 18 | REST/console | every mutating route swept with no session and with a B-targeted body | session required, owner-tier 403/404, victim snapshot byte-identical, no B bot traffic |
//!
//! To add a case: extend the table above, then add a row to the matching
//! surface table inside the test function below.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
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
const UNMAPPED: i64 = 91_999;
const OWNER_TOKEN: &str = "local-only-owner-token-not-a-secret";
const A_MARK: &str = "isoAlphaMarkerQ7V2";
const B_MARK: &str = "isoBetaMarkerL4N8";
const A_CHANNEL: &str = "@iso_alpha_channel";
const B_CHANNEL: &str = "@iso_beta_channel";
const GLOBAL_CHANNEL: &str = "@iso_global_channel";

struct Tenant {
    session: String,
    mcp_token: String,
    bot_id: i64,
    bot_key: &'static str,
    server: MockServer,
}

struct Fixture {
    _dir: tempfile::TempDir,
    db: Arc<Db>,
    state: WebState,
    app: Router,
    owner_session: String,
    a: Tenant,
    b: Tenant,
    primary: MockServer,
}

/// Every Bot API call gets the same small, valid envelope. Teloxide callers
/// that expect a bool may report a decode error on control calls; this suite
/// asserts which transport received the request, not Telegram's reply shape.
async fn bot_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "date": 1_700_000_000,
                "chat": {"id": 42, "type": "private", "first_name": "T"},
                "text": "sent"
            }
        })))
        .mount(&server)
        .await;
    server
}

fn bot_context(
    id: i64,
    key: &str,
    server: &MockServer,
    db: &Arc<Db>,
    shared: &Arc<Config>,
) -> Arc<daybook::bots::BotContext> {
    let bot = teloxide::Bot::new(format!("{id}:local-placeholder"))
        .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());
    Arc::new(daybook::bots::BotContext {
        id,
        key: key.to_string(),
        bot,
        owner_id: id,
        shared: shared.clone(),
        db: db.clone(),
    })
}

async fn make_fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("tenant_isolation.db");
    let db = Arc::new(Db::new(db_path.to_str().unwrap()).unwrap());
    db.create_web_account("owner", "local-pw", "owner", None)
        .await
        .unwrap();
    db.create_web_account("seller_alpha", "local-pw", "seller", Some(A))
        .await
        .unwrap();
    db.create_web_account("seller_beta", "local-pw", "seller", Some(B))
        .await
        .unwrap();
    db.set_config("setup_done", "1");
    db.set_config("channel", GLOBAL_CHANNEL);

    let a_server = bot_server().await;
    let b_server = bot_server().await;
    let primary_server = bot_server().await;
    let bot_a = db.create_bot("iso_alpha_bot", A, None, "Alpha Bot");
    let bot_b = db.create_bot("iso_beta_bot", B, None, "Beta Bot");
    db.upsert_seller(A, Some("seller_alpha"), Some(A_CHANNEL));
    db.upsert_seller(B, Some("seller_beta"), Some(B_CHANNEL));
    db.upsert_seller_bot_id(A, Some(bot_a));
    db.upsert_seller_bot_id(B, Some(bot_b));
    db.add_channel(A_CHANNEL, A).unwrap();
    db.add_channel(B_CHANNEL, B).unwrap();

    let cfg = Config {
        owner_id: 1,
        db_path: db_path.to_str().unwrap().to_string(),
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
        mcp_owner_token: Some(OWNER_TOKEN.to_string()),
        telegram_api_id: None,
        telegram_api_hash: None,
        bots: Vec::new(),
        bots_legacy: true,
    };
    let shared = Arc::new(cfg.clone());
    let primary = bot_context(0, "primary", &primary_server, &db, &shared);
    let a_ctx = bot_context(bot_a, "iso_alpha_bot", &a_server, &db, &shared);
    let b_ctx = bot_context(bot_b, "iso_beta_bot", &b_server, &db, &shared);
    let bots = Arc::new(daybook::bots::BotManager {
        bots: vec![primary.clone(), a_ctx, b_ctx],
        primary,
        db: db.clone(),
        identity: daybook::identity::IdentityHandle::unconnected(),
    });
    let state = WebState {
        db: db.clone(),
        cfg,
        bot: teloxide::Bot::new("0:local-placeholder"),
        bots,
        identity: daybook::identity::IdentityHandle::unconnected(),
        login_attempts: Arc::new(Mutex::new(HashMap::new())),
        ai_test_inflight: Arc::new(tokio::sync::Mutex::new(())),
        base_path: String::new(),
    };
    let app = web::mount(
        "",
        web::router(state.clone())
            .merge(api::router(state.clone()))
            .merge(mcp::router(state.clone())),
    );
    let (alpha_acc, ..) = db.get_web_account_by_username("seller_alpha").unwrap();
    let (beta_acc, ..) = db.get_web_account_by_username("seller_beta").unwrap();
    let (owner_acc, ..) = db.get_web_account_by_username("owner").unwrap();
    let owner_rest = db.create_web_session(owner_acc, "owner", None, 7);
    let (_, mcp_a) = db.create_mcp_token(A, Some("iso-a"));
    let (_, mcp_b) = db.create_mcp_token(B, Some("iso-b"));
    Fixture {
        _dir: dir,
        db: db.clone(),
        state,
        app,
        owner_session: owner_rest,
        a: Tenant {
            session: db.create_web_session(alpha_acc, "seller", Some(A), 7),
            mcp_token: mcp_a,
            bot_id: bot_a,
            bot_key: "iso_alpha_bot",
            server: a_server,
        },
        b: Tenant {
            session: db.create_web_session(beta_acc, "seller", Some(B), 7),
            mcp_token: mcp_b,
            bot_id: bot_b,
            bot_key: "iso_beta_bot",
            server: b_server,
        },
        primary: primary_server,
    }
}

struct Seed {
    b_plan: i64,
    b_sub: i64,
    b_invoice: String,
    a_code: String,
    b_code: String,
    b_post: i64,
    a_product: &'static str,
    b_product: &'static str,
    a_payout: i64,
}

fn seed(db: &Db) -> Seed {
    let a_plan_name = format!("{A_MARK} plan");
    let b_plan_name = format!("{B_MARK} plan");
    db.add_plan(A, &a_plan_name, "10", "USDT", 30, Some(A_CHANNEL));
    db.add_plan(B, &b_plan_name, "20", "USDT", 30, Some(B_CHANNEL));
    let a_plan = db.get_plan(A, &a_plan_name).unwrap().0;
    let b_plan = db.get_plan(B, &b_plan_name).unwrap().0;
    let _a_sub = db.create_subscription(A, 5_001, Some(a_plan), A_CHANNEL, 30, None, None);
    let b_sub = db.create_subscription(B, 5_002, Some(b_plan), B_CHANNEL, 30, None, None);
    let a_invoice = format!("iso_inv_alpha_{A_MARK}");
    let b_invoice = format!("iso_inv_beta_{B_MARK}");
    db.save_invoice(
        &a_invoice,
        A,
        "10",
        "USDT",
        Some(A_MARK),
        "active",
        None,
        None,
        Some(5_001),
        None,
    );
    db.save_invoice(
        &b_invoice,
        B,
        "20",
        "USDT",
        Some(B_MARK),
        "active",
        None,
        None,
        Some(5_002),
        None,
    );
    let a_code = db.create_code(A, Some(a_plan), Some(30), 1, None, 12);
    let b_code = db.create_code(B, Some(b_plan), Some(30), 1, None, 12);
    db.log_post(A, A_CHANNEL, 11, "text", Some(A_MARK), "bot")
        .unwrap();
    db.log_post(B, B_CHANNEL, 22, "text", Some(B_MARK), "bot")
        .unwrap();
    let _a_post = db.list_posts(Some(A), 10)[0].0;
    let b_post = db.list_posts(Some(B), 10)[0].0;
    let a_product = "iso_alpha_product";
    let b_product = "iso_beta_product";
    db.add_product(
        A,
        a_product,
        "10",
        "USDT",
        "digital_link",
        Some("alpha-delivery"),
        Some(3),
    )
    .unwrap();
    db.add_product(
        B,
        b_product,
        "20",
        "USDT",
        "digital_link",
        Some("beta-delivery"),
        Some(4),
    )
    .unwrap();
    let a_payout = db.create_payout(A, "10", "USDT", Some(A_MARK));
    db.create_payout(B, "20", "USDT", Some(B_MARK));
    db.upsert_customer(A, 5_001);
    db.update_customer_notes(A, 5_001, A_MARK);
    db.upsert_customer(B, 5_002);
    db.update_customer_notes(B, 5_002, B_MARK);
    Seed {
        b_plan,
        b_sub,
        b_invoice,
        a_code,
        b_code,
        b_post,
        a_product,
        b_product,
        a_payout,
    }
}

/// Byte-identical snapshot of everything owned by tenant B. Cross-tenant
/// writes must leave this string unchanged.
fn victim_snapshot(db: &Db) -> String {
    format!(
        "{:?}",
        (
            db.get_seller(B),
            db.list_plans(Some(B)),
            db.list_subscriptions(Some(B), 1000),
            db.list_codes(Some(B), 100),
            db.list_posts(Some(B), 100),
            db.list_products(Some(B)),
            db.list_payouts(Some(B)),
            db.list_refunds(Some(B)),
            db.get_customer(B, 5_002),
        )
    )
}

async fn body_text(resp: axum::http::Response<Body>) -> String {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8_lossy(&bytes).to_string()
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

async fn rest(
    app: &Router,
    token: &str,
    method_name: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let mut builder = Request::builder()
        .method(method_name)
        .uri(path)
        .header("authorization", bearer(token));
    let payload = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            v.to_string()
        }
        None => String::new(),
    };
    let resp = app
        .clone()
        .oneshot(builder.body(Body::from(payload)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    (status, body_text(resp).await)
}

fn owner_token() -> String {
    OWNER_TOKEN.to_string()
}

async fn console(
    app: &Router,
    session: &str,
    method_name: &str,
    path: &str,
    form: &str,
) -> (StatusCode, String) {
    let token = daybook::auth::csrf_token_for_session(session);
    let payload = if form.is_empty() {
        format!("csrf_token={token}")
    } else {
        format!("{form}&csrf_token={token}")
    };
    let mut builder = Request::builder()
        .method(method_name)
        .uri(path)
        .header("cookie", format!("session={session}"));
    if method_name == "POST" {
        builder = builder.header("content-type", "application/x-www-form-urlencoded");
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::from(payload)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    (status, body_text(resp).await)
}

async fn mcp_call(app: &Router, token: &str, id: i64, tool: &str, args: Value) -> Value {
    let rpc = json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": tool, "arguments": args}
    });
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/mcp")
                .header("content-type", "application/json")
                .header("authorization", bearer(token))
                .body(Body::from(rpc.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let text = body_text(resp).await;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "tool {tool}: {text}");
    v
}

fn tool_text(v: &Value) -> Value {
    let text = v["result"]["content"][0]["text"].as_str().unwrap_or("");
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn rpc_code(v: &Value) -> i64 {
    v["error"]["code"].as_i64().unwrap_or(0)
}

async fn request_count(server: &MockServer) -> usize {
    server.received_requests().await.unwrap_or_default().len()
}

/// Same REST request as `rest`, but with an invalid session credential. A
/// bogus bearer keeps the request on the REST auth path (the CSRF middleware
/// lets bearer requests through), so a 401 proves the handler's own session
/// gate refuses the caller before any handler body or bot call runs.
async fn rest_without_session(
    app: &Router,
    method_name: &str,
    path: &str,
    body: Option<Value>,
) -> StatusCode {
    let mut builder = Request::builder()
        .method(method_name)
        .uri(path)
        .header("authorization", "Bearer not-a-valid-session");
    let payload = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            v.to_string()
        }
        None => String::new(),
    };
    let resp = app
        .clone()
        .oneshot(builder.body(Body::from(payload)).unwrap())
        .await
        .unwrap();
    resp.status()
}

/// Same console POST as `console`, but without a session cookie or CSRF token.
/// The router-level CSRF middleware refuses these before any handler runs.
async fn console_without_session(app: &Router, path: &str, form: &str) -> StatusCode {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(form.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
}

fn is_denied(status: StatusCode) -> bool {
    status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND
}

async fn assert_only_a_contacted(f: &Fixture) {
    assert_eq!(
        request_count(&f.primary).await,
        0,
        "primary bot received traffic"
    );
    assert_eq!(
        request_count(&f.b.server).await,
        0,
        "tenant B bot received traffic"
    );
}

#[tokio::test]
async fn rest_reads_expose_only_the_callers_rows() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    // surface | path | own marker | foreign marker
    let cases = [
        ("plans", "/api/v1/plans", A_MARK, B_MARK),
        (
            "subscriptions",
            "/api/v1/subscriptions",
            A_CHANNEL,
            B_CHANNEL,
        ),
        ("invoices", "/api/v1/invoices", A_MARK, B_MARK),
        (
            "codes",
            "/api/v1/codes",
            s.a_code.as_str(),
            s.b_code.as_str(),
        ),
        ("posts", "/api/v1/posts", A_MARK, B_MARK),
        ("products", "/api/v1/products", s.a_product, s.b_product),
        ("users", "/api/v1/users", "5001", "5002"),
        ("channels", "/api/v1/channels", A_CHANNEL, B_CHANNEL),
    ];
    for (label, path, own, foreign) in cases {
        let (status, body) = rest(&f.app, &f.a.session, "GET", path, None).await;
        assert_eq!(status, StatusCode::OK, "{label}");
        assert!(body.contains(own), "{label}: own marker missing: {body}");
        assert!(
            !body.contains(foreign),
            "{label}: foreign marker leaked: {body}"
        );
        let (status, body) = rest(&f.app, &f.b.session, "GET", path, None).await;
        assert_eq!(status, StatusCode::OK, "{label}");
        assert!(body.contains(foreign), "{label}: B marker missing: {body}");
        assert!(!body.contains(own), "{label}: A marker leaked: {body}");
    }
    // Aggregates: each seller's summary reflects only their own rows.
    for path in [
        "/api/v1/dashboard/summary",
        "/api/v1/revenue",
        "/api/v1/finance/summary",
        "/api/v1/finance/payouts",
        "/api/v1/ai/templates",
        "/api/v1/ai/usage",
        "/api/v1/users/5001/subscriptions",
    ] {
        let (status_a, body_a) = rest(&f.app, &f.a.session, "GET", path, None).await;
        let (status_b, body_b) = rest(&f.app, &f.b.session, "GET", path, None).await;
        assert_eq!(status_a, StatusCode::OK, "{path}");
        assert_eq!(status_b, StatusCode::OK, "{path}");
        assert!(
            !body_a.contains(B_MARK) && !body_a.contains(B_CHANNEL),
            "{path}: B leaked to A: {body_a}"
        );
        assert!(
            !body_b.contains(A_MARK) && !body_b.contains(A_CHANNEL),
            "{path}: A leaked to B: {body_b}"
        );
    }
    // One shared buyer id with a subscription under each seller: the count is
    // scoped, so A and B see different answers for the same URL.
    let (_, a_body) = rest(&f.app, &f.a.session, "GET", "/api/v1/users/5001", None).await;
    let (_, b_body) = rest(&f.app, &f.b.session, "GET", "/api/v1/users/5001", None).await;
    let a_count: Value = serde_json::from_str(&a_body).unwrap();
    let b_count: Value = serde_json::from_str(&b_body).unwrap();
    assert_eq!(a_count["subscription_count"], 1);
    assert_eq!(b_count["subscription_count"], 0);
    assert!(s.a_payout > 0);
}

#[tokio::test]
async fn rest_single_item_and_admin_reads_fail_closed_for_sellers() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    // case | path | seller result | owner result
    let cases = [
        (
            "seller row",
            format!("/api/v1/sellers/{}", B),
            StatusCode::FORBIDDEN,
            StatusCode::OK,
        ),
        (
            "plan",
            format!("/api/v1/plans/{}", s.b_plan),
            StatusCode::FORBIDDEN,
            StatusCode::OK,
        ),
        (
            "subscription",
            format!("/api/v1/subscriptions/{}", s.b_sub),
            StatusCode::NOT_FOUND,
            StatusCode::OK,
        ),
        (
            "invoice",
            format!("/api/v1/invoices/{}", s.b_invoice),
            StatusCode::FORBIDDEN,
            StatusCode::OK,
        ),
        (
            "code",
            format!("/api/v1/codes/{}", s.b_code),
            StatusCode::FORBIDDEN,
            StatusCode::OK,
        ),
        (
            "code redemptions",
            format!("/api/v1/codes/{}/redemptions", s.b_code),
            StatusCode::FORBIDDEN,
            StatusCode::OK,
        ),
        (
            "post",
            format!("/api/v1/posts/{}", s.b_post),
            StatusCode::NOT_FOUND,
            StatusCode::OK,
        ),
    ];
    for (label, path, seller_status, owner_status) in cases {
        let (got, body) = rest(&f.app, &f.a.session, "GET", &path, None).await;
        assert_eq!(got, seller_status, "{label}: {body}");
        assert!(
            !body.contains(B_MARK),
            "{label}: victim marker leaked: {body}"
        );
        let (got, body) = rest(&f.app, &f.owner_session, "GET", &path, None).await;
        assert_eq!(got, owner_status, "{label} owner control: {body}");
    }
    // Owner listings must span both tenants, proving the seller refusals above
    // are scoping rather than a broken endpoint.
    let (status, body) = rest(&f.app, &f.owner_session, "GET", "/api/v1/plans", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(A_MARK) && body.contains(B_MARK),
        "owner plans: {body}"
    );
    let (status, body) = rest(
        &f.app,
        &f.owner_session,
        "GET",
        "/api/v1/admin/sellers",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(A_CHANNEL) && body.contains(B_CHANNEL),
        "owner sellers: {body}"
    );
    let admin_cases = [
        "/api/v1/sellers",
        "/api/v1/admin/sellers",
        "/api/v1/admin/stats/global",
        "/api/v1/admin/admins",
        "/api/v1/admin/web-accounts",
        "/api/v1/admin/audit-log",
        "/api/v1/finance/revenue-by-seller",
        "/api/v1/finance/reconciliation",
    ];
    for path in admin_cases {
        let (status, _) = rest(&f.app, &f.a.session, "GET", path, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "seller reached {path}");
        let (status, _) = rest(&f.app, &f.owner_session, "GET", path, None).await;
        assert_eq!(status, StatusCode::OK, "owner control {path}");
    }
}

#[tokio::test]
async fn rest_writes_ignore_forged_owner_fields_and_preserve_victim_rows() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let before = victim_snapshot(&f.db);
    let invoice_before = f.db.get_invoice(&s.b_invoice);
    let code_before = f.db.get_code(&s.b_code);
    let post_before = f.db.list_posts(Some(B), 100)[0].clone();
    let product_before = f.db.get_product(B, s.b_product);
    let global_before = f.db.get_config("channel");
    // case | method | path | body | accepted status
    let cases = [
        (
            "seller upsert",
            "POST",
            "/api/v1/sellers".to_string(),
            json!({"user_id": B, "username": "forged-owner", "default_channel": "@forged"}),
            StatusCode::FORBIDDEN,
        ),
        (
            "plan patch",
            "PATCH",
            format!("/api/v1/plans/{}", s.b_plan),
            json!({"name": "forged-plan", "price": "999", "seller_id": A}),
            StatusCode::FORBIDDEN,
        ),
        (
            "plan delete",
            "DELETE",
            format!("/api/v1/plans/{}", s.b_plan),
            Value::Null,
            StatusCode::FORBIDDEN,
        ),
        (
            "plan active",
            "POST",
            format!("/api/v1/plans/{}/active", s.b_plan),
            json!({"active": false}),
            StatusCode::FORBIDDEN,
        ),
        (
            "subscription patch",
            "PATCH",
            format!("/api/v1/subscriptions/{}", s.b_sub),
            json!({"status": "expired", "seller_id": A}),
            StatusCode::FORBIDDEN,
        ),
        (
            "invoice status",
            "POST",
            format!("/api/v1/invoices/{}/status", s.b_invoice),
            json!({"status": "expired"}),
            StatusCode::FORBIDDEN,
        ),
        (
            "code reclaim",
            "POST",
            "/api/v1/codes/reclaim".to_string(),
            json!({"code": s.b_code}),
            StatusCode::NOT_FOUND,
        ),
        (
            "post patch",
            "PATCH",
            format!("/api/v1/posts/{}", s.b_post),
            json!({"text": "forged-post"}),
            StatusCode::NOT_FOUND,
        ),
        (
            "post delete",
            "DELETE",
            format!("/api/v1/posts/{}", s.b_post),
            Value::Null,
            StatusCode::NOT_FOUND,
        ),
        (
            "payout create",
            "POST",
            "/api/v1/finance/payouts".to_string(),
            json!({"seller_id": B, "amount": "1", "asset": "USDT", "destination": "forged"}),
            StatusCode::FORBIDDEN,
        ),
        (
            "product delete",
            "DELETE",
            format!("/api/v1/products/{}", s.b_product),
            Value::Null,
            StatusCode::NOT_FOUND,
        ),
        (
            "global channel config",
            "POST",
            "/api/v1/config/channel".to_string(),
            json!({"channel": B_CHANNEL}),
            StatusCode::FORBIDDEN,
        ),
        (
            "foreign channel metadata",
            "PATCH",
            format!("/api/v1/channels/{}", B_CHANNEL),
            json!({"title": "forged-title"}),
            StatusCode::FORBIDDEN,
        ),
        (
            "foreign post create",
            "POST",
            "/api/v1/posts".to_string(),
            json!({"channel": B_CHANNEL, "text": "forged-post", "seller_id": B}),
            StatusCode::FORBIDDEN,
        ),
        (
            "admin seller patch",
            "PATCH",
            format!("/api/v1/admin/sellers/{}", B),
            json!({"default_channel": "@forged"}),
            StatusCode::FORBIDDEN,
        ),
    ];
    for (label, method_name, path, body, expected) in cases {
        let (status, text) = rest(&f.app, &f.a.session, method_name, &path, Some(body)).await;
        assert_eq!(status, expected, "{label}: {text}");
    }
    // Creates that carry a forged seller_id still land in the caller's scope.
    let created = [
        (
            "plan",
            "POST",
            "/api/v1/plans",
            json!({"name": "forged-plan-owned-by-a", "price": "1", "asset": "USDT", "duration_days": 3, "seller_id": B}),
        ),
        (
            "subscription",
            "POST",
            "/api/v1/subscriptions",
            json!({"user_id": 5_002, "days": 3, "seller_id": B}),
        ),
        (
            "invoice",
            "POST",
            "/api/v1/invoices",
            json!({"amount": "1", "asset": "USDT", "user_id": 5_002, "seller_id": B, "description": "forged-invoice"}),
        ),
        (
            "code",
            "POST",
            "/api/v1/codes",
            json!({"plan_id": s.b_plan, "value_days": 3, "seller_id": B}),
        ),
        (
            "product",
            "POST",
            "/api/v1/products",
            json!({"name": "forged-product-owned-by-a", "price": "1", "asset": "USDT", "kind": "digital_link", "delivery": "payload", "seller_id": B}),
        ),
    ];
    for (label, method_name, path, body) in created {
        let (status, text) = rest(&f.app, &f.a.session, method_name, path, Some(body)).await;
        assert!(status.is_success(), "{label}: {text}");
    }
    assert_eq!(
        f.db.get_plan_global("forged-plan-owned-by-a").unwrap().0,
        A,
        "forged plan owner"
    );
    let new_sub =
        f.db.list_subscriptions(Some(A), 1000)
            .into_iter()
            .find(|x| x.2 == 5_002)
            .expect("forged subscription landed in A scope");
    assert_eq!(new_sub.1, A, "forged subscription owner");
    let new_code =
        f.db.list_codes(Some(A), 100)
            .into_iter()
            .find(|c| c.0 != s.a_code)
            .expect("forged code landed in A scope");
    assert_eq!(new_code.5, A, "forged code owner");
    assert!(
        f.db.get_product(A, "forged-product-owned-by-a").is_some(),
        "forged product owner"
    );
    assert_eq!(victim_snapshot(&f.db), before, "tenant B rows changed");
    assert_eq!(f.db.get_invoice(&s.b_invoice), invoice_before);
    assert_eq!(f.db.get_code(&s.b_code), code_before);
    assert!(f.db.list_posts(Some(B), 100).contains(&post_before));
    assert_eq!(f.db.get_product(B, s.b_product), product_before);
    assert_eq!(f.db.get_config("channel"), global_before);
    assert_eq!(
        request_count(&f.primary).await,
        0,
        "primary bot received traffic"
    );
    assert_eq!(
        request_count(&f.b.server).await,
        0,
        "tenant B bot received traffic"
    );
}

#[tokio::test]
async fn mcp_read_tools_are_scoped_per_seller_and_owner_sees_both() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    // tool | args | A marker | B marker
    let cases: [(&str, Value, &str, &str); 7] = [
        ("list_plans", json!({}), A_MARK, B_MARK),
        ("list_posts", json!({}), A_MARK, B_MARK),
        ("list_codes", json!({}), &s.a_code[..4], &s.b_code[..4]),
        ("list_subscriptions", json!({}), A_CHANNEL, B_CHANNEL),
        ("list_channels", json!({}), A_CHANNEL, B_CHANNEL),
        (
            "list_customers",
            json!({"include_user_ids": true}),
            A_MARK,
            B_MARK,
        ),
        (
            "subs_report",
            json!({"include_user_ids": true}),
            A_CHANNEL,
            B_CHANNEL,
        ),
    ];
    for (i, (tool, args, own, foreign)) in cases.into_iter().enumerate() {
        let a = tool_text(
            &mcp_call(&f.app, &f.a.mcp_token, i as i64 * 2 + 1, tool, args.clone()).await,
        );
        let a_text = a.to_string();
        assert!(a_text.contains(own), "{tool}: A marker missing: {a_text}");
        assert!(
            !a_text.contains(foreign),
            "{tool}: B marker leaked: {a_text}"
        );
        let b = tool_text(
            &mcp_call(&f.app, &f.b.mcp_token, i as i64 * 2 + 2, tool, args.clone()).await,
        );
        let b_text = b.to_string();
        assert!(
            b_text.contains(foreign),
            "{tool}: B marker missing: {b_text}"
        );
        assert!(!b_text.contains(own), "{tool}: A marker leaked: {b_text}");
        let owner = tool_text(&mcp_call(&f.app, &owner_token(), 100 + i as i64, tool, args).await);
        let owner_text = owner.to_string();
        assert!(
            owner_text.contains(own) && owner_text.contains(foreign),
            "{tool} owner control: {owner_text}"
        );
    }
    // Owner-only capability: list_bots is refused for a seller, available to
    // the owner, and names both tenants' bots.
    let seller_bots = mcp_call(&f.app, &f.a.mcp_token, 50, "list_bots", json!({})).await;
    assert_eq!(
        tool_text(&seller_bots)["error"],
        "owner credential required"
    );
    let owner_bots =
        tool_text(&mcp_call(&f.app, &owner_token(), 51, "list_bots", json!({})).await).to_string();
    assert!(owner_bots.contains("iso_alpha_bot") && owner_bots.contains("iso_beta_bot"));
}

#[tokio::test]
async fn mcp_write_tools_ignore_bot_and_seller_spoofs() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let before = victim_snapshot(&f.db);
    // A buyer's publish must use A's mapped bot even when every selector names
    // B's bot; the seller_id field is never an ownership input for a seller.
    for (i, spoof) in [
        json!({"bot_id": f.b.bot_id, "seller_id": B}),
        json!({"bot_key": f.b.bot_key, "seller_id": B}),
        json!({"bot": f.b.bot_key, "bot_id": f.b.bot_id, "seller_id": B}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut args = spoof;
        args["channel"] = json!(A_CHANNEL);
        args["text"] = json!("spoofed-post");
        let v = mcp_call(&f.app, &f.a.mcp_token, i as i64 + 1, "publish_post", args).await;
        assert!(v["error"].is_null(), "publish_post spoof #{i}: {v}");
    }
    assert_eq!(
        request_count(&f.a.server).await,
        3,
        "A's bot should send all three posts"
    );
    assert_eq!(request_count(&f.b.server).await, 0, "B's bot was spoofed");
    assert_eq!(
        request_count(&f.primary).await,
        0,
        "primary bot was used for A"
    );
    let posts = f.db.list_posts(Some(A), 100);
    assert_eq!(posts.len(), 4, "three new posts plus the seeded one");
    assert!(posts.iter().all(|p| p.1 == A));
    // customer_note: seller_id in the args is ignored for a seller principal.
    let v = mcp_call(
        &f.app,
        &f.a.mcp_token,
        10,
        "customer_note",
        json!({"seller_id": B, "user_id": 5_002, "note": "spoofed-note"}),
    )
    .await;
    assert_eq!(tool_text(&v)["seller_id"], A);
    assert_eq!(
        f.db.get_customer(A, 5_002).and_then(|c| c.5).as_deref(),
        Some("spoofed-note")
    );
    // booking status: a foreign booking id is refused without changing B rows.
    let v = mcp_call(
        &f.app,
        &f.a.mcp_token,
        11,
        "booking_set_status",
        json!({"booking_id": 9_999_999, "status": "cancelled", "seller_id": B}),
    )
    .await;
    assert!(
        v["result"]["isError"].as_bool().unwrap_or(false),
        "foreign booking: {v}"
    );
    assert_eq!(victim_snapshot(&f.db), before);
    assert!(s.b_post > 0);
}

#[tokio::test]
async fn mcp_sensitive_and_identity_tools_refuse_both_sellers_before_validation() {
    let f = make_fixture().await;
    let before_codes = f.db.count_codes(None);
    let mut id = 1i64;
    // tool | deliberately invalid args carrying a foreign seller_id
    let sensitive = [
        ("gen_codes", json!({"count": 999, "seller_id": B})),
        ("delete_post", json!({"post_id": 0, "seller_id": B})),
        ("broadcast", json!({"text": "", "seller_id": B})),
        ("ai_publish_post", json!({"draft_id": 0, "seller_id": B})),
        (
            "publish_as_user",
            json!({"channel": "", "text": "", "seller_id": B}),
        ),
        (
            "finance_refund_create",
            json!({"invoice_id": "", "seller_id": B}),
        ),
        (
            "finance_payout_create",
            json!({"amount": "", "asset": "", "seller_id": B}),
        ),
        (
            "export_buyer_data",
            json!({"seller_id": B, "user_id": 5_002}),
        ),
        (
            "delete_buyer_data",
            json!({"seller_id": B, "user_id": 5_002}),
        ),
    ];
    let identity = [
        ("read_channel", json!({})),
        ("read_thread", json!({"channel": "@x"})),
        ("search_messages", json!({"query": ""})),
        ("unread_digest", json!({})),
    ];
    for token in [&f.a.mcp_token, &f.b.mcp_token] {
        for (tool, args) in sensitive.iter().chain(identity.iter()) {
            let v = mcp_call(&f.app, token, id, tool, args.clone()).await;
            id += 1;
            assert_eq!(rpc_code(&v), -32001, "{tool}: {v}");
            let msg = v["error"]["message"].as_str().unwrap_or("");
            assert!(msg.contains("owner credential"), "{tool}: {msg}");
        }
    }
    assert!(
        f.db.mcp_list_approvals(None, 100).is_empty(),
        "a seller parked an approval"
    );
    assert_eq!(
        f.db.count_codes(None),
        before_codes,
        "sensitive tool had side effects"
    );
    let audit = f.db.mcp_list_audit(100);
    assert_eq!(audit.len(), 26, "one failure audit row per refusal");
    assert!(audit.iter().all(|a| !a.3 && a.2 == "seller"));
    // Control: the owner credential reaches validation and parks a sensitive
    // call; a seller token never gets that far.
    let v = mcp_call(
        &f.app,
        &owner_token(),
        99,
        "gen_codes",
        json!({"count": 2, "days": 30}),
    )
    .await;
    let parked = tool_text(&v);
    assert_eq!(parked["requires_approval"], true, "owner control: {v}");
    assert_eq!(f.db.mcp_list_approvals(Some("pending"), 100).len(), 1);
    assert_eq!(
        f.db.count_codes(None),
        before_codes,
        "parking is not execution"
    );
}

#[tokio::test]
async fn console_pages_and_posts_are_tenant_scoped() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let before = victim_snapshot(&f.db);
    let b_code_before = f.db.get_code(&s.b_code);
    // page | A marker | B marker
    let pages = [
        ("/plans", A_MARK, B_MARK),
        ("/posts", A_MARK, B_MARK),
        ("/products", s.a_product, s.b_product),
        ("/codes", s.a_code.as_str(), s.b_code.as_str()),
        ("/subs", A_CHANNEL, B_CHANNEL),
        ("/channels", A_CHANNEL, B_CHANNEL),
        ("/dashboard", A_MARK, B_MARK),
    ];
    for (path, own, foreign) in pages {
        let (status, body) = console(&f.app, &f.a.session, "GET", path, "").await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains(own), "{path}: A marker missing");
        assert!(!body.contains(foreign), "{path}: B marker leaked");
        let (status, body) = console(&f.app, &f.b.session, "GET", path, "").await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(body.contains(foreign), "{path}: B marker missing");
        assert!(!body.contains(own), "{path}: A marker leaked");
    }
    // Cross-tenant console POSTs: no victim row changes and no victim bot traffic.
    let posts = [
        (
            "plan delete by foreign id",
            format!("/plans/{}/delete", s.b_plan),
            String::new(),
            StatusCode::SEE_OTHER,
        ),
        (
            "code delete",
            "/codes/delete".to_string(),
            format!("code={}", s.b_code),
            StatusCode::SEE_OTHER,
        ),
        (
            "code reclaim",
            "/codes/reclaim".to_string(),
            format!("code={}", s.b_code),
            StatusCode::OK,
        ),
        (
            "product delete",
            "/productdel".to_string(),
            "name=iso_beta_product".to_string(),
            StatusCode::SEE_OTHER,
        ),
    ];
    for (label, path, form, expected) in posts {
        let (status, body) = console(&f.app, &f.a.session, "POST", &path, &form).await;
        assert_eq!(status, expected, "{label}: {body}");
    }
    assert_eq!(
        victim_snapshot(&f.db),
        before,
        "a console POST changed B rows"
    );
    assert_eq!(
        f.db.get_code(&s.b_code),
        b_code_before,
        "B code was consumed"
    );
    assert_only_a_contacted(&f).await;
    // Owner control: the same console page spans both tenants.
    let (status, body) = console(&f.app, &f.owner_session, "GET", "/plans", "").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(A_MARK) && body.contains(B_MARK),
        "owner plans: {body}"
    );
}

#[tokio::test]
async fn bot_resolution_follows_seller_and_channel_ownership() {
    let f = make_fixture().await;
    assert_eq!(f.state.bots.for_seller(A).id, f.a.bot_id);
    assert_eq!(f.state.bots.for_seller(B).id, f.b.bot_id);
    assert_eq!(f.state.bots.for_seller(UNMAPPED).id, 0, "unmapped fallback");
    assert_eq!(f.state.bots.for_channel(A_CHANNEL).id, f.a.bot_id);
    assert_eq!(f.state.bots.for_channel(B_CHANNEL).id, f.b.bot_id);
    assert_eq!(f.state.bots.for_channel("@unknown_iso_channel").id, 0);
    let a_session = daybook::web::Session {
        account_id: 2,
        role: "seller".to_string(),
        seller_id: Some(A),
        selected_bot_id: Some(f.b.bot_id),
        csrf_token: String::new(),
    };
    let b_session = daybook::web::Session {
        account_id: 3,
        role: "seller".to_string(),
        seller_id: Some(B),
        selected_bot_id: Some(f.a.bot_id),
        csrf_token: String::new(),
    };
    assert_eq!(web::effective_bot(&f.state, &a_session).id, f.a.bot_id);
    assert_eq!(web::effective_bot(&f.state, &b_session).id, f.b.bot_id);
}

/// Regression for the cross-tenant expire-check write: a seller token may
/// expire only that seller's own active-but-expired rows. The owner control
/// proves the global sweep is still available to the privileged tier.
#[tokio::test]
async fn expire_check_must_not_touch_another_tenants_subscriptions() {
    let f = make_fixture().await;
    let victim =
        f.db.create_subscription(B, 5_003, None, B_CHANNEL, -1, None, None);
    let own =
        f.db.create_subscription(A, 5_004, None, A_CHANNEL, -1, None, None);
    let status = |id: i64| {
        f.db.list_subscriptions(None, 1000)
            .into_iter()
            .find(|s| s.0 == id)
            .unwrap()
            .4
            .clone()
    };
    assert_eq!(status(victim), "active");
    assert_eq!(status(own), "active");
    let (status_code, body) = rest(
        &f.app,
        &f.a.session,
        "POST",
        "/api/v1/subscriptions/expire-check",
        None,
    )
    .await;
    assert_eq!(status_code, StatusCode::OK);
    assert_eq!(
        body, "{\"expired\":1}",
        "seller must expire only its own row: {body}"
    );
    assert_eq!(
        status(own),
        "expired",
        "A's own expired row was not expired"
    );
    assert_eq!(status(victim), "active", "B's row was mutated by A");

    // Owner control: the same endpoint still sweeps every tenant globally.
    let (status_code, body) = rest(
        &f.app,
        &f.owner_session,
        "POST",
        "/api/v1/subscriptions/expire-check",
        None,
    )
    .await;
    assert_eq!(status_code, StatusCode::OK);
    assert_eq!(body, "{\"expired\":1}", "owner control: {body}");
    assert_eq!(status(victim), "expired", "owner did not expire B's row");
}

/// Black-box sweep over every mutating REST and console route covered by this
/// suite. Each route is driven twice: once with no credential (must be refused
/// before the handler runs), then as seller A with a payload aimed at tenant
/// B's ids. A session is never an ownership input: owner/admin-only routes
/// answer 403/404 for a seller, and seller-scoped routes derive the owner from
/// the session, so B's rows and B's bot are untouched on every path.
#[tokio::test]
async fn mutating_routes_require_session_and_never_mutate_another_tenant() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let a_post = f.db.list_posts(Some(A), 10)[0].0;
    let before = victim_snapshot(&f.db);
    let b_invoice_before = f.db.get_invoice(&s.b_invoice);
    let global_before = f.db.get_config("channel");

    // label | method | path | body (seller_id/bot_id/channel aimed at B) | seller is denied
    let rest_cases: Vec<(&str, &str, String, Value, bool)> = vec![
        (
            "seller create",
            "POST",
            "/api/v1/sellers".into(),
            json!({"user_id": B, "username": "sweep", "default_channel": B_CHANNEL}),
            true,
        ),
        (
            "seller channel set",
            "POST",
            format!("/api/v1/sellers/{B}/channel"),
            json!({"channel": B_CHANNEL}),
            true,
        ),
        (
            "seller ai settings",
            "POST",
            format!("/api/v1/sellers/{B}/ai-settings"),
            json!({"daily_token_budget": 1, "seller_id": A}),
            true,
        ),
        (
            "plan create",
            "POST",
            "/api/v1/plans".into(),
            json!({"name": "sweep-plan", "price": "1", "asset": "USDT", "duration_days": 3, "channel_id": B_CHANNEL, "seller_id": B}),
            false,
        ),
        (
            "plan patch",
            "PATCH",
            format!("/api/v1/plans/{}", s.b_plan),
            json!({"name": "sweep", "seller_id": A}),
            true,
        ),
        (
            "plan delete",
            "DELETE",
            format!("/api/v1/plans/{}", s.b_plan),
            Value::Null,
            true,
        ),
        (
            "plan active",
            "POST",
            format!("/api/v1/plans/{}/active", s.b_plan),
            json!({"active": false}),
            true,
        ),
        (
            "subscription grant",
            "POST",
            "/api/v1/subscriptions".into(),
            json!({"user_id": 5_002, "days": 3, "seller_id": B}),
            false,
        ),
        (
            "subscription patch",
            "PATCH",
            format!("/api/v1/subscriptions/{}", s.b_sub),
            json!({"status": "expired", "seller_id": A}),
            true,
        ),
        (
            "subscription expire-check",
            "POST",
            "/api/v1/subscriptions/expire-check".into(),
            Value::Null,
            false,
        ),
        (
            "invoice create",
            "POST",
            "/api/v1/invoices".into(),
            json!({"amount": "1", "asset": "USDT", "user_id": 5_002, "seller_id": B, "description": "sweep"}),
            false,
        ),
        (
            "invoice status",
            "POST",
            format!("/api/v1/invoices/{}/status", s.b_invoice),
            json!({"status": "expired"}),
            true,
        ),
        (
            "code create",
            "POST",
            "/api/v1/codes".into(),
            json!({"plan_id": s.b_plan, "value_days": 3, "seller_id": B}),
            false,
        ),
        (
            "code reclaim",
            "POST",
            "/api/v1/codes/reclaim".into(),
            json!({"code": s.b_code, "seller_id": B}),
            true,
        ),
        (
            "code delete",
            "DELETE",
            format!("/api/v1/codes/{}", s.b_code),
            Value::Null,
            true,
        ),
        (
            "code redeem (own code, forged seller)",
            "POST",
            format!("/api/v1/codes/{}/redeem", s.a_code),
            json!({"seller_id": B}),
            false,
        ),
        (
            "post create",
            "POST",
            "/api/v1/posts".into(),
            json!({"channel": B_CHANNEL, "text": "sweep", "seller_id": B}),
            true,
        ),
        (
            "post schedule",
            "POST",
            "/api/v1/posts/schedule".into(),
            json!({"channel": B_CHANNEL, "text": "sweep", "scheduled_at": "2026-01-02T03:04:05Z", "seller_id": B}),
            true,
        ),
        (
            "post patch",
            "PATCH",
            format!("/api/v1/posts/{}", s.b_post),
            json!({"text": "sweep", "seller_id": A}),
            true,
        ),
        (
            "post delete",
            "DELETE",
            format!("/api/v1/posts/{}", s.b_post),
            Value::Null,
            true,
        ),
        (
            "post pin",
            "POST",
            format!("/api/v1/posts/{}/pin", s.b_post),
            Value::Null,
            true,
        ),
        (
            "post unpin",
            "POST",
            format!("/api/v1/posts/{}/unpin", s.b_post),
            Value::Null,
            true,
        ),
        (
            "post pin (own row, forged seller)",
            "POST",
            format!("/api/v1/posts/{a_post}/pin"),
            json!({"seller_id": B}),
            false,
        ),
        (
            "broadcast",
            "POST",
            "/api/v1/broadcasts".into(),
            json!({"text": "sweep", "seller_id": B}),
            false,
        ),
        (
            "user upsert",
            "POST",
            "/api/v1/users/upsert".into(),
            json!({"user_id": 5_002, "username": "sweep", "seller_id": B}),
            false,
        ),
        (
            "channel patch",
            "PATCH",
            format!("/api/v1/channels/{B_CHANNEL}"),
            json!({"title": "sweep", "seller_id": B}),
            true,
        ),
        (
            "global channel config",
            "POST",
            "/api/v1/config/channel".into(),
            json!({"channel": B_CHANNEL}),
            true,
        ),
        (
            "ai template create",
            "POST",
            "/api/v1/ai/templates".into(),
            json!({"name": "sweep-tpl", "user_prompt": "x", "seller_id": B}),
            false,
        ),
        (
            "ai template patch",
            "PATCH",
            "/api/v1/ai/templates/sweep-tpl".into(),
            json!({"description": "x", "seller_id": B}),
            false,
        ),
        (
            "ai template delete",
            "DELETE",
            "/api/v1/ai/templates/sweep-tpl".into(),
            Value::Null,
            false,
        ),
        (
            "product create",
            "POST",
            "/api/v1/products".into(),
            json!({"name": "sweep-product", "price": "1", "asset": "USDT", "kind": "digital_link", "delivery": "payload", "seller_id": B}),
            false,
        ),
        (
            "product delete",
            "DELETE",
            format!("/api/v1/products/{}", s.b_product),
            Value::Null,
            true,
        ),
        (
            "payout create",
            "POST",
            "/api/v1/finance/payouts".into(),
            json!({"seller_id": B, "amount": "1", "asset": "USDT", "destination": "sweep"}),
            true,
        ),
        (
            "payout patch",
            "PATCH",
            format!("/api/v1/finance/payouts/{}", s.a_payout),
            json!({"status": "failed", "seller_id": B}),
            true,
        ),
        (
            "payout process",
            "POST",
            format!("/api/v1/finance/payouts/{}/process", s.a_payout),
            Value::Null,
            true,
        ),
        (
            "payout approve",
            "POST",
            format!("/api/v1/finance/payouts/{}/approve", s.a_payout),
            Value::Null,
            true,
        ),
        (
            "payout execute",
            "POST",
            format!("/api/v1/finance/payouts/{}/execute", s.a_payout),
            Value::Null,
            true,
        ),
        (
            "refund create",
            "POST",
            "/api/v1/finance/refunds".into(),
            json!({"invoice_id": s.b_invoice, "amount": "1", "seller_id": B}),
            true,
        ),
        (
            "refund process",
            "POST",
            "/api/v1/finance/refunds/1/process".into(),
            Value::Null,
            true,
        ),
        (
            "refund approve",
            "POST",
            "/api/v1/finance/refunds/1/approve".into(),
            Value::Null,
            true,
        ),
        (
            "refund execute",
            "POST",
            "/api/v1/finance/refunds/1/execute".into(),
            Value::Null,
            true,
        ),
        (
            "reconciliation resolve",
            "POST",
            "/api/v1/finance/reconciliation/subscription/1/resolve".into(),
            json!({"outcome": "void", "reason": "sweep", "seller_id": B}),
            true,
        ),
        (
            "admin seller patch",
            "PATCH",
            format!("/api/v1/admin/sellers/{B}"),
            json!({"default_channel": B_CHANNEL}),
            true,
        ),
        (
            "admin create",
            "POST",
            "/api/v1/admin/admins".into(),
            json!({"user_id": B, "username": "sweep", "seller_id": B}),
            true,
        ),
        (
            "admin delete",
            "DELETE",
            format!("/api/v1/admin/admins/{B}"),
            Value::Null,
            true,
        ),
        (
            "web account create",
            "POST",
            "/api/v1/admin/web-accounts".into(),
            json!({"username": "sweep", "password": "sweep-pw", "role": "seller", "seller_id": B}),
            true,
        ),
        (
            "web account delete",
            "DELETE",
            "/api/v1/admin/web-accounts/9999".into(),
            Value::Null,
            true,
        ),
        (
            "web account password",
            "PATCH",
            "/api/v1/admin/web-accounts/9999/password".into(),
            json!({"password": "sweep-pw", "seller_id": B}),
            true,
        ),
        (
            "api key create",
            "POST",
            "/api/v1/auth/api-keys".into(),
            json!({"name": "sweep", "scopes": "*", "seller_id": B}),
            false,
        ),
        (
            "api key delete",
            "DELETE",
            "/api/v1/auth/api-keys/9999".into(),
            Value::Null,
            true,
        ),
    ];

    for (label, method, path, body, deny) in rest_cases {
        let anon = rest_without_session(&f.app, method, &path, Some(body.clone())).await;
        assert_eq!(
            anon,
            StatusCode::UNAUTHORIZED,
            "{label}: sessionless caller was not refused"
        );
        let (status, text) = rest(&f.app, &f.a.session, method, &path, Some(body)).await;
        if deny {
            assert!(
                is_denied(status),
                "{label}: seller A expected 403/404, got {status}: {text}"
            );
        } else {
            assert!(
                status.is_success(),
                "{label}: seller-scoped create expected success, got {status}: {text}"
            );
        }
        assert_eq!(
            victim_snapshot(&f.db),
            before,
            "{label}: tenant B rows changed"
        );
        assert_eq!(
            f.db.get_invoice(&s.b_invoice),
            b_invoice_before,
            "{label}: tenant B invoice changed"
        );
        assert_eq!(
            f.db.get_config("channel"),
            global_before,
            "{label}: global channel changed"
        );
    }

    // Console mutations answer with redirects rather than JSON status codes;
    // the invariant is the same: no cookie means no handler, and an A-targeted
    // form never changes B's rows or reaches B's bot.
    let console_cases: Vec<(&str, String, String)> = vec![
        (
            "console plan delete",
            format!("/plans/{}/delete", s.b_plan),
            String::new(),
        ),
        (
            "console code delete",
            "/codes/delete".into(),
            format!("code={}", s.b_code),
        ),
        (
            "console code reclaim",
            "/codes/reclaim".into(),
            format!("code={}", s.b_code),
        ),
        (
            "console product delete",
            "/productdel".into(),
            format!("name={}", s.b_product),
        ),
    ];
    for (label, path, form) in console_cases {
        let anon = console_without_session(&f.app, &path, &form).await;
        assert_eq!(
            anon,
            StatusCode::FORBIDDEN,
            "{label}: sessionless console POST was not refused"
        );
        let (status, text) = console(&f.app, &f.a.session, "POST", &path, &form).await;
        assert!(
            status.is_success() || status.is_redirection(),
            "{label}: {status}: {text}"
        );
        assert_eq!(
            victim_snapshot(&f.db),
            before,
            "{label}: tenant B rows changed"
        );
        assert_eq!(
            f.db.get_invoice(&s.b_invoice),
            b_invoice_before,
            "{label}: tenant B invoice changed"
        );
        assert_eq!(
            f.db.get_config("channel"),
            global_before,
            "{label}: global channel changed"
        );
    }

    assert_eq!(
        request_count(&f.b.server).await,
        0,
        "tenant B bot received traffic"
    );
    assert_eq!(
        request_count(&f.primary).await,
        0,
        "primary bot received traffic"
    );
}

/// A web account whose role carries no tenant authority must not fall into the
/// operator arm of a console write. The buyer tier is provisionable through the
/// owner-only REST endpoint with an arbitrary role string; its cookie therefore
/// has to fail closed at every route that derives a namespace from the session.
#[tokio::test]
async fn tenantless_console_session_cannot_write_in_the_global_namespace() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let before = victim_snapshot(&f.db);

    f.db.create_web_account("tenantless-buyer", "local-pw", "user", None)
        .await
        .unwrap();
    let (id, _, _, _, _) =
        f.db.get_web_account_by_username("tenantless-buyer")
            .unwrap();
    let buyer = f.db.create_web_session(id, "user", None, 7);

    // A real B-owned ticket: without it the ticket gates could pass vacuously.
    let ticket_id = f.db.create_ticket(B, 5_002, "iso ticket B");
    let ticket_before = f.db.get_ticket(ticket_id);

    let posts = [
        (
            "create plan",
            "/plan",
            "name=foreign&price=1&asset=USDT&duration_days=1",
        ),
        ("delete plan by name", "/plandel", "name=iso_beta_plan"),
        (
            "delete plan by id",
            &format!("/plans/{}/delete", s.b_plan),
            "",
        ),
        (
            "create product",
            "/product",
            "name=foreign&price=1&asset=USDT&kind=digital",
        ),
        ("delete product", "/productdel", "name=iso_beta_product"),
        (
            "activate product",
            "/product/activate",
            "name=iso_beta_product",
        ),
        (
            "delete code",
            "/codes/delete",
            &format!("code={}", s.b_code),
        ),
        (
            "reclaim code",
            "/codes/reclaim",
            &format!("code={}", s.b_code),
        ),
        ("revoke promo", "/promos/revoke", "code=iso_beta_promo"),
        ("approve review", "/reviews/approve", "id=1"),
        (
            "reply to ticket",
            "/tickets/reply",
            &format!("ticket_id={ticket_id}&text=foreign"),
        ),
        (
            "set ticket status",
            "/tickets/status",
            &format!("ticket_id={ticket_id}&status=closed"),
        ),
    ];
    for (label, path, form) in posts {
        let (status, body) = console(&f.app, &buyer, "POST", path, form).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{label}: {body}");
    }

    assert_eq!(
        victim_snapshot(&f.db),
        before,
        "a tenantless console POST changed B rows"
    );
    assert_eq!(
        f.db.get_ticket(ticket_id),
        ticket_before,
        "a tenantless console POST changed B's ticket"
    );
    assert_eq!(
        request_count(&f.b.server).await,
        0,
        "tenant B bot received traffic"
    );
    assert_eq!(
        request_count(&f.primary).await,
        0,
        "primary bot received traffic"
    );
}

/// The buyer tier can hold a REST bearer but no tenant authority. The old
/// seller-only equality gates fell through for that tier and exposed another
/// seller's rows; the scope gates must refuse it while the owner still reads.
#[tokio::test]
async fn tenantless_rest_session_cannot_read_or_write_other_tenants() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let before = victim_snapshot(&f.db);
    let invoice_before = f.db.get_invoice(&s.b_invoice);

    f.db.create_web_account("tenantless-rest-buyer", "local-pw", "user", None)
        .await
        .unwrap();
    let (id, _, _, _, _) =
        f.db.get_web_account_by_username("tenantless-rest-buyer")
            .unwrap();
    let buyer = f.db.create_web_session(id, "user", None, 7);

    let get_paths = [
        format!("/api/v1/sellers/{B}"),
        format!("/api/v1/plans/{}", s.b_plan),
        format!("/api/v1/invoices/{}", s.b_invoice),
        format!("/api/v1/codes/{}", s.b_code),
    ];
    for path in &get_paths {
        let (status, body) = rest(&f.app, &buyer, "GET", path, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "buyer GET {path}: {body}");
        assert!(
            !body.contains(B_MARK),
            "buyer GET {path} leaked B marker: {body}"
        );

        let (status, body) = rest(&f.app, &f.owner_session, "GET", path, None).await;
        assert_eq!(status, StatusCode::OK, "owner control GET {path}: {body}");
    }

    let (status, body) = rest(
        &f.app,
        &buyer,
        "POST",
        &format!("/api/v1/invoices/{}/status", s.b_invoice),
        Some(json!({"status": "expired"})),
    )
    .await;
    assert!(
        is_denied(status),
        "buyer invoice status POST: {status} {body}"
    );

    assert_eq!(
        victim_snapshot(&f.db),
        before,
        "a tenantless REST request changed B rows"
    );
    assert_eq!(
        f.db.get_invoice(&s.b_invoice),
        invoice_before,
        "a tenantless REST request changed B's invoice"
    );
}
