//! Telegram Business Platform — library crate.
//!
//! The binary (`src/main.rs`) is a thin bootstrap over this library; exposing
//! the modules as a lib keeps them fuzzable (`fuzz/`) and integration-testable
//! without going through the process boundary.

pub mod ai;
pub mod api;
pub mod auth;
pub mod bots;
pub mod config;
pub mod constants;
pub mod crypto;
pub mod db;
pub mod dialogue;
pub mod embed;
pub mod handlers;
pub mod identity;
pub mod mcp;
pub mod metrics;
pub mod qr;
pub mod roles;
pub mod services;
pub mod util;
pub mod web;

#[cfg(feature = "automation")]
pub use handlers::capabilities::automation as automation_capability;
#[cfg(feature = "file_share")]
pub use handlers::capabilities::file_share as file_share_capability;
#[cfg(feature = "movie_bot")]
pub use handlers::capabilities::movie_bot as movie_bot_capability;
#[cfg(feature = "reply_bot")]
pub use handlers::capabilities::reply_bot as reply_bot_capability;
#[cfg(feature = "service")]
pub use handlers::capabilities::service as service_capability;
/// Capability catalog and bot-type dispatcher (multibot foundation, L8).
///
/// The catalog itself is always compiled so an uncompiled/unknown bot type
/// can still produce an explicit refusal. Each per-capability module is
/// `#[cfg(feature = "...")]`-gated in `handlers::capabilities`; these
/// crate-root re-exports expose exactly the capability modules compiled into
/// this build, keeping the feature flags and runtime type catalog in sync.
#[cfg(feature = "shop_bot")]
pub use handlers::capabilities::shop_bot as shop_bot_capability;
#[cfg(feature = "web_app")]
pub use handlers::capabilities::web_app as web_app_capability;

// L9 feature-optionalization investigation (complete): the capability
// features are inert markers today and no dependency is optionalizable from
// this crate root. Verified with `cargo tree --depth 1`: the default and
// `--no-default-features --features movie_bot` direct dependency sets are
// identical. Every crate in `[dependencies]` is referenced by a module that
// compiles for every feature set:
//   * src/web/**, src/api.rs, src/auth.rs, src/embed.rs, src/mcp/**,
//     src/bots.rs, src/services/** -> axum/reqwest/serde/tokio/tracing/
//     chrono/sha2/cookie/bcrypt/getrandom;
//   * src/ai/**, src/crypto.rs -> reqwest/async-trait;
//   * src/identity/** -> grammers-*/aes-gcm/pbkdf2/zeroize/thiserror, and it
//     is wired into WebState by web/identity.rs so it cannot be cfg-gated here;
//   * src/db.rs -> rusqlite; src/config.rs + web/bots_*.rs -> toml;
//   * src/main.rs (binary) builds the shop command/dialogue tree from
//     handlers::commands / messages / callbacks unconditionally and needs
//     teloxide's sqlite-storage feature for every dispatcher.
// The capability modules re-exported above are stub `Descriptor` constants
// with no dependency of their own, and every crate-root module below is
// shared infrastructure. Gating any of them requires edits to L7 (web/api/
// auth/embed) and L8 (handlers core, main.rs) files — out of L9 territory.
// Until those rungs gate their modules there is no safe `#[cfg(feature=...)]`
// addition here and no safe `optional = true` / `dep:` mapping in Cargo.toml;
// both builds stay green and the default build stays byte-identical.

use teloxide::prelude::*;

/// Revoke every expired subscription: kick the member out of the channel
/// (ban + unban) and mark the row expired. Shared by the `/expirecheck`
/// command and the periodic expiry sweeper so the behavior stays
/// single-sourced.
///
/// The kick goes through the bot that owns the row's seller
/// (`BotManager::for_seller`), so one bot never moderates another bot's
/// channels. Returns one entry per expired subscription —
/// (subscription id, seller_id, buyer user_id, channel) — so callers can DM
/// the buyer the revocation notice from that same bot; the fields come
/// straight from `expired_subscriptions()` (subscriptions.id / seller_id /
/// user_id / channel_id), no extra joins.
pub async fn expire_subscriptions(
    bots: &crate::bots::BotManager,
    db: &db::Db,
) -> Vec<(i64, i64, i64, String)> {
    let mut expired = Vec::new();
    for (id, seller_id, uid, ch) in db.expired_subscriptions() {
        let bot = bots.for_seller(seller_id).bot.clone();
        let cid = util::to_chat_id(&ch);
        let _ = bot
            .ban_chat_member(cid.clone(), teloxide::types::UserId(uid as u64))
            .await;
        let _ = bot
            .unban_chat_member(cid, teloxide::types::UserId(uid as u64))
            .await;
        db.mark_subscription_expired(id);
        expired.push((id, seller_id, uid, ch));
    }
    expired
}

/// Send `text` to every user id in `users`, counting deliveries. Shared by
/// the `/broadcast` command and the MCP `broadcast` tool so the send loop and
/// its failure-tolerance (skip undeliverable, keep going) stay single-sourced.
/// Returns the number of users the message was delivered to.
pub async fn broadcast_to_users(bot: &Bot, users: &[i64], text: &str) -> usize {
    let mut sent = 0;
    for uid in users {
        if bot
            .send_message(teloxide::types::ChatId(*uid), text.to_string())
            .await
            .is_ok()
        {
            metrics::bot_sent();
            sent += 1;
        }
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn test_shared() -> crate::config::SharedConfig {
        crate::config::SharedConfig {
            db_path: String::new(),
            crypto_token: None,
            ai_enabled: false,
            ai_provider: "openai".to_string(),
            ai_api_style: "openai".to_string(),
            ai_base_url: String::new(),
            ai_api_key: None,
            ai_model: String::new(),
            ai_daily_token_budget: None,
            mcp_owner_token: None,
            telegram_api_id: None,
            telegram_api_hash: None,
            owner_id: 0,
            bots: Vec::new(),
            bots_legacy: true,
        }
    }

    /// The kick is performed by the bot that owns the row's seller, and the
    /// returned tuple carries that seller_id so the caller can DM from the
    /// same bot. Two wiremock Bot API servers stand in for two bots; the
    /// primary must see nothing while the seller's bot serves ban + unban.
    #[tokio::test]
    async fn expire_subscriptions_kicks_through_the_sellers_bot() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let primary_api = MockServer::start().await;
        let seller_api = MockServer::start().await;
        for server in [&primary_api, &seller_api] {
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "ok": true,
                    "result": true
                })))
                .mount(server)
                .await;
        }

        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("t.db").to_string_lossy().into_owned();
        let db = Arc::new(db::Db::new(&db_path).unwrap());
        let shared = Arc::new(test_shared());
        let mk = |id: i64, key: &str, url: &str| {
            let bot = teloxide::Bot::new(format!("{id}:placeholder"))
                .set_api_url(url.parse::<reqwest::Url>().unwrap());
            Arc::new(crate::bots::BotContext {
                id,
                key: key.to_string(),
                bot,
                owner_id: 0,
                shared: shared.clone(),
                db: db.clone(),
            })
        };
        let primary = mk(1, "primary", &primary_api.uri());
        let seller_bot = mk(7, "seller", &seller_api.uri());
        let bots = crate::bots::BotManager {
            bots: vec![primary.clone(), seller_bot],
            primary,
            db: db.clone(),
            identity: crate::identity::IdentityHandle::unconnected(),
        };

        // Seller 5 is bound to bot 7 and its subscription lapsed yesterday.
        db.upsert_seller_bot_id(5, Some(7));
        let sub_id = db.create_subscription(5, 555, None, "123456", -1, None, None);

        // Resolution really picks the seller's bot (not the primary).
        assert_eq!(bots.for_seller(5).id, 7);

        let expired = expire_subscriptions(&bots, &db).await;
        assert_eq!(expired, vec![(sub_id, 5, 555, "123456".to_string())]);

        let hits = seller_api.received_requests().await.unwrap();
        let paths: Vec<String> = hits.iter().map(|r| r.url.path().to_string()).collect();
        assert_eq!(paths.len(), 2, "ban + unban: {paths:?}");
        assert!(
            paths.iter().any(|p| p.ends_with("/BanChatMember")),
            "{paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.ends_with("/UnbanChatMember")),
            "{paths:?}"
        );
        assert!(
            primary_api.received_requests().await.unwrap().is_empty(),
            "the primary bot must never moderate another bot's channel"
        );

        // Exactly-once: the row is now 'expired', so a second sweep is a no-op.
        assert!(db.expired_subscriptions().is_empty());
        assert!(expire_subscriptions(&bots, &db).await.is_empty());
    }
}
