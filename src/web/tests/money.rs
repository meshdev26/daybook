// Web-side money safety surface (MONEY_SAFETY_SPEC §2/§3/§4/§5):
// owner + creator gates, guarded creation/transfer, reconciliation decisions,
// state banners and tenant scoping. No test here logs secrets or message text.
use super::*;
use crate::db::{ReconciliationRow, RefundInput};
use crate::handlers::money::{
    GuardedTransferOutcome, TransferOutcome, classify_transfer_error, execute_guarded_transfer,
    paid_ungranted_escalation_cutoff,
};
use axum::http::StatusCode;
use std::sync::Arc;
use tower::ServiceExt;

/// A second owner / an admin: web accounts are unique by username, so each
/// test names its own.
async fn named_session(
    state: &WebState,
    username: &str,
    role: &str,
    seller: Option<i64>,
) -> String {
    state
        .db
        .create_web_account(username, "secret", role, seller)
        .await
        .unwrap();
    let (id, _, _, _, _) = state
        .db
        .get_web_account_by_username(username)
        .expect("account exists");
    state.db.create_web_session(id, role, seller, 7)
}

fn location(resp: &axum::http::Response<axum::body::Body>) -> String {
    resp.headers()
        .get("location")
        .map(|v| v.to_str().unwrap_or_default().to_string())
        .unwrap_or_default()
}

fn system_refund(db: &Arc<crate::db::Db>, invoice: &str, seller: i64, amount: &str) -> i64 {
    db.create_refund_guarded(RefundInput {
        seller_id: seller,
        invoice_id: Some(invoice),
        user_id: Some(42),
        amount,
        asset: "USDT",
        reason: None,
        idem_key: &format!("webtest:{invoice}"),
        created_by: -1,
        created_role: "system",
    })
    .unwrap()
    .id
}

// ---- §2 owner-only + creator separation ----

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn refund_approve_execute_are_owner_only_and_creator_separated() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    db.save_invoice(
        "inv-a",
        7,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    db.save_invoice(
        "inv-b",
        7,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    let seller = seller_session(&state, 7).await;
    let admin = named_session(&state, "admin", "admin", None).await;
    let owner1 = owner_session(&state).await;
    let owner2 = named_session(&state, "owner-two", "owner", None).await;
    let app = router(state);

    // Seller creates inv-a's refund: the audit event records the real actor.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=inv-a&amount=2&asset=USDT",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?refund_created=1");
    let refund_a = db.list_refunds(Some(7))[0].0;

    // A seller and an admin cannot approve — even though the old web gate
    // accepted Admin+. Refusals are explicit 403 pages, not silent drops.
    for token in [&seller, &admin] {
        let resp = app
            .clone()
            .oneshot(post_authed_req(
                "/finance/refund/approve",
                &format!("id={refund_a}"),
                token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
    // A different owner can approve; the creator (seller) cannot.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/approve",
            &format!("id={refund_a}"),
            &owner2,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?refund_approved=1");
    assert_eq!(db.get_refund(refund_a).unwrap().7, "approved");

    // Execute has the same gates. The approved row now reaches the token
    // check, so a non-creator owner gets `transfer_unavailable` (no network).
    for token in [&seller, &admin] {
        let resp = app
            .clone()
            .oneshot(post_authed_req(
                "/finance/refund/execute",
                &format!("id={refund_a}"),
                token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/execute",
            &format!("id={refund_a}"),
            &owner2,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?error=transfer_unavailable");

    // An owner who CREATES a refund cannot approve it — the two persons are
    // creator and approver/executor, with no break-glass self-approval.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=inv-b&amount=2&asset=USDT",
            &owner1,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?refund_created=1");
    let refund_b = db
        .list_refunds(None)
        .into_iter()
        .find(|r| r.2.as_deref() == Some("inv-b"))
        .unwrap()
        .0;
    for path in ["/finance/refund/approve", "/finance/refund/execute"] {
        let resp = app
            .clone()
            .oneshot(post_authed_req(path, &format!("id={refund_b}"), &owner1))
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "owner creator on {path}"
        );
    }
    let resp = app
        .oneshot(post_authed_req(
            "/finance/refund/approve",
            &format!("id={refund_b}"),
            &owner2,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?refund_approved=1");
}

// ---- §1.2 guarded creation: real actor, real idem key, honest feedback ----

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn over_cap_refund_reports_error_instead_of_false_success() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    db.save_invoice(
        "cap-inv",
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

    // An admin/owner request is NOT pre-clamped, so the DB cap refuses it and
    // the banner must say so (the old code returned refund_created=1 anyway).
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=cap-inv&amount=99&asset=USDT",
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?error=refund_cap_exceeded");
    assert!(
        db.list_refunds(Some(7)).is_empty(),
        "no money row on refusal"
    );

    // The seller's over-cap request clamps to the invoice and is created once.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=cap-inv&amount=99&asset=USDT",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?refund_created=1");
    assert_eq!(db.list_refunds(Some(7))[0].4, "4");
    // A second, distinctly-keyed attempt breaches the cumulative cap.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=cap-inv&amount=99&asset=USDT",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?error=refund_cap_exceeded");
    assert_eq!(db.list_refunds(Some(7)).len(), 1);

    // Real actor + fresh idem key are persisted in the create audit event.
    let (seller_id, _, _, _, _) = db.get_web_account_by_username("seller").unwrap();
    let create_events: Vec<_> = db
        .list_audit_log(1000)
        .into_iter()
        .filter(|row| row.3 == "refund_create")
        .collect();
    let create = create_events
        .iter()
        .find(|row| row.5.as_deref() == Some(db.list_refunds(Some(7))[0].0.to_string().as_str()))
        .expect("create audit exists");
    assert_eq!(create.1, seller_id, "real actor, not a sentinel");
    let meta = create.6.as_deref().unwrap_or_default();
    assert!(
        meta.contains("\"idem_key\":\"web:"),
        "real idem key: {meta}"
    );

    // The error renders as a banner, not a success message.
    let resp = app
        .oneshot(authed_req("/finance?error=refund_cap_exceeded", &seller))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("cumulative refunds would exceed"), "{body}");
}

// ---- §4.2 one shared classifier; §4.1 persist-then-call discipline ----

#[test]
fn web_uses_the_shared_fail_closed_classifier() {
    for e in [
        "TRANSFER_FAILED",
        "insufficient funds",
        "HTTP 400 - bad request",
        "HTTP 422 - invalid",
    ] {
        assert_eq!(
            classify_transfer_error(e),
            TransferOutcome::DefiniteRejected,
            "{e}"
        );
    }
    for e in [
        "",
        "HTTP 408 - timeout",
        "HTTP 500 - gateway",
        "connection reset by peer",
        "some novel error shape",
    ] {
        assert_eq!(
            classify_transfer_error(e),
            TransferOutcome::Ambiguous,
            "{e}"
        );
    }
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn guarded_transfer_persists_spend_id_before_call_and_reuses_it() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    let first = execute_guarded_transfer(&db, "refund", 55, 7, None, |spend_id| {
        let db2 = db.clone();
        async move {
            assert!(
                db2.get_transfer_attempt(&spend_id).is_some(),
                "spend_id must be durable before the transport is called"
            );
            Err("operation timed out".to_string())
        }
    })
    .await
    .unwrap();
    let GuardedTransferOutcome::Ambiguous { spend_id, .. } = &first else {
        panic!("a timeout is ambiguous, never definite");
    };
    let persisted = spend_id.clone();
    assert_eq!(
        db.get_transfer_attempt(&persisted)
            .unwrap()
            .outcome
            .as_deref(),
        Some("ambiguous")
    );
    // Ambiguous reconciliation retries the SAME key: no second attempt row.
    let second = execute_guarded_transfer(
        &db,
        "refund",
        55,
        7,
        Some(spend_id.as_str()),
        |sid| async move { Ok(json!({"transfer_id": sid.len()})) },
    )
    .await
    .unwrap();
    let GuardedTransferOutcome::Succeeded {
        spend_id: reused, ..
    } = second
    else {
        panic!("reuse should succeed");
    };
    assert_eq!(reused, persisted);
    assert_eq!(
        db.latest_transfer_attempt("refund", 55).unwrap().attempt_no,
        1
    );
    // A key belonging to another money row is refused before any call.
    let wrong =
        execute_guarded_transfer(&db, "payout", 55, 7, Some(persisted.as_str()), |_| async {
            Ok(json!({}))
        })
        .await;
    assert!(wrong.is_err(), "cross-row spend_id reuse must fail closed");
}

// ---- §5 reconciliation form, mandatory reason, append-only resolution ----

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn reconciliation_form_requires_reason_and_is_owner_only() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    db.save_invoice(
        "rec-inv",
        7,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    let id = system_refund(&db, "rec-inv", 7, "2");
    db.set_refund_status(id, "approved");
    assert!(db.begin_refund_execution(id));
    db.update_refund_status(id, "needs_reconciliation");
    let owner = owner_session(&state).await;
    let seller = seller_session(&state, 7).await;
    let app = router(state);

    // Owner sees a no-JS form with a required reason and the digest panel.
    let resp = app
        .clone()
        .oneshot(authed_req("/finance", &owner))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("/finance/reconciliation/resolve"), "{body}");
    assert!(
        body.contains("name=\"reason\"") && body.contains("required"),
        "{body}"
    );
    assert!(body.contains("09:00 UTC digest"), "{body}");
    // A seller never sees the owner decision desk.
    let resp = app
        .clone()
        .oneshot(authed_req("/finance", &seller))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(!body.contains("/finance/reconciliation/resolve"), "{body}");

    let form = format!("row_type=refund&id={id}&outcome=void&reason=");
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/reconciliation/resolve",
            &form,
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?error=reason_required");
    assert_eq!(db.get_refund(id).unwrap().7, "needs_reconciliation");

    let form = format!("row_type=refund&id={id}&outcome=void&reason=receipt+missing");
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/reconciliation/resolve",
            &form,
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/reconciliation/resolve",
            &form,
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?reconciliation_resolved=1");
    assert_eq!(db.get_refund(id).unwrap().7, "void");
    assert_eq!(
        db.money_resolution("refund", id).unwrap().1,
        "receipt missing"
    );
    // Append-only: a second decision is rejected.
    let resp = app
        .oneshot(post_authed_req(
            "/finance/reconciliation/resolve",
            &form,
            &owner,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?error=reconcile_already");
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn manual_executed_settlement_blocks_the_creator() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    db.save_invoice(
        "settle-inv",
        7,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    let owner1 = owner_session(&state).await;
    let owner2 = named_session(&state, "owner-two", "owner", None).await;
    let app = router(state);

    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=settle-inv&amount=2&asset=USDT",
            &owner1,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?refund_created=1");
    let id = db
        .list_refunds(None)
        .into_iter()
        .find(|r| r.2.as_deref() == Some("settle-inv"))
        .unwrap()
        .0;
    db.set_refund_status(id, "approved");
    assert!(db.begin_refund_execution(id));
    db.update_refund_status(id, "needs_reconciliation");
    let form = format!("row_type=refund&id={id}&outcome=executed&reason=receipt+checked");
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/reconciliation/resolve",
            &form,
            &owner1,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let resp = app
        .oneshot(post_authed_req(
            "/finance/reconciliation/resolve",
            &form,
            &owner2,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?reconciliation_resolved=1");
    assert_eq!(db.get_refund(id).unwrap().7, "executed");
}

// ---- §3.5 / §5 banners render from state, not from query alone ----

#[test]
fn finance_state_banners_distinguish_blocking_from_advisory() {
    assert!(super::super::finance::finance_ungranted_banner(&[]).is_none());
    let banner = super::super::finance::finance_ungranted_banner(&["inv-1".to_string()]);
    assert!(banner.unwrap().contains("Blocking:"));

    assert!(super::super::finance::finance_reconciliation_banner(&[]).is_none());
    let row = |overdue: bool| ReconciliationRow {
        row_type: "refund".to_string(),
        id: 1,
        seller_id: 7,
        amount: "2".to_string(),
        asset: "USDT".to_string(),
        status: "needs_reconciliation".to_string(),
        since: "2026-11-20T08:00:00+00:00".to_string(),
        spend_id: None,
        overdue,
    };
    let young = super::super::finance::finance_reconciliation_banner(&[row(false)]).unwrap();
    assert!(young.contains("awaiting reconciliation") && !young.contains("Blocking:"));
    let late = super::super::finance::finance_reconciliation_banner(&[row(true)]).unwrap();
    assert!(late.contains("Blocking:"));
}

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn payout_hold_predicate_and_banner_match_the_72h_rule() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    db.save_invoice(
        "hold-inv",
        7,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    assert!(db.mark_invoice_paid_ungranted("hold-inv"));
    let seller = seller_session(&state, 7).await;
    let future = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    // Fresh rows must not be blocked; only rows past the injected cutoff are.
    assert!(!super::super::finance_actions::payout_hold_blocked(
        &db, 7, &past
    ));
    assert!(super::super::finance_actions::payout_hold_blocked(
        &db, 7, &future
    ));
    assert_eq!(
        db.unresolved_ungranted_invoices(7, &future),
        vec!["hold-inv".to_string()]
    );
    // The real handler uses now-72h, so a fresh row may still request a payout.
    let app = router(state);
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/payout/create",
            "amount=1&asset=USDT&destination=42",
            &seller,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?payout_created=1");

    // An operator seeing the hold error gets a blocking explanation banner.
    let resp = app
        .oneshot(authed_req("/finance?error=ungranted_hold", &seller))
        .await
        .unwrap();
    let body = body_string(resp).await;
    assert!(body.contains("unresolved ungranted liability"), "{body}");
}

// ---- §1.2 / §2 cross-tenant scoping ----

#[tokio::test]
#[tracing::instrument(skip_all)]
async fn finance_pages_and_actions_do_not_leak_across_sellers() {
    let (state, _dir) = make_state().await;
    let db = state.db.clone();
    db.save_invoice(
        "inv-seven",
        7,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(42),
        None,
    );
    db.save_invoice(
        "inv-eight",
        8,
        "5",
        "USDT",
        None,
        "paid",
        None,
        None,
        Some(43),
        None,
    );
    let seven_id = system_refund(&db, "inv-seven", 7, "2");
    let eight_id = system_refund(&db, "inv-eight", 8, "2");
    let seven = seller_session(&state, 7).await;
    let eight = seller_session_named(&state, 8, "seller-eight").await;
    let app = router(state);

    for (seller, mine, other) in [
        (&seven, "inv-seven", "inv-eight"),
        (&eight, "inv-eight", "inv-seven"),
    ] {
        let resp = app
            .clone()
            .oneshot(authed_req("/finance", seller))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_string(resp).await;
        assert!(body.contains(mine), "own refund must be visible: {body}");
        assert!(!body.contains(other), "other tenant leaked: {body}");
    }

    // Cross-tenant creation is refused by the invoice-owner check.
    let resp = app
        .clone()
        .oneshot(post_authed_req(
            "/finance/refund/create",
            "invoice_id=inv-eight&amount=1&asset=USDT",
            &seven,
        ))
        .await
        .unwrap();
    assert_eq!(location(&resp), "/finance?error=invoice_owner");

    // Even knowing the other tenant's refund id, a seller cannot act on it.
    for path in ["/finance/refund/approve", "/finance/refund/execute"] {
        let resp = app
            .clone()
            .oneshot(post_authed_req(path, &format!("id={eight_id}"), &seven))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }
    let resp = app
        .oneshot(post_authed_req(
            "/finance/reconciliation/resolve",
            &format!("row_type=refund&id={eight_id}&outcome=void&reason=x"),
            &seven,
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(db.get_refund(seven_id).unwrap().7, "pending");
    assert_eq!(db.get_refund(eight_id).unwrap().7, "pending");
    // Confirm the cutoff helper itself is tenant-scoped.
    let future = paid_ungranted_escalation_cutoff(chrono::Utc::now());
    assert!(db.unresolved_ungranted_invoices(7, &future).is_empty());
}
