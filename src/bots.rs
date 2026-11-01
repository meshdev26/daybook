use std::sync::Arc;

use teloxide::Bot;
use teloxide::requests::Requester;

use crate::config::SharedConfig;
use crate::db::Db;
use crate::identity::{ContentSender, SendError, SenderIdentity, SentMessage};

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
            bot: Bot::new("0:placeholder".to_string()),
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
    ///   loudly at the caller (`SendError::WritesDisabled` while the L10 T2
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

        // The L10 T2 "Enable posting" gate fires first: posting is not
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
}
