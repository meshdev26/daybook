use super::*;

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
    let cp = crate::crypto::CryptoPay::new(tok);
    match cp.transfer(dest_user, &asset, &amount).await {
        Ok(receipt) => {
            // We won the claim, so the executing -> paid finish is ours alone.
            state.db.finish_payout_execution(id, "paid");
            let meta = json!({
                "seller_id": seller_id, "amount": amount, "asset": asset,
                "destination": dest_user, "receipt": receipt,
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
        Err(e) => {
            if transfer_rejected_definitely(&e) {
                // The API refused: no money moved — put the payout back to
                // 'approved' so the transfer can be retried.
                state.db.finish_payout_execution(id, "approved");
                let meta = json!({
                    "seller_id": seller_id, "amount": amount, "asset": asset,
                    "destination": dest_user, "error": e.to_string(),
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
                        qenc(e.chars().take(200).collect::<String>().as_str())
                    ),
                )
            } else {
                // Ambiguous (timeout/network): the transfer may or may not
                // have executed. Leave the row 'executing' — it is the
                // operator's signal that this payout needs a manual check
                // before anyone retries.
                let meta = json!({
                    "seller_id": seller_id, "amount": amount, "asset": asset,
                    "destination": dest_user, "error": e.to_string(),
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
                        qenc(e.chars().take(200).collect::<String>().as_str())
                    ),
                )
            }
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
/// inherits that seller). Amount/asset default to the invoice's. A refund
/// can never move money the invoice did not collect: seller-role creators are
/// clamped to the invoice's amount and locked to its asset (asset mismatch or
/// a bad amount is a banner error, nothing created); admins/owner keep the
/// looser form — they are the approvers of whatever gets created here.
/// Audit: `refund_create`.
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
    // Only settled money can be returned.
    if inv_status != "paid" {
        return finance_redirect(&state, "error=invoice_state");
    }
    // Seller lock: the asset must equal the invoice's — a refund in a
    // different asset is not a return of the buyer's money.
    if Role::from_role_str(&session.role) == Role::Seller
        && f.asset
            .as_deref()
            .map(str::trim)
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
    let id = state.db.create_refund(
        inv_seller,
        Some(invoice_id),
        inv_user,
        &amount,
        &asset,
        reason.as_deref(),
    );
    let meta = json!({
        "invoice_id": invoice_id, "amount": amount, "asset": asset,
        "user_id": inv_user, "reason": reason,
    })
    .to_string();
    state.db.log_action(
        session.account_id,
        &session.role,
        "refund_create",
        "refund",
        id.to_string(),
        Some(&meta),
    );
    finance_redirect(&state, "refund_created=1")
}

#[derive(Deserialize)]
pub(super) struct FinanceRefundActionForm {
    pub(super) id: String,
}

/// Mirrors REST `finance_refunds_approve`: admin-or-owner, executed is final,
/// otherwise pending/approved -> approved. Audit: `refund_approve`.
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
    if !Role::from_role_str(&session.role).has_level(Role::Admin) {
        return forbidden_page(&state, "Finance", "Refund approval (admin or owner)")
            .into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return finance_redirect(&state, "error=invalid");
    };
    if id <= 0 {
        return finance_redirect(&state, "error=invalid");
    }
    let Some(refund) = state.db.get_refund(id.to_string()) else {
        return finance_redirect(&state, "error=notfound");
    };
    let (_rid, _seller, invoice_id, _user, amount, asset, _reason, status, ..) = refund;
    if status == "executed" {
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

/// Mirrors REST `finance_refunds_execute` (reserve-then-transfer):
/// admin-or-owner; approved -> executed by a real Crypto Pay `transfer` back
/// to the buyer. The row is first claimed 'executing' atomically, so two
/// concurrent executors can never both move money — the CAS loser is bounced
/// with an `executing` banner before any transfer is attempted. On success
/// the invoice is marked refunded and the subscription the invoice granted
/// is revoked (status 'refunded' + channel kick via ban+unban — a failed
/// kick is reported in the banner, the status flip stays the source of
/// truth). A definite API rejection reverts the row to 'approved' for retry
/// and is audited (`refund_execute_failed`); an ambiguous transport outcome
/// (timeout, network) leaves the row 'executing' — the transfer state is
/// unknown and retrying blind risks a double payout — and is audited
/// (`refund_execute_ambiguous`) with its own banner.
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
    if !Role::from_role_str(&session.role).has_level(Role::Admin) {
        return forbidden_page(&state, "Finance", "Refund execution (admin or owner)")
            .into_response();
    }
    let Ok(id) = f.id.trim().parse::<i64>() else {
        return finance_redirect(&state, "error=invalid");
    };
    if id <= 0 {
        return finance_redirect(&state, "error=invalid");
    }
    let Some(refund) = state.db.get_refund(id.to_string()) else {
        return finance_redirect(&state, "error=notfound");
    };
    let (_rid, seller_id, invoice_id, refund_user, amount, asset, _reason, status, ..) = refund;
    if status == "executed" {
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
    let cp = crate::crypto::CryptoPay::new(tok);
    match cp.transfer(refund_user, &asset, &amount).await {
        Ok(receipt) => {
            // We won the claim, so the executing -> executed finish is ours
            // alone.
            state.db.finish_refund_execution(id, "executed");
            let (subscription_revoked, kick_failed) =
                finance_refund_finalize(state, seller_id, invoice_id.clone(), refund_user).await;
            let meta = json!({
                "invoice_id": invoice_id, "amount": amount, "asset": asset,
                "user_id": refund_user, "subscription_revoked": subscription_revoked,
                "receipt": receipt,
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
        Err(e) => {
            if transfer_rejected_definitely(&e) {
                // The API refused: no money moved — put the refund back to
                // 'approved' so the transfer can be retried.
                state.db.finish_refund_execution(id, "approved");
                let meta = json!({
                    "invoice_id": invoice_id, "amount": amount, "asset": asset,
                    "user_id": refund_user, "error": e.to_string(),
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
                        qenc(e.chars().take(200).collect::<String>().as_str())
                    ),
                )
            } else {
                // Ambiguous (timeout/network): the transfer may or may not
                // have executed. Leave the row 'executing' — it is the
                // operator's signal that this refund needs a manual check
                // before anyone retries.
                let meta = json!({
                    "invoice_id": invoice_id, "amount": amount, "asset": asset,
                    "user_id": refund_user, "error": e.to_string(),
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
                        qenc(e.chars().take(200).collect::<String>().as_str())
                    ),
                )
            }
        }
    }
}
