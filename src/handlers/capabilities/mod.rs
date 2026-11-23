//! Compile-time capability selection by the *receiving* bot's `bot_type`
//! (multibot foundation, server).
//!
//! A `shop_bot` resolves to [`InboundHandler::Builtin`] and uses the full
//! command/dialogue tree in [`crate::handlers`]. The live scoped types
//! (`service`, `web_app`, `reply_bot`) resolve to [`InboundHandler::Scoped`]:
//! their inbound updates are dispatched through the same shipped handlers,
//! limited to the command set of that type. A compiled vertical stub
//! (`movie_bot`, `file_share`, `automation`) produces an explicit notice.
//!
//! If a type's feature is **not** compiled in — or the type is unknown — the
//! dispatcher sends the same explicit refusal and logs the bot id/key/type.
//! There is never a silent drop and never a panic on an inbound update.
//!
//! Per-capability modules below are gated by the matching Cargo feature. The
//! catalog itself is always compiled so a `--no-default-features` build can
//! still refuse unavailable types with a precise reason.

use std::sync::Arc;

use teloxide::{dispatching::dialogue::ErasedStorage, prelude::*, utils::command::BotCommands};

use crate::{
    ai::AiService,
    bots::{BotContext, BotManager},
    config::SharedConfig,
    crypto::CryptoPay,
    db::Db,
    dialogue::State,
    handlers::{commands::Command, registry::CommandRegistry},
};

pub type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Buyer-facing refusal copy. Internal feature/product-type details stay in
/// [`BotCapability::log`]; buyers never see `movie_bot`,
/// `capability_uncompiled` or other build jargon.
const BUYER_NOTICE: &str =
    "This shop isn't taking messages right now — please try again later or contact support";

/// The command families a live scoped capability may use. Every command in
/// these families already ships in [`crate::handlers::commands`]; this only
/// narrows which ones a bot of that type accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopedCommands {
    Service,
    WebApp,
    ReplyBot,
}

impl ScopedCommands {
    /// Buyer-facing command names, used in the scoped usage hint.
    pub fn names(self) -> &'static [&'static str] {
        match self {
            Self::Service => &[
                "/services",
                "/book",
                "/serviceadd",
                "/slotadd",
                "/bookings",
                "/mybookings",
                "/cancelbooking",
                "/bookingconfirm",
                "/bookingdecline",
            ],
            Self::WebApp => &["/dashboard", "/becomeseller"],
            Self::ReplyBot => &[
                "/support",
                "/tickets",
                "/reply",
                "/closeticket",
                "/escalate",
            ],
        }
    }

    /// Accept only commands that belong to this vertical.
    pub fn allows(self, cmd: &Command) -> bool {
        match self {
            Self::Service => matches!(
                cmd,
                Command::Services
                    | Command::Book(_)
                    | Command::ServiceAdd(_)
                    | Command::SlotAdd(_)
                    | Command::Bookings
                    | Command::MyBookings
                    | Command::CancelBooking(_)
                    | Command::BookingConfirm(_)
                    | Command::BookingDecline(_)
            ),
            Self::WebApp => matches!(cmd, Command::Dashboard | Command::BecomeSeller),
            Self::ReplyBot => matches!(
                cmd,
                Command::Support(_)
                    | Command::Tickets
                    | Command::Reply(_)
                    | Command::CloseTicket(_)
                    | Command::Escalate(_)
            ),
        }
    }
}

/// How an inbound Telegram update is handled once a capability is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundHandler {
    /// The full built-in command enum + dialogue tree (shop bots).
    Builtin,
    /// Shipped handlers, restricted to one vertical's command family.
    Scoped(ScopedCommands),
    /// Compiled in but with no product behind it: explicit notice only.
    VerticalStub,
}

/// One product type known to the foundation, with the Cargo feature that
/// compiles its handler module. `feature` is metadata only: this table never
/// decides availability on its own, [`resolve`] does.
pub const KNOWN_CAPABILITIES: &[(&str, &str)] = &[
    ("shop_bot", "shop_bot"),
    ("movie_bot", "movie_bot"),
    ("reply_bot", "reply_bot"),
    ("file_share", "file_share"),
    ("web_app", "web_app"),
    ("service", "service"),
    ("automation", "automation"),
];

/// A compiled-in capability. Copy so the dispatcher can carry it by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    pub bot_type: &'static str,
    pub feature: &'static str,
    pub label: &'static str,
    pub inbound: InboundHandler,
}

impl Descriptor {
    /// The command set this capability contributes. The shop capability's
    /// set is the existing built-in enum; a scoped type contributes only its
    /// own family and a vertical stub contributes none.
    pub fn command_set(&self) -> Vec<String> {
        match self.inbound {
            InboundHandler::Builtin => Command::descriptions()
                .to_string()
                .lines()
                .filter_map(|line| line.split(" — ").next())
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect(),
            InboundHandler::Scoped(kind) => {
                kind.names().iter().map(|name| name.to_string()).collect()
            }
            InboundHandler::VerticalStub => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// A known product type whose feature is not compiled into this build.
    Uncompiled {
        bot_type: String,
        feature: &'static str,
    },
    /// Not a product type the foundation knows about at all.
    Unknown { bot_type: String },
}

impl ResolveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Uncompiled { .. } => "capability_uncompiled",
            Self::Unknown { .. } => "bot_type_unknown",
        }
    }
}

// ---- Feature-gated capability modules (core feature names, verbatim) ----
#[cfg(feature = "automation")]
pub mod automation;
#[cfg(feature = "file_share")]
pub mod file_share;
#[cfg(feature = "movie_bot")]
pub mod movie_bot;
#[cfg(feature = "reply_bot")]
pub mod reply_bot;
#[cfg(feature = "service")]
pub mod service;
#[cfg(feature = "shop_bot")]
pub mod shop_bot;
#[cfg(feature = "web_app")]
pub mod web_app;

/// Descriptor for a compiled-in bot type, or `None` when its feature is off.
/// Every arm is `#[cfg(feature = ...)]`-gated, so a type can never resolve to
/// a module that was excluded from this build.
fn descriptor_for(bot_type: &str) -> Option<Descriptor> {
    match bot_type {
        #[cfg(feature = "shop_bot")]
        "shop_bot" => Some(shop_bot::DESCRIPTOR),
        #[cfg(feature = "movie_bot")]
        "movie_bot" => Some(movie_bot::DESCRIPTOR),
        #[cfg(feature = "reply_bot")]
        "reply_bot" => Some(reply_bot::DESCRIPTOR),
        #[cfg(feature = "file_share")]
        "file_share" => Some(file_share::DESCRIPTOR),
        #[cfg(feature = "web_app")]
        "web_app" => Some(web_app::DESCRIPTOR),
        #[cfg(feature = "service")]
        "service" => Some(service::DESCRIPTOR),
        #[cfg(feature = "automation")]
        "automation" => Some(automation::DESCRIPTOR),
        _ => None,
    }
}

/// Every capability compiled into this build, in a stable order. Tests use
/// this to pin default-build parity without depending on feature order.
pub fn compiled() -> Vec<Descriptor> {
    KNOWN_CAPABILITIES
        .iter()
        .filter_map(|(bot_type, _)| descriptor_for(bot_type))
        .collect()
}

/// The feature flag that would compile `bot_type`, if the type is known.
pub fn feature_for(bot_type: &str) -> Option<&'static str> {
    KNOWN_CAPABILITIES
        .iter()
        .find(|(t, _)| *t == bot_type)
        .map(|(_, f)| *f)
}

/// Select the capability for a bot type. Empty/whitespace types normalize to
/// the shop default, matching core's data-layer normalization.
pub fn resolve(bot_type: &str) -> Result<Descriptor, ResolveError> {
    let normalized = crate::config::normalize_bot_type(bot_type);
    if let Some(d) = descriptor_for(normalized) {
        return Ok(d);
    }
    if let Some(feature) = feature_for(normalized) {
        return Err(ResolveError::Uncompiled {
            bot_type: normalized.to_string(),
            feature,
        });
    }
    Err(ResolveError::Unknown {
        bot_type: normalized.to_string(),
    })
}

/// The receiving bot's type and capability selection, resolved once per
/// dispatcher (a dispatcher serves exactly one bot). Carried in dptree
/// dependencies so the gate sees the bot that actually received the update.
#[derive(Debug, Clone)]
pub struct BotCapability {
    pub bot_id: i64,
    pub bot_key: String,
    pub bot_type: String,
    pub selection: Result<Descriptor, ResolveError>,
}

impl BotCapability {
    pub fn new(bot_id: i64, bot_key: &str, bot_type: &str) -> Self {
        Self {
            bot_id,
            bot_key: bot_key.to_string(),
            bot_type: bot_type.to_string(),
            selection: resolve(bot_type),
        }
    }

    /// Resolve from a bot context and the authoritative DB row. A missing row
    /// (setup placeholder, id 0) falls back to the shop default so legacy
    /// single-bot behavior is unchanged.
    pub fn for_bot(db: &Db, ctx: &BotContext) -> Self {
        let bot_type = db
            .get_bot(ctx.id)
            .map(|r| r.bot_type)
            .unwrap_or_else(|| crate::config::DEFAULT_BOT_TYPE.to_string());
        Self::new(ctx.id, &ctx.key, &bot_type)
    }

    /// True only for the legacy shop path: the gate lets exactly these
    /// updates fall through to the unchanged built-in handler tree.
    pub fn is_legacy_shop(&self) -> bool {
        matches!(
            self.selection,
            Ok(d)
                if d.bot_type == crate::config::DEFAULT_BOT_TYPE
                    && d.inbound == InboundHandler::Builtin
        )
    }

    /// True for a live scoped vertical: it gets real inbound behavior, but
    /// only from its own command family.
    pub fn is_scoped(&self) -> bool {
        matches!(self.selection, Ok(d) if matches!(d.inbound, InboundHandler::Scoped(_)))
    }

    /// A type with no live inbound product (compiled stub, uncompiled or
    /// unknown) must be intercepted before the built-in tree.
    pub fn refuses_inbound(&self) -> bool {
        !matches!(
            self.selection,
            Ok(d)
                if matches!(
                    d.inbound,
                    InboundHandler::Builtin | InboundHandler::Scoped(_)
                )
        )
    }

    /// Accept a parsed command only when this bot type contributes it.
    pub fn allows_command(&self, cmd: &Command) -> bool {
        match self.selection {
            Ok(d) => match d.inbound {
                InboundHandler::Builtin => true,
                InboundHandler::Scoped(kind) => kind.allows(cmd),
                InboundHandler::VerticalStub => false,
            },
            Err(_) => false,
        }
    }

    /// Plugin commands are registered on the shop surface only.
    pub fn allows_plugin_commands(&self) -> bool {
        self.is_legacy_shop()
    }

    /// Feedback for an out-of-family command on a scoped bot: no product
    /// jargon, just the commands this bot actually supports.
    pub fn scoped_help_text(&self) -> String {
        match self.selection {
            Ok(d) => match d.inbound {
                InboundHandler::Scoped(kind) => format!(
                    "That command isn't available on this bot. Available here: {}.",
                    kind.names().join(", ")
                ),
                _ => BUYER_NOTICE.to_string(),
            },
            Err(_) => BUYER_NOTICE.to_string(),
        }
    }

    /// Log the refusal/notice with the context an operator needs. Bot keys
    /// and product types are non-secret config metadata; no message content
    /// or user presence is logged (security moderation floor).
    pub fn log(&self, source: &'static str) {
        match &self.selection {
            Ok(d) => tracing::warn!(
                bot_id = self.bot_id,
                bot_key = %self.bot_key,
                bot_type = %self.bot_type,
                feature = d.feature,
                source = source,
                inbound = ?d.inbound,
                command_count = d.command_set().len(),
                "bot type has no live inbound handler in this build; refusing update"
            ),
            Err(e) => tracing::warn!(
                bot_id = self.bot_id,
                bot_key = %self.bot_key,
                bot_type = %self.bot_type,
                source = source,
                reason = e.code(),
                error = ?e,
                "bot type unavailable in this build; refusing update"
            ),
        }
    }

    /// Plain-language text sent to the buyer. Identical for every internal
    /// refusal reason: no product types, feature flags or build jargon (those
    /// stay in [`BotCapability::log`] for operators). Never contains secrets
    /// or transcript content.
    pub fn notice_text(&self) -> String {
        BUYER_NOTICE.to_string()
    }
}

/// Intercept a message for a non-shop bot: log with tracing context and send
/// an explicit notice (never a silent drop).
pub async fn refuse_message(
    bot: &Bot,
    chat_id: ChatId,
    cap: &BotCapability,
    source: &'static str,
) -> HandlerResult {
    cap.log(source);
    bot.send_message(chat_id, cap.notice_text()).await?;
    Ok(())
}

/// Same gate for inline buttons. Answering the callback is mandatory here:
/// leaving it unanswered is the callback-path equivalent of a silent drop.
pub async fn refuse_callback(
    bot: &Bot,
    q: &CallbackQuery,
    cap: &BotCapability,
    source: &'static str,
) -> HandlerResult {
    cap.log(source);
    bot.answer_callback_query(q.id.clone())
        .text(cap.notice_text())
        .await?;
    Ok(())
}

/// Process-wide dependencies shared by every scoped dispatcher. The receiving
/// bot's own context is injected per dispatcher by dptree, not stored here.
#[derive(Clone)]
pub struct ScopedDispatch {
    pub db: Arc<Db>,
    pub cfg: SharedConfig,
    pub bot_manager: Arc<BotManager>,
    pub crypto: Option<Arc<CryptoPay>>,
    pub ai: Option<Arc<AiService>>,
    pub registry: Arc<CommandRegistry>,
}

/// Run an allowed command through the shipped command handler, unchanged.
/// Out-of-family commands get a short usage hint instead of reaching any
/// shop-only handler.
pub async fn dispatch_scoped(
    bot: Bot,
    msg: Message,
    cmd: Command,
    dialogue: Dialogue<State, ErasedStorage<State>>,
    cap: &BotCapability,
    deps: &ScopedDispatch,
    bot_ctx: Arc<BotContext>,
) -> HandlerResult {
    if !cap.allows_command(&cmd) {
        return send_scoped_hint(&bot, &msg, cap).await;
    }
    let owner_id = bot_ctx.owner_id;
    crate::handlers::commands::cmd_handler(
        bot,
        msg,
        dialogue,
        deps.db.clone(),
        deps.cfg.clone(),
        bot_ctx,
        deps.bot_manager.clone(),
        owner_id,
        deps.crypto.clone(),
        deps.ai.clone(),
        deps.registry.clone(),
        cmd,
    )
    .await
}

/// Plain text stays silent, as on the shop path; a command outside this bot's
/// family gets an honest list of what it can do.
pub async fn send_scoped_hint(bot: &Bot, msg: &Message, cap: &BotCapability) -> HandlerResult {
    if msg
        .text()
        .is_some_and(|text| text.trim_start().starts_with('/'))
    {
        bot.send_message(msg.chat.id, cap.scoped_help_text())
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled_types() -> Vec<&'static str> {
        compiled().iter().map(|d| d.bot_type).collect()
    }

    /// Type -> capability selection agrees with the compile-time feature set
    /// for every known product type, in any feature combination.
    #[test]
    fn resolve_selects_capability_for_every_known_type() {
        for (bot_type, feature) in KNOWN_CAPABILITIES {
            let active = compiled_types();
            let is_compiled = active.contains(bot_type);
            let got = resolve(bot_type);
            assert_eq!(
                got.is_ok(),
                is_compiled,
                "{bot_type} (feature {feature}): got {got:?}, compiled={active:?}"
            );
            if let Ok(d) = got {
                assert_eq!(d.bot_type, *bot_type);
                assert_eq!(d.feature, *feature);
            }
        }
    }

    /// An uncompiled known type fails with an explicit, machine-readable
    /// reason naming the missing feature (never a silent/default fallback).
    #[cfg(not(feature = "movie_bot"))]
    #[test]
    fn uncompiled_movie_type_refuses_with_feature_context() {
        match resolve("movie_bot") {
            Err(ResolveError::Uncompiled { bot_type, feature }) => {
                assert_eq!(bot_type, "movie_bot");
                assert_eq!(feature, "movie_bot");
            }
            other => panic!("expected Uncompiled, got {other:?}"),
        }
        let cap = BotCapability::new(42, "movie", "movie_bot");
        assert!(!cap.is_legacy_shop());
        let text = cap.notice_text();
        assert_eq!(text, BUYER_NOTICE);
        assert!(!text.contains("movie_bot"), "{text}");
        assert!(!text.contains("not compiled in"), "{text}");
    }

    /// The notice for the three product-less types stays explicit even when
    /// their Cargo features are off, as they are in the default build.
    #[cfg(not(all(feature = "movie_bot", feature = "file_share", feature = "automation")))]
    #[tokio::test]
    async fn productless_types_keep_the_not_available_notice() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 1, "date": 0, "chat": { "id": 7, "type": "private" } }
            })))
            .mount(&server)
            .await;
        let bot = teloxide::Bot::new("123:placeholder")
            .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());

        for bot_type in ["movie_bot", "file_share", "automation"] {
            let cap = BotCapability::new(42, bot_type, bot_type);
            assert!(cap.refuses_inbound(), "{bot_type}");
            refuse_message(&bot, ChatId(7), &cap, "test")
                .await
                .expect("notice must be delivered");
        }
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        for req in requests {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            assert_eq!(body["text"].as_str().unwrap_or_default(), BUYER_NOTICE);
        }
    }

    #[cfg(feature = "movie_bot")]
    #[test]
    fn compiled_movie_type_selects_vertical_stub() {
        let d = resolve("movie_bot").expect("movie_bot feature is on");
        assert_eq!(d.inbound, InboundHandler::VerticalStub);
        assert!(d.command_set().is_empty());
        let cap = BotCapability::new(42, "movie", "movie_bot");
        assert!(!cap.is_legacy_shop());
        assert_eq!(cap.notice_text(), BUYER_NOTICE);
    }

    #[test]
    fn unknown_type_is_reported_not_defaulted() {
        match resolve("totally_unknown") {
            Err(ResolveError::Unknown { bot_type }) => assert_eq!(bot_type, "totally_unknown"),
            other => panic!("expected Unknown, got {other:?}"),
        }
        let cap = BotCapability::new(1, "odd", "totally_unknown");
        assert_eq!(cap.notice_text(), BUYER_NOTICE);
    }

    #[cfg(feature = "shop_bot")]
    #[test]
    fn normalization_matches_the_data_layer() {
        assert_eq!(
            resolve("  ").unwrap().bot_type,
            crate::config::DEFAULT_BOT_TYPE
        );
        assert_eq!(resolve("  shop_bot  ").unwrap().bot_type, "shop_bot");
    }

    /// The default build registers the shop tree plus the three live scoped
    /// verticals. Shop keeps its full command set; the scoped types contribute
    /// only their own family; the stubs stay out of the default build.
    #[cfg(all(
        feature = "shop_bot",
        feature = "service",
        feature = "web_app",
        feature = "reply_bot",
        not(feature = "movie_bot"),
        not(feature = "file_share"),
        not(feature = "automation")
    ))]
    #[test]
    fn default_build_registers_live_verticals_and_keeps_shop_tree() {
        assert_eq!(
            compiled_types(),
            vec!["shop_bot", "reply_bot", "web_app", "service"]
        );
        let shop = resolve(crate::config::DEFAULT_BOT_TYPE).expect("shop_bot default");
        assert_eq!(shop.inbound, InboundHandler::Builtin);
        assert_eq!(
            shop.command_set().len(),
            Command::descriptions().to_string().lines().count()
        );
        let shop_cap = BotCapability::new(1, "primary", crate::config::DEFAULT_BOT_TYPE);
        assert!(shop_cap.is_legacy_shop());
        assert!(!shop_cap.refuses_inbound());
        assert!(shop_cap.allows_command(&Command::Post(String::new())));

        for (bot_type, kind) in [
            ("service", ScopedCommands::Service),
            ("web_app", ScopedCommands::WebApp),
            ("reply_bot", ScopedCommands::ReplyBot),
        ] {
            let d = resolve(bot_type).expect("live scoped type");
            assert_eq!(d.inbound, InboundHandler::Scoped(kind));
            let cap = BotCapability::new(1, bot_type, bot_type);
            assert!(cap.is_scoped());
            assert!(!cap.refuses_inbound());
            assert!(!cap.allows_plugin_commands());
            assert_eq!(d.command_set(), kind.names());
        }
    }

    /// Each scoped vertical accepts its own commands and rejects shop-only
    /// ones before any built-in handler can run.
    #[test]
    fn scoped_verticals_only_accept_their_own_commands() {
        let cases = [
            ("service", Command::Services, Command::Post(String::new())),
            ("web_app", Command::Dashboard, Command::Shop),
            (
                "reply_bot",
                Command::Tickets,
                Command::Broadcast(String::new()),
            ),
        ];
        for (bot_type, allowed, forbidden) in cases {
            let cap = BotCapability::new(1, bot_type, bot_type);
            assert!(cap.allows_command(&allowed), "{bot_type}");
            assert!(!cap.allows_command(&forbidden), "{bot_type}");
            assert!(!cap.allows_plugin_commands(), "{bot_type}");
            let text = cap.scoped_help_text();
            assert!(text.contains("Available here:"), "{bot_type}: {text}");
            assert!(!text.contains('_'), "no build jargon: {bot_type}: {text}");
        }
    }

    /// A compiled stub still has no commands and still refuses every update.
    #[cfg(any(feature = "movie_bot", feature = "file_share", feature = "automation"))]
    #[test]
    fn compiled_stubs_have_no_commands_and_refuse_updates() {
        for (bot_type, feature) in KNOWN_CAPABILITIES {
            if !["movie_bot", "file_share", "automation"].contains(bot_type) {
                continue;
            }
            if let Ok(d) = resolve(bot_type) {
                assert_eq!(d.feature, *feature);
                assert_eq!(d.inbound, InboundHandler::VerticalStub);
                assert!(d.command_set().is_empty());
                let cap = BotCapability::new(1, bot_type, bot_type);
                assert!(cap.refuses_inbound());
                assert!(!cap.is_scoped());
                assert_eq!(cap.notice_text(), BUYER_NOTICE);
            }
        }
    }

    /// The explicit refusal is actually sent (not just logged) when a type's
    /// capability is absent. Wiremock stands in for the Bot API.
    #[cfg(not(feature = "movie_bot"))]
    #[tokio::test]
    async fn uncompiled_type_sends_explicit_refusal() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // Match on method only: the Bot API path embeds the token
        // (`/bot123:placeholder/sendMessage`) and exact-path matchers are
        // sensitive to segment encoding. One send is expected below.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 1, "date": 0, "chat": { "id": 7, "type": "private" } }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let bot = teloxide::Bot::new("123:placeholder")
            .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());
        let cap = BotCapability::new(42, "movie", "movie_bot");
        refuse_message(&bot, ChatId(7), &cap, "test")
            .await
            .expect("refusal must be delivered");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let text = body["text"].as_str().unwrap_or_default();
        assert_eq!(text, BUYER_NOTICE);
        assert!(!text.contains("movie_bot"), "{text}");
        assert!(!text.contains("not compiled in"), "{text}");
    }
}
