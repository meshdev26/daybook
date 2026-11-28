//! Loopback attack cases driven through the merged router. Each case sends a
//! request a hostile caller could send, then checks the victim tenant's rows.
//! The database is a TempDir and the MCP token is a placeholder.

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

async fn bot_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "ok": true,
            "result": {"message_id":1,"date":1_700_000_000,
                       "chat":{"id":42,"type":"private","first_name":"T"},"text":"sent"}
        })))
        .mount(&server)
        .await;
    server
}

async fn make_fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("adversarial_local.db");
    let db = Arc::new(Db::new(db_path.to_str().unwrap()).unwrap());
    for (name, role, sid) in [("seller_alpha", "seller", Some(A)), ("buyer", "user", None)] {
        db.create_web_account(name, "local-pw", role, sid)
            .await
            .unwrap();
    }
    db.set_config("setup_done", "1");
    db.set_config("channel", "@adv_global_channel");
    db.upsert_seller(A, Some("seller_alpha"), Some(A_CHANNEL));
    let cfg = Config {
        owner_id: 1,
        db_path: db_path.to_str().unwrap().to_string(),
        crypto_token: None,
        crypto_api_base: None,
        ai_enabled: false,
        mfa_recovery_codes_enabled: true,
        ai_provider: "openai".into(),
        ai_api_style: "openai".into(),
        ai_base_url: "https://api.openai.com/v1".into(),
        ai_api_key: None,
        ai_model: "gpt-4o-mini".into(),
        ai_daily_token_budget: None,
        mcp_owner_token: Some(OWNER_TOKEN.to_string()),
        telegram_api_id: None,
        telegram_api_hash: None,
        bots: Vec::new(),
        bots_legacy: true,
    };
    let server = bot_server().await;
    let shared = Arc::new(cfg.clone());
    let bot = teloxide::Bot::new("0:local-placeholder")
        .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());
    let primary = Arc::new(daybook::bots::BotContext {
        id: 0,
        key: "primary".into(),
        bot,
        owner_id: 0,
        shared,
        db: db.clone(),
    });
    let bots = Arc::new(daybook::bots::BotManager {
        bots: vec![primary.clone()],
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
    let (buyer_acc, ..) = db.get_web_account_by_username("buyer").unwrap();
    let (alpha_acc, ..) = db.get_web_account_by_username("seller_alpha").unwrap();
    let (_, a_mcp_token) = db.create_mcp_token(A, Some("adv-a"));
    Fixture {
        _dir: dir,
        db: db.clone(),
        state,
        app,
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

fn seed(db: &Db) -> Seed {
    db.add_plan(A, "adv alpha plan", "10", "USDT", 30, Some(A_CHANNEL));
    db.add_plan(B, "adv beta plan", "20", "USDT", 30, Some(B_CHANNEL));
    let a_plan = db.get_plan(A, "adv alpha plan").unwrap().0;
    db.save_invoice(
        B_INVOICE,
        B,
        "20",
        "USDT",
        Some(B_MARK),
        "active",
        None,
        None,
        None,
        None,
    );
    let b_product_id = db
        .add_product(
            B,
            B_PRODUCT,
            "20",
            "USDT",
            "digital_link",
            Some("beta-delivery"),
            Some(9),
        )
        .unwrap();
    let b_review = db
        .create_review(5_002, B, b_product_id, 5, Some(B_MARK))
        .unwrap();
    let b_ticket = db.create_ticket(B, 5_002, B_MARK);
    db.log_post(B, B_CHANNEL, 22, "text", Some(B_MARK), "bot")
        .unwrap();
    let b_post = db.list_posts(Some(B), 10)[0].0;
    Seed {
        b_product_id,
        b_review,
        b_ticket,
        b_post,
        a_plan,
    }
}

fn victim_snapshot(db: &Db) -> String {
    format!(
        "{:?}",
        (
            db.list_plans(Some(B)),
            db.list_codes(Some(B), 100),
            db.list_invoices(Some(B), 100),
            db.get_product(B, B_PRODUCT)
        )
    )
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

async fn rest(app: &Router, tok: &str, m: &str, p: &str, body: Value) -> (StatusCode, String) {
    let payload = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    let auth = format!("Bearer {tok}");
    let req = Request::builder()
        .method(m)
        .uri(p)
        .header("authorization", auth)
        .header("content-type", "application/json")
        .body(Body::from(payload))
        .unwrap();
    send(app, req).await
}

async fn console(app: &Router, sess: &str, m: &str, p: &str, form: &str) -> (StatusCode, String) {
    let t = daybook::auth::csrf_token_for_session(sess);
    let payload = format!("{form}&csrf_token={t}");
    let ct = if m == "POST" {
        "application/x-www-form-urlencoded"
    } else {
        "application/json"
    };
    let cookie = format!("session={sess}");
    let req = Request::builder()
        .method(m)
        .uri(p)
        .header("cookie", cookie)
        .header("content-type", ct)
        .body(Body::from(payload))
        .unwrap();
    send(app, req).await
}

/// Console GET/POST over the bearer transport. Mirrors `console()` but carries
/// the same session token in `Authorization` instead of a `session=` cookie, so
/// a scope-gate comparison changes only the transport.
async fn bearer_console(
    app: &Router,
    sess: &str,
    m: &str,
    p: &str,
    form: &str,
) -> (StatusCode, String) {
    let payload = if form.is_empty() {
        String::new()
    } else {
        form.to_string()
    };
    let ct = if m == "POST" {
        "application/x-www-form-urlencoded"
    } else {
        "application/json"
    };
    let req = Request::builder()
        .method(m)
        .uri(p)
        .header("authorization", format!("Bearer {sess}"))
        .header("content-type", ct)
        .body(Body::from(payload))
        .unwrap();
    send(app, req).await
}

async fn mcp_call(app: &Router, tok: &str, tool: &str, args: Value) -> Value {
    let rpc = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":tool,"arguments":args}});
    let (code, text) = rest(app, tok, "POST", "/api/v1/mcp", rpc).await;
    assert_eq!(code, StatusCode::OK, "tool {tool}: {text}");
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

#[tokio::test]
async fn buyer_bearer_cannot_forge_invoice_status_executed() {
    let f = make_fixture().await;
    let _s = seed(&f.db);
    let before = f.db.get_invoice(B_INVOICE);
    let p = format!("/api/v1/invoices/{B_INVOICE}/status");
    // "executed" is not a legal transition; "expired" is. Scope must refuse
    // both before the status vocabulary is consulted.
    for status in ["executed", "expired"] {
        let (code, text) = rest(
            &f.app,
            &f.buyer_session,
            "POST",
            &p,
            json!({"status":status}),
        )
        .await;
        assert_eq!(
            code,
            StatusCode::FORBIDDEN,
            "buyer invoice {status}: {code} {text}"
        );
        assert_eq!(
            f.db.get_invoice(B_INVOICE),
            before,
            "B invoice mutated: {status}"
        );
    }
}

#[tokio::test]
async fn buyer_cannot_approve_foreign_review_or_ticket() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let approved = || {
        f.db.list_reviews(B, s.b_product_id, true)
            .iter()
            .any(|r| r.0 == s.b_review)
    };
    assert!(!approved(), "fixture review already approved");
    let ticket_before = f.db.get_ticket(s.b_ticket).unwrap();
    let (code, body) = console(
        &f.app,
        &f.buyer_session,
        "POST",
        "/reviews/approve",
        &format!("id={}", s.b_review),
    )
    .await;
    assert_eq!(
        code,
        StatusCode::FORBIDDEN,
        "buyer approved B review: {code} {body}"
    );
    assert!(!approved(), "B review approved by buyer");
    let (code, body) = console(
        &f.app,
        &f.buyer_session,
        "POST",
        "/tickets/status",
        &format!("ticket_id={}&status=resolved", s.b_ticket),
    )
    .await;
    assert_eq!(
        code,
        StatusCode::FORBIDDEN,
        "buyer changed B ticket: {code} {body}"
    );
    assert_eq!(f.db.get_ticket(s.b_ticket), Some(ticket_before));
}

#[tokio::test]
async fn seller_cannot_delete_foreign_post_via_mcp_direct() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let principal = daybook::mcp::tools::Principal {
        role: daybook::roles::Role::Seller,
        seller_id: Some(A),
    };
    let err = daybook::mcp::tools::execute_sensitive(
        f.state.clone(),
        &principal,
        "delete_post",
        json!({"post_id":s.b_post}),
    )
    .await
    .expect_err("seller A deleted B's post directly");
    assert_eq!(err, "post not found or not yours");
    assert_eq!(
        f.db.get_post_seller(s.b_post),
        Some(B),
        "B post deleted by A"
    );
}

#[tokio::test]
async fn forged_seller_id_in_body_does_not_widen() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let before = victim_snapshot(&f.db);
    let codes_before: Vec<String> =
        f.db.list_codes(Some(A), 100)
            .into_iter()
            .map(|c| c.0)
            .collect();
    let invs_before: Vec<String> =
        f.db.list_invoices(Some(A), 100)
            .into_iter()
            .map(|i| i.0)
            .collect();
    let body = json!({"name":"adv forged plan","price":"1","asset":"USDT","duration_days":3,"channel_id":A_CHANNEL,"seller_id":B});
    let (code, text) = rest(&f.app, &f.a_session, "POST", "/api/v1/plans", body).await;
    assert!(code.is_success(), "plan create: {code} {text}");
    assert_eq!(
        f.db.get_plan_global("adv forged plan").unwrap().0,
        A,
        "plan under B"
    );
    let body = json!({"plan_id":s.a_plan,"value_days":3,"seller_id":B});
    let (code, text) = rest(&f.app, &f.a_session, "POST", "/api/v1/codes", body).await;
    assert!(code.is_success(), "code create: {code} {text}");
    let row =
        f.db.list_codes(Some(A), 100)
            .into_iter()
            .find(|c| !codes_before.contains(&c.0))
            .expect("no forged code in A scope");
    assert_eq!(row.5, A, "code under B");
    let body = json!({"amount":"1","asset":"USDT","user_id":5_002,"seller_id":B,"description":"adv forged invoice"});
    let (code, text) = rest(&f.app, &f.a_session, "POST", "/api/v1/invoices", body).await;
    assert!(code.is_success(), "invoice create: {code} {text}");
    let inv =
        f.db.list_invoices(Some(A), 100)
            .into_iter()
            .find(|i| !invs_before.contains(&i.0))
            .expect("no forged invoice in A scope");
    assert_eq!(f.db.get_invoice(&inv.0).unwrap().6, A, "invoice under B");
    assert_eq!(victim_snapshot(&f.db), before, "tenant B rows changed");
}

#[tokio::test]
async fn tenantless_session_cannot_read_console_global_pages() {
    let f = make_fixture().await;
    let _s = seed(&f.db);
    for p in ["/dashboard", "/plans", "/subs", "/customers"] {
        let (code, body) = console(&f.app, &f.buyer_session, "GET", p, "").await;
        assert_eq!(code, StatusCode::FORBIDDEN, "buyer read {p}: {code}");
        assert!(
            !body.contains(B_MARK) && !body.contains(B_CHANNEL),
            "{p} leaked tenant B"
        );
    }
}

#[tokio::test]
async fn unbound_seller_session_cannot_authenticate() {
    let f = make_fixture().await;
    let (account_id, ..) = f.db.get_web_account_by_username("seller_alpha").unwrap();
    // create_web_account refuses seller+NULL, but the session row is what the
    // loader reads. Forge that legacy shape through the public mint API.
    let scopeless = f.db.create_web_session(account_id, "seller", None, 7);
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {scopeless}")).unwrap(),
    );
    assert!(
        daybook::auth::session_from_headers(&f.state, &headers).is_none(),
        "scopeless bearer resolved"
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::COOKIE,
        HeaderValue::from_str(&format!("session={scopeless}")).unwrap(),
    );
    assert!(
        daybook::auth::session_from_headers(&f.state, &headers).is_none(),
        "scopeless cookie resolved"
    );
    let (code, _) = rest(&f.app, &scopeless, "GET", "/api/v1/plans", Value::Null).await;
    assert_eq!(
        code,
        StatusCode::UNAUTHORIZED,
        "scopeless session reached REST"
    );
}

#[tokio::test]
async fn mcp_seller_token_cannot_park_sensitive_delete_post() {
    let f = make_fixture().await;
    let s = seed(&f.db);
    let before = f.db.mcp_list_approvals(None, 100).len();
    let v = mcp_call(
        &f.app,
        &f.a_mcp_token,
        "delete_post",
        json!({"post_id":s.b_post,"seller_id":B}),
    )
    .await;
    assert_eq!(
        v["error"]["code"].as_i64().unwrap_or(0),
        -32001,
        "seller parked: {v}"
    );
    assert_eq!(
        f.db.mcp_list_approvals(None, 100).len(),
        before,
        "seller parked approval"
    );
    assert_eq!(
        f.db.get_post_seller(s.b_post),
        Some(B),
        "B post deleted via MCP"
    );
    assert!(
        f.db.mcp_list_audit(100)
            .iter()
            .any(|a| a.2 == "seller" && !a.3),
        "no failed seller audit"
    );
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
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "buyer must not write seller -1: {status} {body}"
    );
    assert!(
        f.db.get_seller(-1).is_none(),
        "phantom seller -1 was created"
    );
}

/// Read gates must refuse the denied sentinel as a target id, rather than
/// falling through to a 404 from a missing seller row.
#[tokio::test]
async fn buyer_cannot_target_sentinel_in_seller_reads() {
    let f = make_fixture().await;
    assert!(
        f.db.get_seller(-1).is_none(),
        "phantom seller -1 exists before the requests"
    );

    // The router registers GET on the seller row and on its AI settings. It
    // registers only POST on `.../channel`, so that path is exercised below
    // with its registered method instead of a GET that would return 405.
    for p in ["/api/v1/sellers/-1", "/api/v1/sellers/-1/ai-settings"] {
        let (code, body) = rest(&f.app, &f.buyer_session, "GET", p, Value::Null).await;
        assert_eq!(code, StatusCode::FORBIDDEN, "buyer read {p}: {code} {body}");
    }

    // A negative numeric channel id skips proof-of-control for non-admins
    // (src/api.rs), so without the ownership gate this write would succeed and
    // `set_seller_channel(-1, ..)` would upsert a phantom sellers(-1) row.
    let (code, body) = rest(
        &f.app,
        &f.buyer_session,
        "POST",
        "/api/v1/sellers/-1/channel",
        json!({"channel": "-1001234567890"}),
    )
    .await;
    assert_eq!(
        code,
        StatusCode::FORBIDDEN,
        "buyer bound a channel to seller -1: {code} {body}"
    );
    assert!(
        f.db.get_seller(-1).is_none(),
        "phantom seller -1 was created by a sentinel request"
    );
}

/// These id-based handlers look the row up before they check ownership. `-1`
/// names no plan or code, so the observed denial is NOT_FOUND (404), not the
/// 403 an existing victim row would produce.
#[tokio::test]
async fn buyer_cannot_target_sentinel_in_plan_and_code_routes() {
    let f = make_fixture().await;
    let _s = seed(&f.db);
    let before = victim_snapshot(&f.db);

    let cases = [
        ("POST", "/api/v1/plans/-1/active", json!({"active": false})),
        (
            "PATCH",
            "/api/v1/plans/-1",
            json!({"name":"sentinel-plan","price":"1","asset":"USDT","duration_days":3,"channel_id":A_CHANNEL}),
        ),
        ("DELETE", "/api/v1/codes/-1", Value::Null),
    ];
    for (method, path, body) in cases {
        let (code, text) = rest(&f.app, &f.buyer_session, method, path, body).await;
        assert_eq!(
            code,
            StatusCode::NOT_FOUND,
            "buyer {method} {path}: {code} {text}"
        );
    }
    assert_eq!(victim_snapshot(&f.db), before, "tenant B rows changed");
    assert!(
        f.db.list_plans(Some(-1)).is_empty(),
        "phantom plan created under seller -1"
    );
    assert!(
        f.db.list_codes(Some(-1), 100).is_empty(),
        "phantom code created under seller -1"
    );
}

/// A bound seller's scope is its own id, so equality with `-1` fails too.
/// Operator tiers resolve to `None` scope and are the deliberate exception.
#[tokio::test]
async fn sentinel_target_is_refused_for_bound_seller_too() {
    let f = make_fixture().await;
    assert!(
        f.db.get_seller(-1).is_none(),
        "phantom seller -1 exists before the request"
    );
    let (code, body) = rest(
        &f.app,
        &f.a_session,
        "POST",
        "/api/v1/sellers/-1/ai-settings",
        json!({"daily_token_budget": 999999999, "rpm_limit": 999999}),
    )
    .await;
    assert_eq!(
        code,
        StatusCode::FORBIDDEN,
        "bound seller A wrote seller -1 ai settings: {code} {body}"
    );
    assert!(
        f.db.get_seller(-1).is_none(),
        "phantom seller -1 was created by seller A"
    );

    // Control: the same bound seller may write its own settings, so the
    // refusal above is about the sentinel and not a broken endpoint.
    let own = format!("/api/v1/sellers/{A}/ai-settings");
    let (code, body) = rest(
        &f.app,
        &f.a_session,
        "POST",
        &own,
        json!({"daily_token_budget": 1}),
    )
    .await;
    assert_eq!(
        code,
        StatusCode::OK,
        "bound seller could not write own ai settings: {code} {body}"
    );
    assert!(
        f.db.get_seller(-1).is_none(),
        "phantom seller -1 appeared after the own-seller control"
    );
}

/// A buyer has no seller scope: zero and ordinary negative ids must be refused
/// like the `-1` sentinel, without opening a phantom namespace. Observed on
/// the merged router: the seller routes gate on scope first and answer 403;
/// `/api/v1/plans/0` looks the row up first and answers 404. Route and body
/// shapes follow tenant_isolation.rs.
#[tokio::test]
async fn zero_and_non_sentinel_negative_seller_target_ids_are_refused() {
    let f = make_fixture().await;
    let _s = seed(&f.db);
    let before = victim_snapshot(&f.db);

    let cases = [
        (
            "GET",
            "/api/v1/sellers/0",
            Value::Null,
            StatusCode::FORBIDDEN,
        ),
        (
            "GET",
            "/api/v1/sellers/-5",
            Value::Null,
            StatusCode::FORBIDDEN,
        ),
        ("GET", "/api/v1/plans/0", Value::Null, StatusCode::NOT_FOUND),
        (
            "POST",
            "/api/v1/sellers/0/ai-settings",
            json!({"daily_token_budget": 999999999}),
            StatusCode::FORBIDDEN,
        ),
        (
            "POST",
            "/api/v1/sellers/-5/ai-settings",
            json!({"daily_token_budget": 999999999}),
            StatusCode::FORBIDDEN,
        ),
    ];
    for (method, path, body, expected) in cases {
        let (code, text) = rest(&f.app, &f.buyer_session, method, path, body).await;
        assert_eq!(code, expected, "buyer {method} {path}: {code} {text}");
    }
    assert_eq!(victim_snapshot(&f.db), before, "tenant B rows changed");
    for id in [0_i64, -5] {
        assert!(f.db.get_seller(id).is_none(), "phantom seller {id} created");
        assert!(
            f.db.list_plans(Some(id)).is_empty(),
            "phantom plans under {id}"
        );
    }
}

/// The two transports resolve the same session and must land on the same
/// scope gate. A buyer is denied on every protected console page through both
/// the cookie and the bearer header; seller A and the owner are allowed through
/// both, so a matching denial cannot be an unrelated transport failure.
#[tokio::test]
async fn cookie_and_bearer_sessions_have_identical_scope_gates() {
    let f = make_fixture().await;
    let _s = seed(&f.db);
    let pages = ["/dashboard", "/plans", "/subs", "/customers"];
    for p in pages {
        let (cookie_code, cookie_body) = console(&f.app, &f.buyer_session, "GET", p, "").await;
        let (bearer_code, bearer_body) =
            bearer_console(&f.app, &f.buyer_session, "GET", p, "").await;
        assert_eq!(
            cookie_code,
            StatusCode::FORBIDDEN,
            "buyer cookie {p}: {cookie_code} {cookie_body}"
        );
        assert_eq!(
            bearer_code, cookie_code,
            "buyer bearer {p}: {bearer_code} {bearer_body}"
        );
        assert!(
            !cookie_body.contains(B_MARK) && !bearer_body.contains(B_MARK),
            "buyer {p} leaked tenant B through a transport"
        );
    }
    // Controls: a bound seller and an owner reach the same pages over both
    // transports. The owner's /plans view spans both tenants (tenant_isolation.rs
    // uses the same control), proving the buyer's denial is a scope gate.
    for p in ["/dashboard", "/plans", "/subs"] {
        let (cookie_code, cookie_body) = console(&f.app, &f.a_session, "GET", p, "").await;
        let (bearer_code, bearer_body) = bearer_console(&f.app, &f.a_session, "GET", p, "").await;
        assert_eq!(
            cookie_code,
            StatusCode::OK,
            "seller A cookie {p}: {cookie_code} {cookie_body}"
        );
        assert_eq!(
            bearer_code, cookie_code,
            "seller A bearer {p}: {bearer_code} {bearer_body}"
        );
        assert!(
            !cookie_body.contains(B_MARK) && !bearer_body.contains(B_MARK),
            "seller A {p} leaked tenant B"
        );
    }
    f.db.create_web_account("adv_owner", "local-pw", "owner", None)
        .await
        .unwrap();
    let (owner_acc, ..) = f.db.get_web_account_by_username("adv_owner").unwrap();
    let owner_session = f.db.create_web_session(owner_acc, "owner", None, 7);
    for p in ["/dashboard", "/plans", "/subs"] {
        let (cookie_code, cookie_body) = console(&f.app, &owner_session, "GET", p, "").await;
        let (bearer_code, bearer_body) = bearer_console(&f.app, &owner_session, "GET", p, "").await;
        assert_eq!(
            cookie_code,
            StatusCode::OK,
            "owner cookie {p}: {cookie_code} {cookie_body}"
        );
        assert_eq!(
            bearer_code, cookie_code,
            "owner bearer {p}: {bearer_code} {bearer_body}"
        );
        if p == "/plans" {
            assert!(
                cookie_body.contains("adv beta plan") && bearer_body.contains("adv beta plan"),
                "owner /plans lost the cross-tenant control: cookie={cookie_body} bearer={bearer_body}"
            );
        }
    }
}
