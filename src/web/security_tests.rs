use super::tests::{
    authed_req, body_string, get_req, make_state, make_state_with_recovery_codes, owner_session,
    pending_get_req, pending_post_req, post_authed_req, seller_session,
};
use super::*;
use axum::http::StatusCode;
use std::sync::Arc;
use tower::ServiceExt;

// ---- Settings MFA re-auth (MED-6) ----

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_mfa_disable_requires_current_factor() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let (acc_id, _u, _h, _r, _s) = db
        .get_web_account_by_username("owner")
        .expect("owner account exists");
    const SECRET: &str = "JBSWY3DPEHPK3PXP";
    db.enable_totp(acc_id, SECRET).unwrap();
    let token = owner_session(&state).await;
    let app = router(state);
    // No code at all: refused, MFA stays on.
    let resp = app
        .clone()
        .oneshot(post_authed_req("/settings/mfa/disable", "code=", &token))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/settings?mfa=reauth"
    );
    assert!(
        db.get_totp_state(acc_id).1,
        "MFA must survive a rejected factor"
    );
    // A wrong code: same refusal.
    let now_ts = chrono::Utc::now().timestamp() as u64;
    let valid = crate::util::totp_generate(SECRET, now_ts, 6usize, 30u64);
    let wrong = format!("{:06}", (valid.parse::<u32>().unwrap_or(0) + 1) % 1_000_000);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/settings/mfa/disable",
            &format!("code={wrong}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/settings?mfa=reauth"
    );
    assert!(db.get_totp_state(acc_id).1);
    // The current TOTP: accepted, MFA is disabled. (Recomputed here so a
    // 30-second window rolling mid-test cannot flake the assertion.)
    let now_ts = chrono::Utc::now().timestamp() as u64;
    let valid = crate::util::totp_generate(SECRET, now_ts, 6usize, 30u64);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/settings/mfa/disable",
            &format!("code={valid}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/settings?mfa=disabled"
    );
    assert!(
        !db.get_totp_state(acc_id).1,
        "the current factor authorized the disable"
    );
    // The failure banner renders on the settings page.
    let resp = app
        .oneshot(authed_req("/settings?mfa=reauth", &token))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("That code was not accepted"),
        "reauth failure banner must render: {body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_mfa_disable_with_totp_when_recovery_codes_disabled() {
    let (state, _dir) = make_state_with_recovery_codes(false).await;
    let db = state.db.clone();
    let (acc_id, _u, _h, _r, _s) = db
        .get_web_account_by_username("owner")
        .expect("owner account exists");
    const SECRET: &str = "JBSWY3DPEHPK3PXP";
    db.enable_totp(acc_id, SECRET).unwrap();
    let token = owner_session(&state).await;
    let app = router(state);
    let now_ts = chrono::Utc::now().timestamp() as u64;
    let valid = crate::util::totp_generate(SECRET, now_ts, 6usize, 30u64);
    let resp = app
        .oneshot(post_authed_req(
            "/settings/mfa/disable",
            &format!("code={valid}"),
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/settings?mfa=disabled"
    );
    assert!(
        !db.get_totp_state(acc_id).1,
        "TOTP must work without recovery codes"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_mfa_recovery_requires_current_factor() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let (acc_id, _u, _h, _r, _s) = db
        .get_web_account_by_username("owner")
        .expect("owner account exists");
    db.enable_totp(acc_id, "JBSWY3DPEHPK3PXP").unwrap();
    let token = owner_session(&state).await;
    let app = router(state);
    // No code: no rotation (the old recovery codes stay untouched).
    let resp = app
        .oneshot(post_authed_req("/settings/mfa/recovery", "code=", &token))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/settings?mfa=reauth"
    );
    assert_eq!(
        db.list_unused_recovery_codes(acc_id),
        0,
        "no codes may be minted without the current factor"
    );
}

fn pending_row(
    db: &crate::db::Db,
    account_id: i64,
    purpose: &str,
    secret: Option<&str>,
    raw: &str,
) {
    let expires = (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339();
    db.create_pending_mfa(
        crate::web::auth_pages::sha256_hex(raw),
        account_id,
        purpose,
        secret,
        expires,
    )
    .unwrap();
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn mfa_enroll_omits_recovery_codes_when_disabled() {
    const SECRET: &str = "JBSWY3DPEHPK3PXP";
    const RAW: &str = "enroll-pending-off";
    let (state, _dir) = make_state_with_recovery_codes(false).await;
    let db = state.db.clone();
    let (acc_id, _, _, _, _) = db.get_web_account_by_username("owner").unwrap();
    pending_row(&db, acc_id, "enroll", Some(SECRET), RAW);
    let now = chrono::Utc::now().timestamp() as u64;
    let code = crate::util::totp_generate(SECRET, now, 6usize, 30u64);
    let app = router(state);
    let resp = app
        .oneshot(pending_post_req(
            "/mfa/enroll",
            &format!("code={code}"),
            RAW,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("MFA enabled"), "{body}");
    assert!(body.contains("Continue to dashboard"), "{body}");
    assert!(!body.contains("recovery-codes"), "{body}");
    assert!(!body.contains("wallet seed"), "{body}");
    assert!(db.get_totp_state(acc_id).1, "TOTP must still be enabled");
    assert_eq!(db.list_unused_recovery_codes(acc_id), 0);
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn mfa_enroll_shows_recovery_codes_when_enabled() {
    const SECRET: &str = "JBSWY3DPEHPK3PXP";
    const RAW: &str = "enroll-pending-on";
    let (state, _dir) = make_state_with_recovery_codes(true).await;
    let db = state.db.clone();
    let (acc_id, _, _, _, _) = db.get_web_account_by_username("owner").unwrap();
    pending_row(&db, acc_id, "enroll", Some(SECRET), RAW);
    let now = chrono::Utc::now().timestamp() as u64;
    let code = crate::util::totp_generate(SECRET, now, 6usize, 30u64);
    let app = router(state);
    let resp = app
        .oneshot(pending_post_req(
            "/mfa/enroll",
            &format!("code={code}"),
            RAW,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("recovery-codes"), "{body}");
    assert!(body.contains("wallet seed"), "{body}");
    assert_eq!(db.list_unused_recovery_codes(acc_id), 10);
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn mfa_verify_rejects_stored_recovery_code_when_disabled() {
    const SECRET: &str = "JBSWY3DPEHPK3PXP";
    const RAW: &str = "verify-pending-flag";
    const PLAIN: &str = "alpha-bravo-charlie-delta";
    let (mut state, _dir) = make_state_with_recovery_codes(false).await;
    let db = state.db.clone();
    let (acc_id, _, _, _, _) = db.get_web_account_by_username("owner").unwrap();
    db.enable_totp(acc_id, SECRET).unwrap();
    let hashed = bcrypt::hash(PLAIN, bcrypt::DEFAULT_COST).unwrap_or_default();
    db.create_recovery_codes(acc_id, [hashed]).unwrap();
    pending_row(&db, acc_id, "verify", None, RAW);
    let app = router(state.clone());
    let resp = app
        .oneshot(pending_post_req(
            "/mfa/verify",
            &format!("code={PLAIN}"),
            RAW,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("Invalid code"), "{body}");
    assert_eq!(
        db.list_unused_recovery_codes(acc_id),
        1,
        "code stays dormant"
    );

    state.cfg.mfa_recovery_codes_enabled = true;
    let app = router(state);
    let resp = app
        .oneshot(pending_post_req(
            "/mfa/verify",
            &format!("code={PLAIN}"),
            RAW,
        ))
        .await
        .unwrap();
    assert_eq!(resp.headers().get("location").unwrap(), "/dashboard");
    assert_eq!(
        db.list_unused_recovery_codes(acc_id),
        0,
        "code revives when re-enabled"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn mfa_verify_page_label_tracks_flag() {
    let (state_off, _dir_off) = make_state_with_recovery_codes(false).await;
    let db_off = state_off.db.clone();
    let (acc_off, _, _, _, _) = db_off.get_web_account_by_username("owner").unwrap();
    pending_row(&db_off, acc_off, "verify", None, "page-off");
    let app = router(state_off);
    let resp = app
        .oneshot(pending_get_req("/mfa/verify", "page-off"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains(">Authenticator code<"), "{body}");
    assert!(!body.contains("or recovery code"), "{body}");

    let (state_on, _dir_on) = make_state_with_recovery_codes(true).await;
    let db_on = state_on.db.clone();
    let (acc_on, _, _, _, _) = db_on.get_web_account_by_username("owner").unwrap();
    pending_row(&db_on, acc_on, "verify", None, "page-on");
    let app = router(state_on);
    let resp = app
        .oneshot(pending_get_req("/mfa/verify", "page-on"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        body.contains("Authenticator code or recovery code"),
        "{body}"
    );
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_mfa_recovery_route_refused_when_disabled() {
    let (state, _dir) = make_state_with_recovery_codes(false).await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/settings/mfa/recovery",
            "code=123456",
            &token,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    for path in ["/mfa/verify", "/mfa/enroll"] {
        let resp = app.clone().oneshot(get_req(path)).await.unwrap();
        assert_ne!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "{path} must stay live"
        );
    }
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn settings_mfa_panel_hides_recovery_ui_when_disabled() {
    let (state, _dir) = make_state_with_recovery_codes(false).await;
    let token = owner_session(&state).await;
    let app = router(state);
    let resp = app.oneshot(authed_req("/settings", &token)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("Multi-factor authentication"), "{body}");
    assert!(body.contains("Enroll / Re-enroll MFA"), "{body}");
    assert!(body.contains("Disable MFA"), "{body}");
    assert!(!body.contains("Unused recovery codes"), "{body}");
    assert!(!body.contains("Regenerate recovery codes"), "{body}");
    assert!(!body.contains("wallet seed"), "{body}");
}

// ---- Refund create seller clamp (MED-7 mirror) ----

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn finance_refund_create_seller_clamped_to_invoice() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    // A paid invoice for seller 7: 10.5 USDT, buyer user 42; a second
    // invoice exercises the admin path.
    db.save_invoice(
        "inv-1",
        7,
        "10.5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    db.save_invoice(
        "inv-2",
        7,
        "4",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    let seller = seller_session(&state, 7).await;
    let owner = owner_session(&state).await;
    let app = router(state);
    refund_seller_flow(app.clone(), seller, db.clone()).await;
    refund_owner_flow(app, owner, db).await;
}

async fn refund_seller_flow(app: Router, seller: String, db: Arc<crate::db::Db>) {
    // A seller may not pick a different asset: banner error, no create.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=inv-1&amount=99&asset=TON",
            &seller,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/finance?error=invalid_asset"
    );
    assert!(db.list_refunds(Some(7)).is_empty());
    // A seller may not ask for more than the invoice collected: clamped.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=inv-1&amount=99&asset=USDT",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/finance?refund_created=1"
    );
    let refunds = db.list_refunds(Some(7));
    assert_eq!(refunds.len(), 1);
    assert_eq!(refunds[0].4, "10.5", "amount must clamp to the invoice");
    assert_eq!(refunds[0].5, "USDT");
    // A non-numeric amount: banner error, no second row.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=inv-1&amount=abc&asset=USDT",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/finance?error=amount_number"
    );
    // A non-positive amount: banner error too.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=inv-1&amount=0&asset=USDT",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/finance?error=amount_positive"
    );
    assert_eq!(db.list_refunds(Some(7)).len(), 1, "only the clamped row");
}

async fn refund_owner_flow(app: Router, owner: String, db: Arc<crate::db::Db>) {
    // Admins/owner keep the looser web form (they approve what gets created),
    // but the data layer enforces the cumulative cap for everyone: an
    // over-cap request must never persist a money row, even for the owner.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=inv-2&amount=99&asset=USDT",
            &owner,
        ))
        .await
        .unwrap();
    assert!(resp.status().is_redirection());
    let all = db.list_refunds(None);
    assert!(
        !all.iter().any(|r| r.2.as_deref() == Some("inv-2")),
        "an over-cap refund must not be persisted, even for the owner: {all:?}"
    );
    // The owner is still authorised to refund another seller's invoice with
    // a valid (≤ invoice) amount — the gating intent of this test. A partial
    // amount is honoured verbatim (the web form does not clamp admins).
    let resp = app
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=inv-2&amount=2&asset=USDT",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(
        resp.headers().get("location").unwrap(),
        "/finance?refund_created=1"
    );
    let all = db.list_refunds(None);
    assert!(
        all.iter()
            .any(|r| r.2.as_deref() == Some("inv-2") && r.4 == "2" && r.5 == "USDT"),
        "authorised owner refund must be persisted: {all:?}"
    );
}

// ---- Login throttle hard cap (LOW-10) ----

#[test]
fn login_throttle_hard_cap_bounds_the_map() {
    let map: Arc<StdMutex<HashMap<String, (u32, Instant)>>> =
        Arc::new(StdMutex::new(HashMap::new()));
    // A flood of single-attempt keys inside the hour window: the time
    // based trim cannot help, the oldest-eviction cap must.
    for i in 0..2000 {
        login_throttle(&map, &format!("flood-{i}"), 5, 900);
    }
    let len = map.lock().unwrap().len();
    assert!(
        len <= 1025,
        "the map must stay bounded at the 1024 cap, got {len}"
    );
    // The newest key must still be tracked after the evictions: its
    // counter survived (it was among the last inserted) and keeps
    // counting toward the lockout (1 from the flood + 4 more = 5).
    let mut throttled = false;
    for _ in 0..4 {
        throttled = login_throttle(&map, "flood-1999", 5, 900);
    }
    assert!(
        throttled,
        "recent failures keep counting toward the lockout"
    );
}

// ---- CryptoPay transfer error classification (BLOCK-2 / MONEY_SAFETY §4.2) ----

#[test]
fn transfer_rejection_classification() {
    // The web no longer owns a second classifier: REST, bot and web all use
    // the shared fail-closed enum. Only positively recognized API refusals
    // are definite; everything else is Ambiguous.
    use crate::handlers::money::{TransferOutcome, classify_transfer_error};
    let definite = |e: &str| {
        assert_eq!(
            classify_transfer_error(e),
            TransferOutcome::DefiniteRejected,
            "{e}"
        )
    };
    let ambiguous = |e: &str| {
        assert_eq!(
            classify_transfer_error(e),
            TransferOutcome::Ambiguous,
            "{e}"
        )
    };
    // Parsed ok:false from the API: definite refusal, no money moved.
    definite("INSUFFICIENT_FUNDS");
    definite("unknown error");
    // 4xx from the API: the request was seen and rejected.
    definite("HTTP 400 Bad Request - {\"ok\":false}");
    definite("HTTP 401 Unauthorized - {}");
    // 5xx and 408: an intermediary may have dropped a processed transfer.
    ambiguous("HTTP 502 Bad Gateway - proxy");
    ambiguous("HTTP 504 Gateway Timeout - proxy");
    ambiguous("HTTP 408 Request Timeout - {}");
    // reqwest transport shapes (connect/timeout/body/decode): ambiguous.
    ambiguous(
        "error sending request for url (https://pay.crypt.bot/api/transfer): operation timed out",
    );
    ambiguous("error decoding response body");
    ambiguous("error reading a body from connection");
    ambiguous("builder error");
    // A novel error shape must default to Ambiguous, never definite.
    ambiguous("a brand new provider error we did not anticipate");
}
