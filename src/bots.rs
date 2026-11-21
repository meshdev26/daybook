use std::sync::Arc;
use std::time::Duration;

use teloxide::Bot;
use teloxide::requests::Requester;

use crate::config::SharedConfig;
use crate::db::Db;
use crate::identity::{ContentSender, SendError, SenderIdentity, SentMessage};

/// Long-poll budget sent as the `getUpdates` `timeout` parameter: the server
/// holds each request up to this long before answering. 30s keeps one poll
/// cycle per round trip on this deployment's high-latency route to
/// api.telegram.org (~3s per trivial call), instead of cycling three times
/// as often with teloxide's 10s default. Must stay below
/// [`BOT_HTTP_TOTAL_TIMEOUT`] (teloxide: the client timeout must exceed the
/// polling timeout).
pub const LONG_POLL_TIMEOUT: Duration = Duration::from_secs(30);

/// Total per-request budget for every Telegram API call the bots make —
/// long polls and sends alike: connect ([`BOT_HTTP_CONNECT_TIMEOUT`]) +
/// [`LONG_POLL_TIMEOUT`] hold + response transfer, with headroom for the
/// ~3s-per-call baseline latency. The load-bearing half of the polling fix:
/// without a total cap a black-holed `getUpdates` never errors, teloxide's
/// retry loop never runs, and the bot goes deaf while the process (and the
/// web UI) stay alive — invisible to systemd-style restart. With it, a wedged
/// request is cut off at 45s and surfaces as `RequestError::Network`,
/// which the polling backoff retries indefinitely until the network returns.
pub const BOT_HTTP_TOTAL_TIMEOUT: Duration = Duration::from_secs(45);

/// Connect budget for the same client: ~3x the observed 2.4-2.9s per-call
/// baseline, so slow-but-alive TLS handshakes survive while connects that
/// will never complete are abandoned early (a dropped SYN otherwise burns
/// the whole 45s total budget per attempt).
pub const BOT_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The reqwest client every bot shares for Telegram API traffic.
///
/// Applied via `Bot::with_client` (teloxide's own default client has a 17s
/// total timeout, which would chronically cut a 30s long poll mid-flight).
/// Built once and cloned into each bot — `reqwest::Client` is an internal
/// Arc, so sharing is free and connection pooling is common across bots.
/// Keeps teloxide's `tcp_nodelay` (their long-duration hygiene, issue 223);
/// like `Bot::new`, no proxy is configured.
pub fn bot_http_client() -> reqwest::Client {
    bot_http_client_with(BOT_HTTP_TOTAL_TIMEOUT, BOT_HTTP_CONNECT_TIMEOUT)
}

/// Test seam: the same builder shape with explicit budgets. reqwest offers
/// no post-build introspection, so the timeout contract is pinned by the
/// loopback black-hole test below against this constructor; the production
/// client differs only in the constants it passes.
fn bot_http_client_with(total: Duration, connect: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(total)
        .connect_timeout(connect)
        .tcp_nodelay(true)
        .build()
        .expect("bot HTTP client construction failed (startup path)")
}

pub struct BotContext {
    pub id: i64,
    pub key: String,
    pub bot: Bot,
    pub owner_id: i64,
    pub shared: Arc<SharedConfig>,
    pub db: Arc<Db>,
}

/// Bot-API posting path for a bot context, behind the same [`ContentSender`]
/// surface the personal identity implements. Error mapping:
/// teloxide's `RetryAfter` (429 + retry_after) → `FloodWait`, the Bot API's
/// "chat not found" → `ChatNotFound`, everything else → `Transport`.
impl ContentSender for BotContext {
    fn identity(&self) -> SenderIdentity {
        SenderIdentity::Bot(self.id)
    }

    async fn send_text(&self, chat_id: i64, text: &str) -> Result<SentMessage, SendError> {
        // Outbound moderation gate (multibot foundation Step 4.2). This is the
        // single ContentSender choke point for bot-identity sends; every post,
        // broadcast, AI draft, delivery DM and admin reply that routes through
        // it is checked against the receiving bot's effective policy before
        // any transport work. The gate only READS policy (no floor writes) and
        // fails closed to a hold when enforcement is degraded.
        let seller_id = crate::handlers::moderation::outbound_seller_scope(&self.db, self.id);
        crate::handlers::moderation::gate_outbound(&self.db, seller_id, Some(self.id), text)
            .map_err(|hold| SendError::Transport(hold.to_string()))?;
        match self
            .bot
            .send_message(teloxide::types::ChatId(chat_id), text)
            .await
        {
            Ok(sent) => Ok(SentMessage {
                chat_id,
                message_id: sent.id.0 as i64,
            }),
            Err(teloxide::RequestError::RetryAfter(secs)) => {
                Err(SendError::FloodWait(secs.seconds() as u64))
            }
            Err(teloxide::RequestError::Api(e)) if e.to_string().contains("chat not found") => {
                Err(SendError::ChatNotFound)
            }
            Err(e) => Err(SendError::Transport(e.to_string())),
        }
    }
}

pub struct BotManager {
    pub bots: Vec<Arc<BotContext>>,
    pub primary: Arc<BotContext>,
    pub db: Arc<Db>,
    /// Handle to the operator's personal-account identity actor. Always
    /// present (possibly `IdentityHandle::unconnected()`); readiness is a
    /// property of the subsystem, not of the handle.
    pub identity: crate::identity::IdentityHandle,
}

impl BotManager {
    /// Minimal placeholder manager for tests/embedded hosts that have no
    /// configured bots. Creates a single primary bot context with id 0.
    pub fn placeholder(
        db: std::sync::Arc<Db>,
        shared: std::sync::Arc<SharedConfig>,
        identity: crate::identity::IdentityHandle,
    ) -> Self {
        use teloxide::Bot;
        let primary = std::sync::Arc::new(BotContext {
            id: 0,
            key: "primary".to_string(),
            // Placeholder never talks to the network, but route it through
            // the shared client anyway so no bot-shaped handle bypasses the
            // transport budget.
            bot: Bot::with_client("0:placeholder".to_string(), bot_http_client()),
            owner_id: 0,
            shared,
            db: db.clone(),
        });
        Self {
            bots: vec![primary.clone()],
            primary,
            db,
            identity,
        }
    }
    pub fn get(&self, id: i64) -> Option<Arc<BotContext>> {
        self.bots.iter().find(|b| b.id == id).cloned()
    }

    /// Resolve the receiving bot's product type and compiled capability
    /// (multibot foundation, server). Used once per dispatcher so the inbound
    /// gate sees the bot that actually received the update, not the seller's
    /// routing bot. Missing ids fall back to the primary context, whose type
    /// resolves to the shop default — byte-identical legacy behavior.
    pub fn capability_for_bot(&self, id: i64) -> crate::handlers::capabilities::BotCapability {
        let ctx = self.get(id).unwrap_or_else(|| self.primary.clone());
        crate::handlers::capabilities::BotCapability::for_bot(&self.db, &ctx)
    }

    pub fn for_seller(&self, seller_id: i64) -> Arc<BotContext> {
        if let Some(bid) = self.db.get_seller_bot_id(seller_id)
            && let Some(ctx) = self.get(bid)
        {
            return ctx;
        }
        self.primary.clone()
    }

    pub fn for_channel(&self, channel: &str) -> Arc<BotContext> {
        if let Some(bid) = self.db.get_channel_bot_id(channel)
            && let Some(ctx) = self.get(bid)
        {
            return ctx;
        }
        // Fall back to the channel's seller.
        if let Some(sid) = self.db.get_channel_seller_id(channel)
            && sid != 0
        {
            return self.for_seller(sid);
        }
        self.primary.clone()
    }

    /// Resolve the sender for a channel (user-identity design §1): the bot
    /// resolution mirrors [`Self::for_channel`]; the per-channel
    /// `sender_identity` binding then decides who posts.
    ///
    /// Binding matrix (fail-safe: anything that is not explicitly `'user'`
    /// posts as the bot):
    /// - no channel row / column / value `'bot'` → `Sender::Bot`;
    /// - value `'user'` → `Sender::User`, **always** — even when the
    ///   identity is locked or not running. A user-bound send then fails
    ///   loudly at the caller (`SendError::WritesDisabled` while the security owner-approval
    ///   "Enable posting" gate is unconfirmed, `SendError::NotAuthorized`
    ///   when the subsystem is not signed in) instead of silently falling
    ///   back to the bot (IDENTITY_SENDER_UX_SPEC).
    pub fn for_channel_sender(&self, channel: &str) -> crate::identity::Sender {
        let bot = self.for_channel(channel);
        match self.db.get_channel_sender_identity(channel).as_deref() {
            Some("user") => {
                crate::identity::Sender::User(Arc::new(crate::identity::UserContext {
                    handle: self.identity.clone(),
                    // The writes-enabled gate rides along: every user send
                    // through this sender is refused while posting is
                    // disabled (identity::user_writes_enabled).
                    db: self.db.clone(),
                }))
            }
            _ => crate::identity::Sender::Bot(bot),
        }
    }

    pub fn for_bot_key(&self, key: &str) -> Option<Arc<BotContext>> {
        self.bots.iter().find(|b| b.key == key).cloned()
    }

    pub fn is_configured(&self) -> bool {
        !self.bots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SharedConfig;
    use crate::identity::{IdentityHandle, SendError, SenderIdentity};

    /// Pins the load-bearing transport contract: a request whose peer accepts
    /// the connection but never responds must be cut off by the client's
    /// total timeout, not hang forever (the deaf-poller failure mode). Runs
    /// against loopback with a 1s budget so the suite stays fast — the
    /// production client is this exact code path with the (45s, 10s)
    /// constants. Without a configured timeout this test fails by hanging
    /// until the peer socket drops (~10s) instead of erroring promptly.
    #[tokio::test]
    async fn http_client_total_timeout_caps_black_holed_requests() {
        let srv = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = srv.local_addr().unwrap();
        // Accept and hold: connection established, no HTTP response, ever
        // (longer than every budget in this test).
        let holder = std::thread::spawn(move || {
            let (_conn, _) = srv.accept().unwrap();
            std::thread::sleep(Duration::from_secs(10));
        });

        let client = bot_http_client_with(Duration::from_secs(1), Duration::from_secs(1));
        let started = std::time::Instant::now();
        let res = client.get(format!("http://{addr}/")).send().await;
        let elapsed = started.elapsed();

        assert!(res.is_err(), "a black-holed request must error, not hang");
        assert!(
            elapsed < Duration::from_secs(5),
            "total timeout must cap the request promptly, took {elapsed:?}"
        );
        holder.join().unwrap();
    }

    /// The production client must build (startup-path `expect` never fires)
    /// and be shareable across bots.
    #[test]
    fn bot_http_client_builds() {
        let _a = bot_http_client();
        let _b = bot_http_client();
    }

    fn test_cfg() -> SharedConfig {
        SharedConfig {
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

    /// Fresh manager + handle on the raw sqlite path. The `sender_identity`
    /// binding flips through the Phase-2 setter
    /// (`Db::set_channel_sender_identity`).
    fn setup() -> (tempfile::TempDir, BotManager) {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("t.db").to_string_lossy().into_owned();
        let db = Arc::new(Db::new(&db_path).unwrap());
        let mgr = BotManager::placeholder(
            db.clone(),
            Arc::new(test_cfg()),
            IdentityHandle::unconnected(),
        );
        (dir, mgr)
    }

    #[tokio::test]
    async fn for_channel_sender_defaults_to_bot() {
        let (_dir, mgr) = setup();
        mgr.db.add_channel("chan", 0).unwrap();

        // No binding value set at all → bot.
        let sender = mgr.for_channel_sender("chan");
        assert!(sender.is_bot());
        assert_eq!(sender.identity(), SenderIdentity::Bot(0));

        // Explicit 'bot' → bot.
        assert!(mgr.db.set_channel_sender_identity("chan", "bot"));
        assert!(mgr.for_channel_sender("chan").is_bot());

        // Unknown channel → bot (fail-safe default).
        assert!(mgr.for_channel_sender("missing").is_bot());
    }

    #[tokio::test]
    async fn for_channel_sender_user_binding_fails_loudly() {
        let (_dir, mgr) = setup();
        mgr.db.add_channel("chan", 0).unwrap();
        assert!(mgr.db.set_channel_sender_identity("chan", "user"));

        let sender = mgr.for_channel_sender("chan");
        assert!(!sender.is_bot());
        assert_eq!(sender.identity(), SenderIdentity::User);

        // The security owner-approval "Enable posting" gate fires first: posting is not
        // enabled yet, so the send is refused as WritesDisabled — distinct
        // from the identity state, never a bot fallback.
        let err = sender.send_text(42, "hello").await.unwrap_err();
        assert!(matches!(err, SendError::WritesDisabled), "got {err:?}");

        // With posting enabled, the unconnected identity handle fails loudly
        // with NotAuthorized — still no fallback to the bot.
        mgr.db.set_config("identity_writes_enabled", "1");
        let err = sender.send_text(42, "hello").await.unwrap_err();
        assert!(matches!(err, SendError::NotAuthorized), "got {err:?}");
    }

    /// The setter refuses identity values outside the 'bot' | 'user'
    /// contract (the binding writer is shared infrastructure; the fail-safe
    /// default must never be corruptible through it).
    #[test]
    fn channel_sender_identity_setter_validates_values() {
        let (_dir, mgr) = setup();
        mgr.db.add_channel("chan", 0).unwrap();
        assert!(!mgr.db.set_channel_sender_identity("chan", "admin"));
        assert!(!mgr.db.set_channel_sender_identity("chan", ""));
        assert_eq!(
            mgr.db.get_channel_sender_identity("chan").as_deref(),
            Some("bot")
        );
        assert!(mgr.db.set_channel_sender_identity("chan", "user"));
        assert!(!mgr.db.set_channel_sender_identity("missing", "user"));
    }

    #[tokio::test]
    async fn bot_sender_tags_its_row_id() {
        let (_dir, mgr) = setup();
        assert_eq!(mgr.primary.identity(), SenderIdentity::Bot(0));
        assert!(mgr.for_channel_sender("anything").is_bot());
    }

    /// Outbound moderation gate on the bot-identity `ContentSender` choke
    /// point: a matching seller term holds the send before ANY transport work
    /// (wiremock sees zero requests); a clean send passes through unchanged.
    #[tokio::test]
    async fn bot_context_send_text_gate_holds_then_passes() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::new(dir.path().join("t.db").to_str().unwrap()).unwrap());
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 1, "date": 0, "chat": { "id": 42, "type": "private" } }
            })))
            .mount(&server)
            .await;
        let bot = teloxide::Bot::new("9:placeholder")
            .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());
        let ctx = BotContext {
            id: 9,
            key: "mod".to_string(),
            bot,
            owner_id: 0,
            shared: Arc::new(test_cfg()),
            db: db.clone(),
        };

        assert!(db.add_blocked_word(0, "banned", "hide", true));
        let err = ctx
            .send_text(42, "please read banned content")
            .await
            .unwrap_err();
        assert!(
            matches!(&err, SendError::Transport(m) if m.contains("moderation_hold")),
            "got {err:?}"
        );
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "a held send must not reach the transport"
        );

        assert!(db.remove_blocked_word(0, "banned"));
        let sent = ctx.send_text(42, "clean content").await.unwrap();
        assert_eq!(sent.chat_id, 42);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}
