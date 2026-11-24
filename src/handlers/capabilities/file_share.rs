//! File Share capability — compiled only with the `file_share` feature.
//!
//! A real, deliberately narrow share flow: a seller attaches a Telegram
//! document whose caption is `/upload <name> [| description]`. Only the
//! returned file_id is persisted; the bytes stay in Telegram. The seller can
//! grant a user access with `/share`, list shares with `/files`, and a granted
//! user downloads with `/getfile`, which re-sends the document through the
//! shared Bot API delivery path. This is not arbitrary file hosting.

use teloxide::prelude::*;

use crate::bots::BotContext;

use super::{Descriptor, HandlerResult, InboundHandler, ScopedCommands};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "file_share",
    feature: "file_share",
    label: "File Share",
    inbound: InboundHandler::Scoped(ScopedCommands::FileShare),
};

/// Ingest a document uploaded with a `/upload` caption. Exposed here so the
/// dispatcher can reach the real handler through the capability module.
pub async fn ingest_document(bot: &Bot, msg: &Message, bot_ctx: &BotContext) -> HandlerResult {
    crate::services::capabilities::upload_document(bot, msg, bot_ctx).await
}
