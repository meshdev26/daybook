use super::*;
use crate::db::{ReconcileError, ReconciliationOutcome, RefundCreateError, RefundInput};
use crate::handlers::money::{
    GuardedTransferOutcome, execute_guarded_transfer, fresh_idem_key, is_full_refund,
    paid_ungranted_escalation_cutoff,
};

/// §2: money approvals, executions and manual settlements require the exact
/// Owner role — `has_level(Owner)` is the same check but keeps the intent
/// explicit at every money call site.
fn is_owner(session: &Session) -> bool {
    Role::from_role_str(&session.role) == Role::Owner
}

/// §2 separation of duties: the human that created a refund may never approve,
/// execute or manually settle their own row. System-created refunds have no
/// creator audit event and are exempt (they still need an Owner).
fn creator_owns_refund(state: &WebState, session: &Session, refund_id: i64) -> bool {
    crate::handlers::money::creator_owns_refund(&state.db, session.account_id, refund_id)
}

/// §3.5 data predicate used by the payout-create hold. Kept as a small
/// injectable-cutoff helper so the 72h boundary is testable without sleeping;
/// the handler always passes the real `now - 72h` cutoff.
pub(super) fn payout_hold_blocked(
    db: &crate::db::Db,
    seller_id: i64,
    escalation_cutoff: &str,
) -> bool {
    !db.unresolved_ungranted_invoices(seller_id, escalation_cutoff)
        .is_empty()
}

#[derive(Deserialize)]
pub(super) struct FinancePayoutCreateForm {
    pub(super) seller_id: Option<String>,
    pub(super) amount: Option<String>,
    pub(super) asset: Option<String>,
    pub(super) destination: Option<String>,
}

/// Mirrors REST `finance_payouts_create`: sellers create payouts for
/// themselves; admins must name the seller. Audit: `payout_create`.
#[tracing::instrument(skip_all)]
pub(super) async fn finance_payout_create(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinancePayoutCreateForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if Role::from_role_str(&session.role) != Role::Seller
        && !Role::from_role_str(&session.role).has_level(Role::Admin)
    {
        return forbidden_page(&state, "Finance", "Finance").into_response();
    }
    let requested = f
        .seller_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse::<i64>().ok());
    let seller_id = if Role::from_role_str(&session.role) == Role::Seller {
        match session.seller_id {
            Some(s) => {
                if requested.is_some() && requested != Some(s) {
                    // The REST forbidden case: a seller naming another seller.
                    return forbidden_page(&state, "Finance", "Finance").into_response();
                }
                s
            }
            None => return forbidden_page(&state, "Finance", "Finance").into_response(),
        }
    } else {
        match requested {
            Some(s) if s > 0 => s,
            _ => return finance_redirect(&state, "error=seller_required"),
        }
    };
    // §3.5: a paid-but-ungranted invoice older than 72h whose auto-refund has
    // not executed is an unresolved liability. New payouts are refused until
    // it is resolved; this is an internal operational target, not an SLA.
    if payout_hold_blocked(
        &state.db,
        seller_id,
        &paid_ungranted_escalation_cutoff(chrono::Utc::now()),
    ) {
        return finance_redirect(&state, "error=ungranted_hold");
    }
    let amount = f.amount.as_deref().map(str::trim).unwrap_or_default();
    let asset = f.asset.as_deref().map(str::trim).unwrap_or_default();
    if amount.is_empty() || asset.is_empty() {
        return finance_redirect(&state, "error=amount_required");
    }
    let destination = f
        .destination
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(|d| d.to_string());
    let id = state
        .db
        .create_payout(seller_id, amount, asset, destination.as_deref());
    let meta = json!({
        "seller_id": seller_id, "amount": amount, "asset": asset,
        "destination": destination,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "payout_create",
        "payout",
        id.to_string(),
        Some(&meta),
    );
    finance_redirect(&state, "payout_created=1")
}

#[derive(Deserialize)]
pub(super) struct FinancePayoutActionForm {
    pub(super) id: String,
}

/// Mirrors REST `finance_payouts_approve`: owner only (two-person money
/// rule), paid is already executed, otherwise pending/approved -> approved.
/// Audit: `payout_approve`.
#[tracing::instrument(skip_all)]
pub(super) async fn finance_payout_approve(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinancePayoutActionForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Finance", "Payout approval (owner only)").into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return finance_redirect(&state, "error=invalid");
    };
    if id <= 0 {
        return finance_redirect(&state, "error=invalid");
    }
    let Some(payout) = state.db.get_payout(id.to_string()) else {
        return finance_redirect(&state, "error=notfound");
    };
    let (_pid, _seller, amount, asset, destination, status, ..) = payout;
    if status == "paid" {
        return finance_redirect(
            &state,
            &format!("error=state&detail={}", qenc("payout already executed")),
        );
    }
    if status != "pending" && status != "approved" {
        return finance_redirect(
            &state,
            &format!(
                "error=state&detail={}",
                qenc(&format!("cannot approve payout in status '{}'", status))
            ),
        );
    }
    state.db.set_payout_status(id, "approved");
    let meta = json!({
        "amount": amount, "asset": asset, "destination": destination,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "payout_approve",
        "payout",
        id.to_string(),
        Some(&meta),
    );
    finance_redirect(&state, "payout_approved=1")
}

/// Mirrors REST `finance_payouts_execute` (reserve-then-transfer): owner
/// only; approved -> paid by a real Crypto Pay `transfer` to the destination
/// Telegram user id. The row is first claimed 'executing' atomically, so two
/// concurrent executors can never both move money — the CAS loser is bounced
/// with an `executing` banner before any transfer is attempted. A definite
/// API rejection reverts the row to 'approved' for retry and is audited
/// (`payout_execute_failed`); an ambiguous transport outcome (timeout,
/// network) leaves the row 'executing' — the transfer state is unknown and
/// retrying blind risks a double payout — and is audited
/// (`payout_execute_ambiguous`) with its own banner.
#[allow(clippy::too_many_arguments)]
async fn finance_payout_execute_run(
    state: &WebState,
    session: &Session,
    id: i64,
    seller_id: i64,
    amount: String,
    asset: String,
    dest_user: i64,
    tok: String,
) -> Response {
    if !state.db.begin_payout_execution(id) {
        // Lost the atomic claim: another executor holds the row.
        return finance_redirect(state, "error=executing");
    }
    // §4.1: no raw transfer() here. The shared helper persists a fresh
    // spend_id BEFORE dispatch and passes it to transfer_with_spend_id; its
    // fail-closed classifier is the single source of truth for the outcome.
    let cp = crate::crypto::CryptoPay::new(tok);
    let outcome =
        match execute_guarded_transfer(&state.db, "payout", id, seller_id, None, |spend_id| {
            let cp = cp;
            let asset = asset.clone();
            let amount = amount.clone();
            async move {
                cp.transfer_with_spend_id(dest_user, &asset, &amount, &spend_id)
                    .await
            }
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                return finance_redirect(
                    state,
                    &format!(
                        "error=transfer_not_attempted&detail={}",
                        qenc(e.chars().take(200).collect::<String>().as_str())
                    ),
                );
            }
        };
    match outcome {
        GuardedTransferOutcome::Succeeded { spend_id, receipt } => {
            // We won the claim, so the executing -> paid finish is ours alone.
            state.db.finish_payout_execution(id, "paid");
            let meta = json!({
                "seller_id": seller_id, "amount": amount, "asset": asset,
                "destination": dest_user, "spend_id": spend_id, "receipt": receipt,
            })
            .to_string();
            state.db.log_action(
                session.account_id,
                &session.role,
                "payout_executed",
                "payout",
                id.to_string(),
                Some(&meta),
            );
            finance_redirect(state, "payout_executed=1")
        }
        GuardedTransferOutcome::DefiniteRejected { spend_id, error } => {
            // API refused: no money moved. Revert to approved; a retry mints a
            // fresh spend_id/attempt (§4.1).
            state.db.finish_payout_execution(id, "approved");
            let meta = json!({
                "seller_id": seller_id, "amount": amount, "asset": asset,
                "destination": dest_user, "spend_id": spend_id, "error": error,
            })
            .to_string();
            state.db.log_action(
                session.account_id,
                &session.role,
                "payout_execute_failed",
                "payout",
                id.to_string(),
                Some(&meta),
            );
            finance_redirect(
                state,
                &format!(
                    "error=transfer&detail={}",
                    qenc(error.chars().take(200).collect::<String>().as_str())
                ),
            )
        }
        GuardedTransferOutcome::Ambiguous { spend_id, error } => {
            // Unknown outcome: keep the row 'executing' with its persisted
            // spend_id for the §5 reconciliation flow; never retry blind.
            let meta = json!({
                "seller_id": seller_id, "amount": amount, "asset": asset,
                "destination": dest_user, "spend_id": spend_id, "error": error,
            })
            .to_string();
            state.db.log_action(
                session.account_id,
                &session.role,
                "payout_execute_ambiguous",
                "payout",
                id.to_string(),
                Some(&meta),
            );
            finance_redirect(
                state,
                &format!(
                    "error=ambiguous&detail={}",
                    qenc(error.chars().take(200).collect::<String>().as_str())
                ),
            )
        }
    }
}

#[tracing::instrument(skip_all)]
pub(super) async fn finance_payout_execute(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinancePayoutActionForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !Role::from_role_str(&session.role).has_level(Role::Owner) {
        return forbidden_page(&state, "Finance", "Payout execution (owner only)").into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return finance_redirect(&state, "error=invalid");
    };
    if id <= 0 {
        return finance_redirect(&state, "error=invalid");
    }
    let Some(payout) = state.db.get_payout(id.to_string()) else {
        return finance_redirect(&state, "error=notfound");
    };
    let (_pid, seller_id, amount, asset, destination, status, ..) = payout;
    if status == "paid" {
        return finance_redirect(
            &state,
            &format!("error=state&detail={}", qenc("payout already executed")),
        );
    }
    if status == "executing" {
        return finance_redirect(&state, "error=executing");
    }
    if status != "approved" {
        return finance_redirect(
            &state,
            &format!(
                "error=state&detail={}",
                qenc("payout must be approved before execution")
            ),
        );
    }
    // Crypto Pay transfers go to a Telegram user id: the destination must be
    // exactly that.
    let Some(dest) = destination
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    else {
        return finance_redirect(&state, "error=destination");
    };
    let Ok(dest_user) = dest.parse::<i64>() else {
        return finance_redirect(&state, "error=bad_destination");
    };
    let Some(tok) = crypto_token(&state) else {
        return finance_redirect(&state, "error=transfer_unavailable");
    };
    // Audit the attempt BEFORE the await: if the process dies mid-transfer,
    // the audit trail must already show that an execution was started.
    let attempt_meta = json!({
        "seller_id": seller_id, "amount": amount.clone(), "asset": asset.clone(),
        "destination": dest_user,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "payout_execute_attempt",
        "payout",
        id.to_string(),
        Some(&attempt_meta),
    );
    finance_payout_execute_run(
        &state, &session, id, seller_id, amount, asset, dest_user, tok,
    )
    .await
}

#[derive(Deserialize)]
pub(super) struct FinanceRefundCreateForm {
    pub(super) invoice_id: Option<String>,
    pub(super) amount: Option<String>,
    pub(super) asset: Option<String>,
    pub(super) reason: Option<String>,
}

/// Mirrors REST `finance_refunds_create`: seller-or-admin, rooted in a paid
/// invoice (the seller's own; admins may refund any seller's invoice and it
/// inherits that seller). Creation always goes through `create_refund_guarded`
/// with the REAL actor and a fresh idempotency key — a DB refusal (cumulative
/// cap, in-flight row, unquantizable legacy amount) is reported as an error
/// banner, never as `refund_created=1`. The asset lock applies to every role;
/// the seller's amount is clamped to the invoice, while admin/owner requests
/// are checked by the authoritative DB cap. Audit: `refund_create` (only for
/// a newly created row, so an idem replay is not a second money event).
#[tracing::instrument(skip_all)]
pub(super) async fn finance_refund_create(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinanceRefundCreateForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if Role::from_role_str(&session.role) != Role::Seller
        && !Role::from_role_str(&session.role).has_level(Role::Admin)
    {
        return forbidden_page(&state, "Finance", "Finance").into_response();
    }
    let invoice_id = f.invoice_id.as_deref().map(str::trim).unwrap_or_default();
    if invoice_id.is_empty() {
        return finance_redirect(&state, "error=invoice_required");
    }
    let Some(inv) = state.db.get_invoice(invoice_id) else {
        return finance_redirect(&state, "error=invoice_notfound");
    };
    let (inv_amount, inv_asset, inv_status, _desc, _plan, inv_user, inv_seller) = inv;
    if Role::from_role_str(&session.role) == Role::Seller && session.seller_id != Some(inv_seller) {
        return finance_redirect(&state, "error=invoice_owner");
    }
    // Only settled money can be returned. `paid_ungranted` is paid money in
    // a non-terminal recovery state (§3), so it may also be refunded.
    if inv_status != "paid" && inv_status != "paid_ungranted" {
        return finance_redirect(&state, "error=invoice_state");
    }
    // §1.2 asset lock applies to EVERY role, including admin and owner: a
    // refund in a different asset is not a return of the buyer's money.
    // An empty field means "use the invoice's asset" and is allowed.
    if f.asset
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .is_some_and(|a| a != inv_asset)
    {
        return finance_redirect(&state, "error=invalid_asset");
    }
    let inv_amount_f: f64 = inv_amount.parse().unwrap_or(f64::NAN);
    let amount = match f.amount.as_deref().map(str::trim) {
        None | Some("") => inv_amount.clone(),
        Some(raw) => {
            if Role::from_role_str(&session.role) != Role::Seller {
                raw.to_string()
            } else {
                let Ok(requested) = raw.parse::<f64>() else {
                    return finance_redirect(&state, "error=amount_number");
                };
                if requested <= 0.0 {
                    return finance_redirect(&state, "error=amount_positive");
                }
                if inv_amount_f.is_nan() || requested > inv_amount_f {
                    // Clamp: never more than the invoice collected (and if
                    // the stored amount is not parseable, refund exactly
                    // that amount verbatim rather than trusting the request).
                    inv_amount.clone()
                } else {
                    raw.to_string()
                }
            }
        }
    };
    let asset = f
        .asset
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or(inv_asset);
    let reason = f
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let idem_key = match fresh_idem_key("web") {
        Ok(k) => k,
        Err(e) => {
            return finance_redirect(
                &state,
                &format!(
                    "error=idem_unavailable&detail={}",
                    qenc(e.chars().take(200).collect::<String>().as_str())
                ),
            );
        }
    };
    let role_str = session.role.as_str();
    let creation = match state.db.create_refund_guarded(RefundInput {
        seller_id: inv_seller,
        invoice_id: Some(invoice_id),
        user_id: inv_user,
        amount: &amount,
        asset: &asset,
        reason: reason.as_deref(),
        idem_key: &idem_key,
        created_by: session.account_id,
        created_role: role_str,
    }) {
        Ok(creation) => creation,
        Err(e) => {
            let target = match e {
                RefundCreateError::InvoiceNotFound => "error=invoice_notfound",
                RefundCreateError::InvoiceNotPaid => "error=invoice_state",
                RefundCreateError::AssetMismatch => "error=invalid_asset",
                RefundCreateError::UnquantizableLegacy => "error=unquantifiable_legacy",
                RefundCreateError::CapExceeded => "error=refund_cap_exceeded",
                RefundCreateError::InFlight => "error=refund_in_flight",
                RefundCreateError::Database(err) => {
                    tracing::error!("refund creation failed: {}", err);
                    "error=refund_create_failed"
                }
            };
            return finance_redirect(&state, target);
        }
    };
    // An idem replay is the SAME money movement: never emit a second audit
    // event (§1.2 step 5), but it is still a successful create for the user.
    if creation.created_new {
        let meta = json!({
            "invoice_id": invoice_id, "amount": amount, "asset": asset,
            "user_id": inv_user, "reason": reason, "idem_key": idem_key,
        })
        .to_string();
        state.db.log_action(
            session.account_id,
            &session.role,
            "refund_create",
            "refund",
            creation.id.to_string(),
            Some(&meta),
        );
    }
    finance_redirect(&state, "refund_created=1")
}

#[derive(Deserialize)]
pub(super) struct FinanceRefundActionForm {
    pub(super) id: String,
}

/// Mirrors REST `finance_refunds_approve`: §2 Owner-only with creator
/// separation, executed/processed is final, otherwise pending/approved ->
/// approved. Audit: `refund_approve`.
#[tracing::instrument(skip_all)]
pub(super) async fn finance_refund_approve(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinanceRefundActionForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !is_owner(&session) {
        return forbidden_page(&state, "Finance", "Refund approval (owner only)").into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return finance_redirect(&state, "error=invalid");
    };
    if id <= 0 {
        return finance_redirect(&state, "error=invalid");
    }
    if creator_owns_refund(&state, &session, id) {
        return forbidden_page(
            &state,
            "Finance",
            "Refund approval (creator cannot approve own refund)",
        )
        .into_response();
    }
    let Some(refund) = state.db.get_refund(id.to_string()) else {
        return finance_redirect(&state, "error=notfound");
    };
    let (_rid, _seller, invoice_id, _user, amount, asset, _reason, status, ..) = refund;
    if status == "executed" || status == "processed" {
        return finance_redirect(
            &state,
            &format!("error=state&detail={}", qenc("refund already executed")),
        );
    }
    if status != "pending" && status != "approved" {
        return finance_redirect(
            &state,
            &format!(
                "error=state&detail={}",
                qenc(&format!("cannot approve refund in status '{}'", status))
            ),
        );
    }
    state.db.set_refund_status(id, "approved");
    let meta = json!({
        "invoice_id": invoice_id, "amount": amount, "asset": asset,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "refund_approve",
        "refund",
        id.to_string(),
        Some(&meta),
    );
    finance_redirect(&state, "refund_approved=1")
}

/// Mirrors REST `finance_refunds_execute` (reserve-then-transfer): §2
/// Owner-only with creator separation; approved -> executed by a real Crypto
/// Pay `transfer` back to the buyer. The row is first claimed 'executing'
/// atomically, so two concurrent executors can never both move money — the
/// CAS loser is bounced with an `executing` banner before any transfer is
/// attempted. A definite API rejection reverts the row to 'approved' for
/// retry and is audited (`refund_execute_failed`); an ambiguous transport
/// outcome (timeout, network) leaves the row 'executing' — the transfer state
/// is unknown and retrying blind risks a double payout — and is audited
/// (`refund_execute_ambiguous`) with its own banner. Every transfer goes
/// through the shared persist-then-call helper with a fresh spend_id (§4.1).
#[tracing::instrument(skip_all)]
pub(super) async fn finance_refund_execute(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinanceRefundActionForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !is_owner(&session) {
        return forbidden_page(&state, "Finance", "Refund execution (owner only)").into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return finance_redirect(&state, "error=invalid");
    };
    if id <= 0 {
        return finance_redirect(&state, "error=invalid");
    }
    if creator_owns_refund(&state, &session, id) {
        return forbidden_page(
            &state,
            "Finance",
            "Refund execution (creator cannot execute own refund)",
        )
        .into_response();
    }
    let Some(refund) = state.db.get_refund(id.to_string()) else {
        return finance_redirect(&state, "error=notfound");
    };
    let (_rid, seller_id, invoice_id, refund_user, amount, asset, _reason, status, ..) = refund;
    if status == "executed" || status == "processed" {
        return finance_redirect(
            &state,
            &format!("error=state&detail={}", qenc("refund already executed")),
        );
    }
    if status == "executing" {
        return finance_redirect(&state, "error=executing");
    }
    if status != "approved" {
        return finance_redirect(
            &state,
            &format!(
                "error=state&detail={}",
                qenc("refund must be approved before execution")
            ),
        );
    }
    // Crypto Pay transfers target a Telegram user: the buyer the invoice was
    // bound to.
    let Some(refund_user) = refund_user else {
        return finance_redirect(&state, "error=no_user");
    };
    let Some(tok) = crypto_token(&state) else {
        return finance_redirect(&state, "error=transfer_unavailable");
    };
    // Audit the attempt BEFORE the await: if the process dies mid-transfer,
    // the audit trail must already show that an execution was started.
    let attempt_meta = json!({
        "invoice_id": invoice_id.clone(), "amount": amount.clone(), "asset": asset.clone(),
        "user_id": refund_user,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "refund_execute_attempt",
        "refund",
        id.to_string(),
        Some(&attempt_meta),
    );
    finance_refund_execute_run(
        &state,
        &session,
        id,
        seller_id,
        invoice_id,
        refund_user,
        amount,
        asset,
        tok,
    )
    .await
}

async fn finance_refund_finalize(
    state: &WebState,
    seller_id: i64,
    invoice_id: Option<String>,
    refund_user: i64,
) -> (bool, bool) {
    let mut subscription_revoked = false;
    let mut kick_failed = false;
    if let Some(inv) = invoice_id.as_deref() {
        // §1.3: only a FULL refund settles the invoice and revokes access.
        // A partial refund leaves the invoice paid with no kick; this is the
        // same read-only decision the server's REST execute uses.
        if !is_full_refund(&state.db, inv) {
            return (false, false);
        }
        state.db.update_invoice_status(inv, "refunded");
        if let Some((sub_id, channel)) = state.db.subscription_by_invoice(inv) {
            state.db.set_subscription_status(sub_id, "refunded");
            subscription_revoked = true;
            if !channel.is_empty() {
                let kick_bot = state.bots.for_seller(seller_id);
                let cid = crate::util::to_chat_id(&channel);
                let ban = kick_bot
                    .bot
                    .ban_chat_member(cid.clone(), teloxide::types::UserId(refund_user as u64))
                    .await;
                let unban = kick_bot
                    .bot
                    .unban_chat_member(cid, teloxide::types::UserId(refund_user as u64))
                    .await;
                kick_failed = ban.is_err() || unban.is_err();
            }
        }
    }
    (subscription_revoked, kick_failed)
}

#[allow(clippy::too_many_arguments)]
async fn finance_refund_execute_run(
    state: &WebState,
    session: &Session,
    id: i64,
    seller_id: i64,
    invoice_id: Option<String>,
    refund_user: i64,
    amount: String,
    asset: String,
    tok: String,
) -> Response {
    if !state.db.begin_refund_execution(id) {
        // Lost the atomic claim: another executor holds the row.
        return finance_redirect(state, "error=executing");
    }
    // §4.1: no raw transfer() here — the shared helper persists the spend_id
    // before dispatch and classifies the outcome fail-closed.
    let cp = crate::crypto::CryptoPay::new(tok);
    let outcome =
        match execute_guarded_transfer(&state.db, "refund", id, seller_id, None, |spend_id| {
            let cp = cp;
            let asset = asset.clone();
            let amount = amount.clone();
            async move {
                cp.transfer_with_spend_id(refund_user, &asset, &amount, &spend_id)
                    .await
            }
        })
        .await
        {
            Ok(outcome) => outcome,
            Err(e) => {
                return finance_redirect(
                    state,
                    &format!(
                        "error=transfer_not_attempted&detail={}",
                        qenc(e.chars().take(200).collect::<String>().as_str())
                    ),
                );
            }
        };
    match outcome {
        GuardedTransferOutcome::Succeeded { spend_id, receipt } => {
            // We won the claim, so the executing -> executed finish is ours
            // alone. Settlement effects run only for a full refund (§1.3).
            state.db.finish_refund_execution(id, "executed");
            let (subscription_revoked, kick_failed) =
                finance_refund_finalize(state, seller_id, invoice_id.clone(), refund_user).await;
            let meta = json!({
                "invoice_id": invoice_id, "amount": amount, "asset": asset,
                "user_id": refund_user, "subscription_revoked": subscription_revoked,
                "spend_id": spend_id, "receipt": receipt,
            })
            .to_string();
            state.db.log_action(
                session.account_id,
                &session.role,
                "refund_executed",
                "refund",
                id.to_string(),
                Some(&meta),
            );
            if kick_failed {
                finance_redirect(state, "refund_executed=1&warn=kick")
            } else {
                finance_redirect(state, "refund_executed=1")
            }
        }
        GuardedTransferOutcome::DefiniteRejected { spend_id, error } => {
            // API refused: no money moved. Revert to approved; a retry mints a
            // fresh spend_id/attempt (§4.1).
            state.db.finish_refund_execution(id, "approved");
            let meta = json!({
                "invoice_id": invoice_id, "amount": amount, "asset": asset,
                "user_id": refund_user, "spend_id": spend_id, "error": error,
            })
            .to_string();
            state.db.log_action(
                session.account_id,
                &session.role,
                "refund_execute_failed",
                "refund",
                id.to_string(),
                Some(&meta),
            );
            finance_redirect(
                state,
                &format!(
                    "error=transfer&detail={}",
                    qenc(error.chars().take(200).collect::<String>().as_str())
                ),
            )
        }
        GuardedTransferOutcome::Ambiguous { spend_id, error } => {
            // Unknown outcome: keep the row 'executing' with its persisted
            // spend_id for the §5 reconciliation flow; never retry blind.
            let meta = json!({
                "invoice_id": invoice_id, "amount": amount, "asset": asset,
                "user_id": refund_user, "spend_id": spend_id, "error": error,
            })
            .to_string();
            state.db.log_action(
                session.account_id,
                &session.role,
                "refund_execute_ambiguous",
                "refund",
                id.to_string(),
                Some(&meta),
            );
            finance_redirect(
                state,
                &format!(
                    "error=ambiguous&detail={}",
                    qenc(error.chars().take(200).collect::<String>().as_str())
                ),
            )
        }
    }
}

#[derive(Deserialize)]
pub(super) struct FinanceReconciliationResolveForm {
    pub(super) row_type: String,
    pub(super) id: String,
    pub(super) outcome: String,
    pub(super) reason: String,
}

/// §5.3 owner resolution of one `needs_reconciliation` row. The web surface
/// used here is a plain POST form (no JS, no inline secrets) over core's
/// append-only DB CAS. A non-empty reason is mandatory at both layers, and
/// "executed" for a refund is the manual-settlement path, so it also gets the
/// §2 creator-separation check. Auditing mirrors the server's REST endpoint.
#[tracing::instrument(skip_all)]
pub(super) async fn finance_reconciliation_resolve(
    State(state): State<WebState>,
    headers: axum::http::HeaderMap,
    Form(f): Form<FinanceReconciliationResolveForm>,
) -> impl IntoResponse {
    let session = match session_from_headers(&state, &headers) {
        Some(s) => s,
        None => return Redirect::to(&state.url("/login")).into_response(),
    };
    if !is_owner(&session) {
        return forbidden_page(&state, "Finance", "Reconciliation (owner only)").into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return finance_redirect(&state, "error=invalid");
    };
    if id <= 0 || !matches!(f.row_type.as_str(), "refund" | "payout") {
        return finance_redirect(&state, "error=invalid");
    }
    let outcome = match f.outcome.as_str() {
        "executed" => ReconciliationOutcome::Executed,
        "reverted_to_approved" => ReconciliationOutcome::RevertedToApproved,
        "void" => ReconciliationOutcome::Void,
        _ => return finance_redirect(&state, "error=invalid_reconciliation"),
    };
    if f.reason.trim().is_empty() {
        return finance_redirect(&state, "error=reason_required");
    }
    // Manual "executed" on a refund is effectively the process/settle path:
    // the creator may not settle their own money row.
    if f.row_type == "refund"
        && outcome == ReconciliationOutcome::Executed
        && creator_owns_refund(&state, &session, id)
    {
        return forbidden_page(
            &state,
            "Finance",
            "Refund settlement (creator cannot settle own refund)",
        )
        .into_response();
    }
    // Attach the persisted spend_id BEFORE the CAS so an append-only loser
    // cannot emit a second money event with a stale key.
    let spend_id = state
        .db
        .latest_transfer_attempt(&f.row_type, id)
        .map(|a| a.spend_id);
    if let Err(e) =
        state
            .db
            .resolve_reconciliation(&f.row_type, id, outcome, session.account_id, &f.reason)
    {
        let target = match e {
            ReconcileError::NotFound => "error=reconcile_notfound",
            ReconcileError::NotDue => "error=reconcile_notdue",
            ReconcileError::AlreadyResolved => "error=reconcile_already",
            ReconcileError::ReasonRequired => "error=reason_required",
            ReconcileError::Database(err) => {
                tracing::error!("reconciliation resolution failed: {}", err);
                "error=reconcile_failed"
            }
        };
        return finance_redirect(&state, target);
    }
    if let Some(ref sid) = spend_id {
        let outcome_token = match outcome {
            ReconciliationOutcome::Executed => "executed",
            ReconciliationOutcome::RevertedToApproved => "reverted_to_approved",
            ReconciliationOutcome::Void => "void",
        };
        state.db.set_transfer_attempt_outcome(sid, outcome_token);
    }
    let mut kick_failed = false;
    // An "executed" resolution is owner proof that the transfer landed:
    // apply the same §1.3 settlement as a receipt.
    if outcome == ReconciliationOutcome::Executed
        && f.row_type == "refund"
        && let Some(r) = state.db.get_refund(id)
    {
        let (_, seller, invoice_id, user, ..) = r;
        let (_, kick) =
            finance_refund_finalize(&state, seller, invoice_id, user.unwrap_or(0)).await;
        kick_failed = kick;
    }
    let meta = json!({
        "row_type": f.row_type, "outcome": f.outcome, "reason": f.reason,
        "spend_id": spend_id, "actor_id": session.account_id,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "transfer_reconciliation_resolved",
        &f.row_type,
        id.to_string(),
        Some(&meta),
    );
    if kick_failed {
        finance_redirect(&state, "reconciliation_resolved=1&warn=kick")
    } else {
        finance_redirect(&state, "reconciliation_resolved=1")
    }
}
