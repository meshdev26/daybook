//! Post-payment grant fulfillment, single-sourced for the background granter
//! and shared with the REST claim handler's verification step.
//!
//! Every grant is claim-before-verify: the invoice is flipped `active` ->
//! `granting` atomically, then re-verified against the provider, then granted
//! exactly once. A not-settled invoice releases its claim; a verification
//! error leaves it granted-by-nobody and ages out at a bounded attempt cap.

use async_trait::async_trait;
use serde_json::Value;
use teloxide::{payloads::CreateChatInviteLinkSetters, prelude::*};

use crate::{db::Db, metrics, util};

/// Bounded retries for background jobs. After this many failed verification or
/// send attempts the item is terminal-failed; a not-settled release does not
/// count (a buyer may pay days later).
pub const VERIFY_ATTEMPT_CAP: i64 = 10;

/// Minimal provider seam so the granter can be driven by a local mock in tests
/// without changing production wiring. The real implementation is
/// `crypto::CryptoPay`, constructed with the canonical production base URL.
#[async_trait]
pub trait InvoiceVerifier: Send + Sync {
    async fn get_invoice(&self, invoice_id: &str) -> Result<Value, String>;
}

#[async_trait]
impl InvoiceVerifier for crate::crypto::CryptoPay {
    async fn get_invoice(&self, invoice_id: &str) -> Result<Value, String> {
        crate::crypto::CryptoPay::get_invoice(self, invoice_id).await
    }
}

/// Verify a settled payment through a provider and decode the first `paid`
/// item. Shared by the granter and the REST claim so both interpret the
/// provider response identically (fail closed on transport/API errors).
pub async fn verify_paid<V: InvoiceVerifier + ?Sized>(
    verifier: &V,
    invoice_id: &str,
) -> Result<bool, String> {
    let response = verifier.get_invoice(invoice_id).await?;
    Ok(crate::api::extract_invoice_items(&response)
        .iter()
        .any(|x| x.get("status").and_then(|s| s.as_str()) == Some("paid")))
}

/// Record one failed verification attempt and, at the cap, move the row to
/// the terminal `failed` state, log loudly, and audit. A degraded counter (0)
/// reads as "never capped", preserving retry-forever behavior on DB errors.
pub fn bump_invoice_failure(db: &Db, inv_id: &str, reason: &str) -> i64 {
    let attempts = db.bump_invoice_attempts(inv_id);
    if attempts >= VERIFY_ATTEMPT_CAP {
        // Once the provider verified `paid` the money is settled: terminal
        // `failed` is illegal because it would strand a paid buyer and hide
        // the liability. Refuse the transition and raise an audited alarm.
        let status = db.get_invoice(inv_id).map(|i| i.2).unwrap_or_default();
        if status == "paid" || status == "paid_ungranted" {
            db.log_action(
                0,
                "system",
                "invoice_failed_after_paid_blocked",
                "invoice",
                inv_id,
                Some(&format!(
                    "attempts={} reason={} status={}",
                    attempts, reason, status
                )),
            );
            tracing::error!(
                "blocked terminal-failed transition on paid invoice {} (status {}, {} attempts, reason: {})",
                inv_id,
                status,
                attempts,
                reason
            );
            return attempts;
        }
        db.update_invoice_status(inv_id, "failed");
        tracing::error!(
            "invoice {} failed terminally after {} verification attempts ({}); restore manually via the admin invoice-status API if needed",
            inv_id,
            attempts,
            reason
        );
        db.log_action(
            0,
            "system",
            "invoice_verify_failed",
            "invoice",
            inv_id,
            Some(&format!("attempts={} reason={}", attempts, reason)),
        );
    }
    attempts
}

/// Verification said `paid` but the exactly-once claim produced no durable
/// grant. Move the row to the non-terminal recovery state and audit. Never
/// call `finalize_invoice_paid` here: that would hide the ungranted liability.
pub fn mark_paid_ungranted(db: &Db, inv_id: &str, seller_id: i64, reason: &str) {
    if !db.mark_invoice_paid_ungranted(inv_id) {
        return;
    }
    let inv = db.get_invoice(inv_id);
    let meta = serde_json::json!({
        "seller_id": seller_id,
        "amount": inv.as_ref().map(|i| i.0.as_str()).unwrap_or("-"),
        "asset": inv.as_ref().map(|i| i.1.as_str()).unwrap_or("-"),
        "reason": reason,
    })
    .to_string();
    db.log_action(
        0,
        "system",
        "invoice_paid_ungranted",
        "invoice",
        inv_id,
        Some(&meta),
    );
    tracing::warn!(
        "invoice {} verified paid but not granted ({}); queued for 30s recovery",
        inv_id,
        reason
    );
}

/// Process one claimed invoice end-to-end. The caller (the 30s granter loop)
/// has already selected the invoice for this bot via `BotManager::for_seller`;
/// `bot` is that seller's Telegram client and `this_id` its bot id (used by
/// the outbound moderation gate).
///
/// Behavior is byte-identical to the inlined loop body: claim first, verify
/// second, fail closed on every error, keep paid-but-ungranted rows recoverable
/// and never finalize them into `paid`.
#[allow(clippy::too_many_arguments)]
pub async fn grant_one<V: InvoiceVerifier + ?Sized>(
    db: &Db,
    bot: &Bot,
    verifier: Option<&V>,
    inv_id: &str,
    plan_name: Option<&str>,
    uid: Option<i64>,
    inv_seller: i64,
    this_id: i64,
    owner_id: i64,
) {
    // (a) Claim: only the winner verifies and grants.
    if !db.claim_invoice_for_grant(inv_id) {
        return;
    }
    // (b) Without a provider there is no way to verify payment: leave the
    // claim for a later cycle. A missing token is a permanent misconfiguration
    // and ages out at the attempt cap, so no grant is ever issued on doubt.
    let Some(verifier) = verifier else {
        let attempts = bump_invoice_failure(db, inv_id, "no crypto token configured");
        if attempts < VERIFY_ATTEMPT_CAP {
            tracing::warn!(
                "invoice {} claimed but no crypto token configured; leaving for retry (attempt {}/{})",
                inv_id,
                attempts,
                VERIFY_ATTEMPT_CAP
            );
        }
        return;
    };
    // (c) Verify with the provider; any error stays fail-closed.
    let paid = match verify_paid(verifier, inv_id).await {
        Ok(paid) => paid,
        Err(e) => {
            let attempts =
                bump_invoice_failure(db, inv_id, &format!("provider verification error: {}", e));
            if attempts < VERIFY_ATTEMPT_CAP {
                tracing::warn!(
                    "invoice {} verification failed: {}; leaving for retry (attempt {}/{})",
                    inv_id,
                    e,
                    attempts,
                    VERIFY_ATTEMPT_CAP
                );
            }
            return;
        }
    };
    if !paid {
        // Not settled: release the claim so future cycles and /paid can
        // re-verify cleanly. This is the sanctioned unwinder, never a raw
        // status write.
        db.release_invoice_claim(inv_id);
        return;
    }
    // (d-bis) Product invoices: grant the purchase and DM the delivery payload.
    // Plan invoices take the subscription path below; the branch is selected
    // by invoices.product_id.
    if let Some((pid, kind, pname, delivery)) = db.invoice_product_delivery(inv_id) {
        let target_user = uid.unwrap_or(0);
        if target_user == 0 {
            mark_paid_ungranted(
                db,
                inv_id,
                inv_seller,
                "paid product invoice has no bound user",
            );
            return;
        }
        if !db.product_grant_exists(inv_id) {
            match db.grant_product(inv_seller, target_user, pid, inv_id) {
                Ok(true) => {
                    if let Err(e) = crate::services::deliver::deliver(
                        bot,
                        target_user,
                        &kind,
                        &pname,
                        delivery.as_deref(),
                        db,
                        inv_seller,
                        Some(this_id),
                    )
                    .await
                    {
                        tracing::warn!(
                            "invoice {}: product '{}' granted but delivery DM failed: {}",
                            inv_id,
                            pname,
                            e
                        );
                    }
                }
                // A concurrent granter won the UNIQUE(invoice_id) race.
                Ok(false) => {}
                // e.g. out of stock: money settled, grant not, so keep the row
                // recoverable instead of hiding the liability.
                Err(e) => {
                    tracing::warn!(
                        "invoice {}: product grant failed ({}); queuing for recovery",
                        inv_id,
                        e
                    );
                    mark_paid_ungranted(db, inv_id, inv_seller, "product grant failed");
                    return;
                }
            }
        }
        db.finalize_invoice_paid(inv_id);
        return;
    }
    // (d) Idempotency: an existing subscription only needs finalizing.
    if db.subscription_exists_for_invoice(inv_id) {
        db.finalize_invoice_paid(inv_id);
        return;
    }
    // (e) Grant: subscription + single-use invite link + notice.
    let target_user = uid.unwrap_or(0);
    if target_user == 0 {
        mark_paid_ungranted(
            db,
            inv_id,
            inv_seller,
            "paid plan invoice has no bound user",
        );
        return;
    }
    // Plan lookup is seller-scoped: a same-name plan owned by another seller
    // must never decide this invoice's duration or channel.
    let plan_info = plan_name.and_then(|pn| db.get_plan(inv_seller, pn));
    let days = plan_info
        .as_ref()
        .map(|p| p.4)
        .unwrap_or(30)
        .clamp(1, crate::handlers::commands::MAX_PLAN_DAYS);
    let seller_id = inv_seller;
    let is_admin = seller_id == crate::roles::TenantScope::PLATFORM_ID
        || crate::roles::role_of(db, seller_id, owner_id) >= crate::roles::Role::Admin;
    let ch = plan_info
        .as_ref()
        .and_then(|p| p.5.clone())
        .unwrap_or_else(|| crate::handlers::commands::channel_of(db, Some(seller_id), is_admin));
    if ch.is_empty() {
        // Verified paid but nothing to grant into: non-terminal recovery with
        // a recoverable clock. Never age this to terminal `failed`.
        mark_paid_ungranted(db, inv_id, seller_id, "no channel resolvable");
        return;
    }
    let sub_id =
        db.create_subscription(seller_id, target_user, None, &ch, days, Some(inv_id), None);
    // Checked expiry math, defensive even behind the clamp above.
    let expire = chrono::Duration::try_days(days)
        .and_then(|d| chrono::Utc::now().checked_add_signed(d))
        .unwrap_or_else(chrono::Utc::now);
    let link = bot
        .create_chat_invite_link(util::to_chat_id(&ch))
        .member_limit(1)
        .expire_date(expire)
        .await
        .ok()
        .map(|l| l.invite_link);
    match link {
        Some(l) => {
            let grant_text = format!(
                "Payment confirmed. Access granted.\nInvite: {}\nSubscription #{}",
                l, sub_id
            );
            // Outbound gate before this raw-Bot DM; the grant itself is already
            // durable and finalizes below, only the notice may be skipped.
            if crate::handlers::moderation::gate_outbound(db, seller_id, Some(this_id), &grant_text)
                .is_ok()
            {
                match bot
                    .send_message(teloxide::types::ChatId(target_user), grant_text)
                    .await
                {
                    Err(e) => tracing::warn!("failed to send grant message: {}", e),
                    Ok(_) => metrics::bot_sent(),
                }
            }
        }
        None => {
            // Money settled and the subscription row exists, but no invite
            // link could be created: still finalize (the grant is durable),
            // then tell the buyer honestly instead of leaving them waiting.
            // DM failures are logged, never propagated to the granter.
            let notice = "Payment confirmed — your invite link couldn't be created. Contact the seller to get access.";
            if crate::handlers::moderation::gate_outbound(db, seller_id, Some(this_id), notice)
                .is_ok()
            {
                match bot
                    .send_message(teloxide::types::ChatId(target_user), notice)
                    .await
                {
                    Err(e) => tracing::warn!(
                        "invoice {}: granted without invite link and buyer notice failed: {}",
                        inv_id,
                        e
                    ),
                    Ok(_) => metrics::bot_sent(),
                }
            }
        }
    }
    // (f) Terminal: verified paid + granted.
    db.finalize_invoice_paid(inv_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bots::{BotContext, BotManager};
    use crate::identity::IdentityHandle;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use std::sync::Arc;
    use tower::ServiceExt;
    use wiremock::matchers::{body_partial_json, method, path, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const TOKEN: &str = "grant-provider-token";

    fn bot_for(url: &str) -> Bot {
        Bot::new("42:placeholder").set_api_url(url.parse::<reqwest::Url>().unwrap())
    }

    fn invite_response(link: &str) -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "result": {
                "invite_link": link,
                "creator": {"id": 42, "is_bot": true, "first_name": "Seller"},
                "creates_join_request": false,
                "is_primary": false,
                "is_revoked": false
            }
        })
    }

    fn message_response() -> serde_json::Value {
        serde_json::json!({
            "ok": true,
            "result": {
                "message_id": 1,
                "date": 1_700_000_000,
                "chat": {"id": 42, "type": "private", "first_name": "T"},
                "text": "x"
            }
        })
    }

    async fn mount_provider(server: &MockServer, invoice_id: &str, status: &str) {
        Mock::given(method("POST"))
            .and(path("/getInvoices"))
            .and(body_partial_json(
                serde_json::json!({"invoice_ids": invoice_id}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"items": [{"invoice_id": invoice_id, "status": status}]}
            })))
            .mount(server)
            .await;
    }

    async fn mount_provider_error(server: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/getInvoices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "error": "provider down"
            })))
            .mount(server)
            .await;
    }

    async fn mount_telegram(server: &MockServer, invite_ok: bool) {
        let invite = if invite_ok {
            ResponseTemplate::new(200).set_body_json(invite_response("https://t.me/+grant-invite"))
        } else {
            ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "ok": false,
                "description": "Bad Request: invite refused"
            }))
        };
        Mock::given(method("POST"))
            .and(path_regex(r".*/CreateChatInviteLink"))
            .respond_with(invite)
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path_regex(r".*/SendMessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(message_response()))
            .mount(server)
            .await;
    }

    fn new_db() -> (tempfile::TempDir, Arc<Db>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grant.db").to_string_lossy().into_owned();
        let db = Db::new(&path).unwrap();
        (dir, Arc::new(db))
    }

    fn cp_for(server: &MockServer) -> crate::crypto::CryptoPay {
        crate::crypto::CryptoPay::with_base_url(TOKEN.to_string(), &server.uri())
    }

    async fn hit_count(server: &MockServer, suffix: &str) -> usize {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with(suffix))
            .count()
    }

    async fn request_bodies(server: &MockServer, suffix: &str) -> Vec<serde_json::Value> {
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path().ends_with(suffix))
            .filter_map(|r| serde_json::from_slice(&r.body).ok())
            .collect()
    }

    async fn post_webhook(app: axum::Router, token: &str, id: &str) -> StatusCode {
        let body = format!(r#"{{"payload":{{"invoice_id":"{id}","status":"paid"}}}}"#);
        let mac = crate::util::hmac_sha256(token.as_bytes(), body.as_bytes());
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
        .status()
    }

    /// Full deterministic slice: signed webhook enqueues the row, the 30s
    /// granter's extracted body claims + verifies + grants, and the seller's
    /// mock bot sees exactly one invite link and one buyer DM. A replay adds
    /// no second row, provider call or DM.
    #[tokio::test]
    async fn webhook_to_grant_one_grants_once_with_invite_and_dm() {
        let (mut state, _state_dir) = crate::web::test_support::make_state().await;
        const WEBHOOK_TOKEN: &str = "webhook-secret";
        state.cfg.crypto_token = Some(WEBHOOK_TOKEN.to_string());
        let db = state.db.clone();
        db.upsert_seller(7, None, None);
        db.add_plan(7, "plan-e2e", "5", "USDT", 7, Some("@chan7"));
        db.save_invoice(
            "inv-e2e",
            7,
            "5",
            "USDT",
            Some("plan-e2e"),
            "pending",
            None,
            Some("plan-e2e"),
            Some(42),
            None,
        );
        let app = crate::api::router(state);
        assert_eq!(
            post_webhook(app.clone(), WEBHOOK_TOKEN, "inv-e2e").await,
            StatusCode::OK
        );
        assert_eq!(db.get_invoice("inv-e2e").unwrap().2, "active");

        let telegram = MockServer::start().await;
        let provider = MockServer::start().await;
        mount_provider(&provider, "inv-e2e", "paid").await;
        mount_telegram(&telegram, true).await;
        let bot = bot_for(&telegram.uri());
        let cp = cp_for(&provider);

        grant_one(
            &db,
            &bot,
            Some(&cp),
            "inv-e2e",
            Some("plan-e2e"),
            Some(42),
            7,
            9,
            999,
        )
        .await;

        assert_eq!(db.get_invoice("inv-e2e").unwrap().2, "paid");
        let subs = db.subscriptions_for_invoice("inv-e2e");
        assert_eq!(subs.len(), 1);
        assert_eq!(hit_count(&provider, "/getInvoices").await, 1);
        assert_eq!(hit_count(&telegram, "/CreateChatInviteLink").await, 1);
        assert_eq!(hit_count(&telegram, "/SendMessage").await, 1);
        let dms = request_bodies(&telegram, "/SendMessage").await;
        let text = dms[0]["text"].as_str().unwrap();
        assert!(text.contains("https://t.me/+grant-invite"), "{text}");
        assert!(
            text.contains(&format!("Subscription #{}", subs[0].0)),
            "{text}"
        );

        // Replay: the webhook cannot resurrect a paid row, and the granter
        // loses the claim, so nothing is duplicated.
        assert_eq!(
            post_webhook(app, WEBHOOK_TOKEN, "inv-e2e").await,
            StatusCode::OK
        );
        grant_one(
            &db,
            &bot,
            Some(&cp),
            "inv-e2e",
            Some("plan-e2e"),
            Some(42),
            7,
            9,
            999,
        )
        .await;
        assert_eq!(db.get_invoice("inv-e2e").unwrap().2, "paid");
        assert_eq!(db.subscriptions_for_invoice("inv-e2e").len(), 1);
        assert_eq!(hit_count(&provider, "/getInvoices").await, 1);
        assert_eq!(hit_count(&telegram, "/SendMessage").await, 1);
    }

    /// The plan branch asks Telegram for a single-use link that expires about
    /// the plan's duration from now, and the buyer DM carries both the link
    /// and the durable subscription id.
    #[tokio::test]
    async fn plan_grant_requests_single_use_invite_with_plan_expiry() {
        let (_dir, db) = new_db();
        db.add_plan(7, "week", "5", "USDT", 7, Some("@chan7"));
        db.save_invoice(
            "inv-plan",
            7,
            "5",
            "USDT",
            Some("week"),
            "active",
            None,
            Some("week"),
            Some(42),
            None,
        );
        let telegram = MockServer::start().await;
        let provider = MockServer::start().await;
        mount_provider(&provider, "inv-plan", "paid").await;
        mount_telegram(&telegram, true).await;

        grant_one(
            &db,
            &bot_for(&telegram.uri()),
            Some(&cp_for(&provider)),
            "inv-plan",
            Some("week"),
            Some(42),
            7,
            9,
            999,
        )
        .await;

        let invites = request_bodies(&telegram, "/CreateChatInviteLink").await;
        assert_eq!(invites.len(), 1);
        assert_eq!(invites[0]["member_limit"], 1);
        let expire = invites[0]["expire_date"].as_i64().unwrap();
        let expected = (chrono::Utc::now() + chrono::Duration::days(7)).timestamp();
        assert!(
            (expire - expected).abs() < 120,
            "expire={expire}, expected={expected}"
        );
        let sub_id = db.subscriptions_for_invoice("inv-plan")[0].0;
        let dms = request_bodies(&telegram, "/SendMessage").await;
        let text = dms[0]["text"].as_str().unwrap();
        assert!(text.contains("https://t.me/+grant-invite"), "{text}");
        assert!(text.contains(&format!("Subscription #{sub_id}")), "{text}");
        assert_eq!(db.get_invoice("inv-plan").unwrap().2, "paid");
    }

    /// Product invoices grant the purchase and send the delivery payload via
    /// the shared deliver service, with no invite link involved.
    #[tokio::test]
    async fn product_grant_delivers_payload_then_finalizes() {
        let (_dir, db) = new_db();
        let pid = db
            .add_product(
                7,
                "ebook",
                "5",
                "USDT",
                "digital_link",
                Some("https://example.com/ebook"),
                Some(10),
            )
            .unwrap();
        db.save_invoice(
            "inv-product",
            7,
            "5",
            "USDT",
            Some("ebook"),
            "active",
            None,
            None,
            Some(42),
            Some(pid),
        );
        let telegram = MockServer::start().await;
        let provider = MockServer::start().await;
        mount_provider(&provider, "inv-product", "paid").await;
        mount_telegram(&telegram, true).await;

        grant_one(
            &db,
            &bot_for(&telegram.uri()),
            Some(&cp_for(&provider)),
            "inv-product",
            None,
            Some(42),
            7,
            9,
            999,
        )
        .await;

        assert!(db.product_grant_exists("inv-product"));
        assert_eq!(db.get_invoice("inv-product").unwrap().2, "paid");
        assert_eq!(hit_count(&telegram, "/CreateChatInviteLink").await, 0);
        assert_eq!(hit_count(&telegram, "/SendMessage").await, 1);
        let dms = request_bodies(&telegram, "/SendMessage").await;
        let text = dms[0]["text"].as_str().unwrap();
        assert!(text.contains("Delivery: ebook"), "{text}");
        assert!(text.contains("https://example.com/ebook"), "{text}");
    }

    /// A failed invite-link API call must not strand the buyer: the
    /// subscription is durable, the fallback notice goes out, and the invoice
    /// is finalized (never left in paid-ungranted).
    #[tokio::test]
    async fn invite_link_failure_still_finalizes_and_sends_fallback_notice() {
        let (_dir, db) = new_db();
        db.add_plan(7, "plan", "5", "USDT", 7, Some("@chan7"));
        db.save_invoice(
            "inv-noinvite",
            7,
            "5",
            "USDT",
            Some("plan"),
            "active",
            None,
            Some("plan"),
            Some(42),
            None,
        );
        let telegram = MockServer::start().await;
        let provider = MockServer::start().await;
        mount_provider(&provider, "inv-noinvite", "paid").await;
        mount_telegram(&telegram, false).await;

        grant_one(
            &db,
            &bot_for(&telegram.uri()),
            Some(&cp_for(&provider)),
            "inv-noinvite",
            Some("plan"),
            Some(42),
            7,
            9,
            999,
        )
        .await;

        assert_eq!(db.subscriptions_for_invoice("inv-noinvite").len(), 1);
        assert_eq!(db.get_invoice("inv-noinvite").unwrap().2, "paid");
        assert_eq!(hit_count(&telegram, "/CreateChatInviteLink").await, 1);
        assert_eq!(hit_count(&telegram, "/SendMessage").await, 1);
        let dms = request_bodies(&telegram, "/SendMessage").await;
        let text = dms[0]["text"].as_str().unwrap();
        assert!(text.contains("invite link couldn't be created"), "{text}");
    }

    /// A not-settled provider answer releases the claim, grants nothing and
    /// spends exactly one provider call.
    #[tokio::test]
    async fn not_settled_invoice_releases_claim_without_grant_or_dm() {
        let (_dir, db) = new_db();
        db.add_plan(7, "plan", "5", "USDT", 7, Some("@chan7"));
        db.save_invoice(
            "inv-pending",
            7,
            "5",
            "USDT",
            Some("plan"),
            "active",
            None,
            Some("plan"),
            Some(42),
            None,
        );
        let telegram = MockServer::start().await;
        let provider = MockServer::start().await;
        mount_provider(&provider, "inv-pending", "pending").await;
        mount_telegram(&telegram, true).await;

        grant_one(
            &db,
            &bot_for(&telegram.uri()),
            Some(&cp_for(&provider)),
            "inv-pending",
            Some("plan"),
            Some(42),
            7,
            9,
            999,
        )
        .await;

        assert_eq!(db.get_invoice("inv-pending").unwrap().2, "active");
        assert!(db.subscriptions_for_invoice("inv-pending").is_empty());
        assert_eq!(hit_count(&provider, "/getInvoices").await, 1);
        assert_eq!(hit_count(&telegram, "/CreateChatInviteLink").await, 0);
        assert_eq!(hit_count(&telegram, "/SendMessage").await, 0);
    }

    /// Provider errors leave the claim in `granting` and bump the attempt;
    /// reaching the cap terminal-fails with an audited reason. No grant or DM
    /// happens on either path.
    #[tokio::test]
    async fn provider_error_keeps_claim_and_terminal_fails_at_cap() {
        let (_dir, db) = new_db();
        for id in ["inv-err", "inv-cap"] {
            db.save_invoice(
                id,
                7,
                "5",
                "USDT",
                None,
                "active",
                None,
                None,
                Some(42),
                None,
            );
        }
        for _ in 0..(VERIFY_ATTEMPT_CAP - 1) {
            db.bump_invoice_attempts("inv-cap");
        }
        let telegram = MockServer::start().await;
        let provider = MockServer::start().await;
        mount_provider_error(&provider).await;
        mount_telegram(&telegram, true).await;

        for id in ["inv-err", "inv-cap"] {
            grant_one(
                &db,
                &bot_for(&telegram.uri()),
                Some(&cp_for(&provider)),
                id,
                None,
                Some(42),
                7,
                9,
                999,
            )
            .await;
        }

        assert_eq!(db.get_invoice("inv-err").unwrap().2, "granting");
        assert_eq!(db.get_invoice("inv-cap").unwrap().2, "failed");
        assert!(db.subscriptions_for_invoice("inv-err").is_empty());
        assert!(db.subscriptions_for_invoice("inv-cap").is_empty());
        assert_eq!(hit_count(&provider, "/getInvoices").await, 2);
        assert_eq!(hit_count(&telegram, "/SendMessage").await, 0);
        assert!(
            db.list_audit_log(50)
                .iter()
                .any(|a| a.3 == "invoice_verify_failed" && a.5.as_deref() == Some("inv-cap")),
            "cap failure must be audited"
        );
    }

    /// No provider configured: no provider call at all, the claim is left for
    /// retry below the cap, and a preloaded row ages out to terminal failed.
    #[tokio::test]
    async fn missing_verifier_never_calls_provider_and_bumps_attempt() {
        let (_dir, db) = new_db();
        for id in ["inv-notoken", "inv-notoken-cap"] {
            db.save_invoice(
                id,
                7,
                "5",
                "USDT",
                None,
                "active",
                None,
                None,
                Some(42),
                None,
            );
        }
        for _ in 0..(VERIFY_ATTEMPT_CAP - 1) {
            db.bump_invoice_attempts("inv-notoken-cap");
        }
        let telegram = MockServer::start().await;
        let provider = MockServer::start().await;
        mount_provider(&provider, "inv-notoken", "paid").await;
        mount_telegram(&telegram, true).await;

        for id in ["inv-notoken", "inv-notoken-cap"] {
            grant_one::<crate::crypto::CryptoPay>(
                &db,
                &bot_for(&telegram.uri()),
                None,
                id,
                None,
                Some(42),
                7,
                9,
                999,
            )
            .await;
        }

        assert_eq!(db.get_invoice("inv-notoken").unwrap().2, "granting");
        assert_eq!(db.get_invoice("inv-notoken-cap").unwrap().2, "failed");
        assert_eq!(hit_count(&provider, "/getInvoices").await, 0);
        assert_eq!(hit_count(&telegram, "/SendMessage").await, 0);
        assert!(
            db.list_audit_log(50).iter().any(
                |a| a.3 == "invoice_verify_failed" && a.5.as_deref() == Some("inv-notoken-cap")
            )
        );
    }

    /// Verified paid but ungrantable (no bound user, or no channel) goes to
    /// non-terminal recovery and never sends a buyer DM.
    #[tokio::test]
    async fn ungrantable_paid_invoices_go_to_paid_ungranted() {
        let (_dir, db) = new_db();
        db.add_plan(7, "plan", "5", "USDT", 7, Some("@chan7"));
        db.add_plan(7, "nochan", "5", "USDT", 7, None);
        db.save_invoice(
            "inv-nouser",
            7,
            "5",
            "USDT",
            Some("plan"),
            "active",
            None,
            Some("plan"),
            None,
            None,
        );
        db.save_invoice(
            "inv-nochan",
            7,
            "5",
            "USDT",
            Some("nochan"),
            "active",
            None,
            Some("nochan"),
            Some(42),
            None,
        );
        let telegram = MockServer::start().await;
        let provider = MockServer::start().await;
        mount_provider(&provider, "inv-nouser", "paid").await;
        mount_provider(&provider, "inv-nochan", "paid").await;
        mount_telegram(&telegram, true).await;

        for (id, plan) in [("inv-nouser", "plan"), ("inv-nochan", "nochan")] {
            grant_one(
                &db,
                &bot_for(&telegram.uri()),
                Some(&cp_for(&provider)),
                id,
                Some(plan),
                if id == "inv-nouser" { None } else { Some(42) },
                7,
                9,
                999,
            )
            .await;
        }

        for id in ["inv-nouser", "inv-nochan"] {
            assert_eq!(db.get_invoice(id).unwrap().2, "paid_ungranted", "{id}");
            assert!(db.subscriptions_for_invoice(id).is_empty(), "{id}");
        }
        assert_eq!(hit_count(&telegram, "/SendMessage").await, 0);
        assert_eq!(hit_count(&telegram, "/CreateChatInviteLink").await, 0);
    }

    /// The granter runs on the seller's bot: with A as primary and B bound in
    /// the DB, only B's mock sees the invite and DM; A's and the provider of A
    /// see nothing.
    #[tokio::test]
    async fn seller_b_invoice_uses_bots_bot_only() {
        let (state, _state_dir) = crate::web::test_support::make_state().await;
        let db = state.db.clone();
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
            Some(42),
            None,
        );

        let telegram_a = MockServer::start().await;
        let telegram_b = MockServer::start().await;
        let provider_b = MockServer::start().await;
        let provider_a = MockServer::start().await;
        mount_provider(&provider_b, "inv-b", "paid").await;
        mount_telegram(&telegram_a, true).await;
        mount_telegram(&telegram_b, true).await;

        let shared = Arc::new(state.cfg.clone());
        let ctx_a = Arc::new(BotContext {
            id: 1,
            key: "a".to_string(),
            bot: bot_for(&telegram_a.uri()),
            owner_id: 0,
            shared: shared.clone(),
            db: db.clone(),
        });
        let ctx_b = Arc::new(BotContext {
            id: 9,
            key: "b".to_string(),
            bot: bot_for(&telegram_b.uri()),
            owner_id: 0,
            shared,
            db: db.clone(),
        });
        let bots = BotManager {
            bots: vec![ctx_a.clone(), ctx_b.clone()],
            primary: ctx_a,
            db: db.clone(),
            identity: IdentityHandle::unconnected(),
        };
        db.upsert_seller_bot_id(99, Some(9));
        assert_eq!(bots.for_seller(99).id, 9);

        let resolved = bots.for_seller(99).bot.clone();
        grant_one(
            &db,
            &resolved,
            Some(&cp_for(&provider_b)),
            "inv-b",
            Some("plan-b"),
            Some(42),
            99,
            9,
            999,
        )
        .await;

        assert_eq!(db.get_invoice("inv-b").unwrap().2, "paid");
        assert_eq!(db.subscriptions_for_invoice("inv-b").len(), 1);
        assert_eq!(hit_count(&provider_b, "/getInvoices").await, 1);
        assert_eq!(hit_count(&provider_a, "/getInvoices").await, 0);
        assert_eq!(hit_count(&telegram_a, "/CreateChatInviteLink").await, 0);
        assert_eq!(hit_count(&telegram_a, "/SendMessage").await, 0);
        assert_eq!(hit_count(&telegram_b, "/CreateChatInviteLink").await, 1);
        assert_eq!(hit_count(&telegram_b, "/SendMessage").await, 1);
    }

    /// A seller-tier account with no channel of its own must never be granted
    /// into the operator's global channel: fail closed to `paid_ungranted`.
    #[tokio::test]
    async fn seller_no_channel_never_grants_into_global_channel() {
        let (_dir, db) = new_db();
        db.set_config("channel", "@operator");
        db.upsert_seller(7, None, None);
        db.add_plan(7, "no-channel-plan", "5", "USDT", 7, None);
        db.save_invoice(
            "inv-no-channel",
            7,
            "5",
            "USDT",
            Some("no-channel-plan"),
            "active",
            None,
            Some("no-channel-plan"),
            Some(42),
            None,
        );
        let telegram = MockServer::start().await;
        let provider = MockServer::start().await;
        mount_provider(&provider, "inv-no-channel", "paid").await;
        mount_telegram(&telegram, true).await;

        grant_one(
            &db,
            &bot_for(&telegram.uri()),
            Some(&cp_for(&provider)),
            "inv-no-channel",
            Some("no-channel-plan"),
            Some(42),
            7,
            9,
            999,
        )
        .await;

        assert_eq!(
            db.get_invoice("inv-no-channel").unwrap().2,
            "paid_ungranted"
        );
        assert!(db.subscriptions_for_invoice("inv-no-channel").is_empty());
        assert_eq!(hit_count(&telegram, "/CreateChatInviteLink").await, 0);
    }

    /// Admin/Owner keep the global fallback; only the seller-tier fallback is
    /// removed. The admin control proves the rule is a scope check, not a ban.
    #[tokio::test]
    async fn admin_seller_keeps_global_channel_fallback() {
        let (_dir, db) = new_db();
        db.set_config("channel", "@operator");
        db.upsert_seller(7, None, None);
        db.add_admin(7, None);
        db.add_plan(7, "no-channel-plan", "5", "USDT", 7, None);
        db.save_invoice(
            "inv-admin",
            7,
            "5",
            "USDT",
            Some("no-channel-plan"),
            "active",
            None,
            Some("no-channel-plan"),
            Some(42),
            None,
        );
        let telegram = MockServer::start().await;
        let provider = MockServer::start().await;
        mount_provider(&provider, "inv-admin", "paid").await;
        mount_telegram(&telegram, true).await;

        grant_one(
            &db,
            &bot_for(&telegram.uri()),
            Some(&cp_for(&provider)),
            "inv-admin",
            Some("no-channel-plan"),
            Some(42),
            7,
            9,
            999,
        )
        .await;

        assert_eq!(db.get_invoice("inv-admin").unwrap().2, "paid");
        let subs = db.subscriptions_for_invoice("inv-admin");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].3, "@operator");
        assert_eq!(hit_count(&telegram, "/CreateChatInviteLink").await, 1);
    }
}
