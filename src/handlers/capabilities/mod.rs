//! Compile-time capability selection by the *receiving* bot's `bot_type`
//! (multibot foundation, server).
//!
//! The default build keeps every pre-existing Telegram path byte-identical:
//! a `shop_bot` resolves to [`InboundHandler::Builtin`] and the dispatcher
//! falls through to the unchanged command/dialogue tree in
//! [`crate::handlers`]. Every other product type is intercepted at the top of
//! the update tree:
//!
//! * if its Cargo feature is compiled in, its (currently vertical-stub)
//!   inbound handler produces an explicit, logged notice;
//! * if the feature is **not** compiled in — or the type is unknown — the
//!   dispatcher sends an explicit refusal and logs the bot id/key/type. There
//!   is never a silent drop and never a panic on an inbound update.
//!
//! Per-capability modules below are gated by the matching Cargo feature. The
//! catalog itself is always compiled so a `--no-default-features` build can
//! still refuse unavailable types with a precise reason.

use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;

use crate::{bots::BotContext, db::Db, handlers::commands::Command};

pub type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Buyer-facing refusal copy. Internal feature/product-type details stay in
/// [`BotCapability::log`]; buyers never see `movie_bot`,
/// `capability_uncompiled` or other build jargon.
const BUYER_NOTICE: &str =
    "This shop isn't taking messages right now — please try again later or contact support";

/// How an inbound Telegram update is handled once a capability is selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundHandler {
    /// The existing built-in command enum + dialogue tree (default build).
    Builtin,
    /// Capability is compiled in but its vertical handler is not wired up
    /// yet (all non-shop verticals are deferred by the foundation plan).
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
    /// set is the existing built-in enum (the default build's help output is
    /// unchanged); a vertical stub has no commands yet.
    pub fn command_set(&self) -> Vec<String> {
        match self.inbound {
            InboundHandler::Builtin => Command::descriptions()
                .to_string()
                .lines()
                .filter_map(|line| line.split(" — ").next())
                .filter(|name| !name.is_empty())
                .map(str::to_string)
                .collect(),
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
                "compiled capability has no wired vertical handler; refusing update"
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

    /// Default build parity: the shop capability is the only compiled bot
    /// type, it keeps the built-in inbound tree, and its command set is the
    /// existing enum's help output (no path or command added/removed).
    #[cfg(all(
        feature = "shop_bot",
        not(feature = "movie_bot"),
        not(feature = "reply_bot"),
        not(feature = "file_share"),
        not(feature = "web_app"),
        not(feature = "service"),
        not(feature = "automation")
    ))]
    #[test]
    fn default_build_keeps_shop_only_and_builtin_command_set() {
        assert_eq!(compiled_types(), vec!["shop_bot"]);
        let d = resolve(crate::config::DEFAULT_BOT_TYPE).expect("shop_bot default");
        assert_eq!(d.inbound, InboundHandler::Builtin);
        assert_eq!(
            d.command_set().len(),
            Command::descriptions().to_string().lines().count()
        );
        let cap = BotCapability::new(1, "primary", crate::config::DEFAULT_BOT_TYPE);
        assert!(cap.is_legacy_shop());
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
