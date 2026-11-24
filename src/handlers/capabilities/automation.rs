//! Automation capability — compiled only with the `automation` feature.
//!
//! The supported slice is keyword-triggered auto-reply: sellers define rules
//! with `/ruleadd`, list them with `/rules` and disable them with `/ruledel`.
//! On every plain-text message the runner matches active rules for the
//! message's tenant, sends the configured response and writes an audit row.
//! This is not a general-purpose rules engine; there are no schedules or
//! multi-step actions.

use teloxide::prelude::*;

use crate::bots::BotContext;

use super::{Descriptor, HandlerResult, InboundHandler, ScopedCommands};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "automation",
    feature: "automation",
    label: "Automation",
    inbound: InboundHandler::Scoped(ScopedCommands::Automation),
};

/// Match and fire rules for one inbound text message.
pub async fn run_for_text(
    bot: &Bot,
    bot_ctx: &BotContext,
    user_id: i64,
    text: &str,
) -> HandlerResult {
    crate::services::capabilities::run_for_text(bot, bot_ctx, user_id, text).await
}
