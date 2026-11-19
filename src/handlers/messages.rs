use crate::{bots::BotContext, config::SharedConfig, db::Db, dialogue::State, roles, util};
use std::sync::Arc;
use teloxide::{prelude::*, types::InputFile};

type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

pub async fn awaiting_photo_handler(
    _bot: Bot,
    msg: Message,
    dialogue: Dialogue<State, teloxide::dispatching::dialogue::ErasedStorage<State>>,
    db: Arc<Db>,
    _cfg: SharedConfig,
    bot_ctx: Arc<BotContext>,
    _owner_id: i64,
) -> HandlerResult {
    let bot = bot_ctx.bot.clone();
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    // /postphoto is a seller command, so this dialogue handler must accept
    // seller+ too. It previously required admin, silently dropping every
    // seller's photo. Plain users can never be in the AwaitingPhoto state
    // (the /postphoto gate excludes them) and are still ignored here.
    let role = roles::role_of(&db, user_id, bot_ctx.owner_id);
    if role < roles::Role::Seller {
        return Ok(());
    }

    // If user sends /cancel
    if let Some(text) = msg.text()
        && text == "/cancel"
    {
        dialogue.exit().await?;
        bot.send_message(msg.chat.id, "Cancelled.").await?;
        return Ok(());
    }

    // Target channel mirrors /post's resolution for the caller's role:
    // Admin/Owner may target any configured channel (pending_channel from
    // /postphoto, then the global default); sellers may only post to their
    // OWN default/owned channel — never the global default.
    let ch = if role >= roles::Role::Admin {
        db.get_config("pending_channel")
            .or_else(|| db.get_config("channel"))
            .unwrap_or_default()
    } else {
        db.seller_channel(user_id).unwrap_or_default()
    };
    if ch.is_empty() {
        bot.send_message(
            msg.chat.id,
            "No target channel configured. Use /setchannel first.",
        )
        .await?;
        dialogue.exit().await?;
        return Ok(());
    }

    if let Some(photos) = msg.photo() {
        // largest photo
        if let Some(p) = photos.last() {
            let file_id = p.file.id.clone();
            let caption = msg
                .caption()
                .map(|s| s.to_string())
                .or_else(|| db.get_config("pending_caption"));
            let mut req = bot.send_photo(util::to_chat_id(&ch), InputFile::file_id(file_id));
            if let Some(c) = caption.clone() {
                req = req.caption(c);
            }
            match req.await {
                Ok(sent) => {
                    let _ =
                        db.log_post(user_id, &ch, sent.id.0, "photo", caption.as_deref(), "bot");
                    bot.send_message(
                        msg.chat.id,
                        format!("Photo posted. message_id={}", sent.id.0),
                    )
                    .await?;
                }
                Err(e) => {
                    bot.send_message(msg.chat.id, format!("Failed: {}", e))
                        .await?;
                }
            }
            db.set_config("pending_caption", "");
            dialogue.exit().await?;
            return Ok(());
        }
    }

    bot.send_message(msg.chat.id, "Please send a photo (or /cancel).")
        .await?;
    Ok(())
}

pub async fn awaiting_ai_post_edit_handler(
    _bot: Bot,
    msg: Message,
    dialogue: Dialogue<State, teloxide::dispatching::dialogue::ErasedStorage<State>>,
    db: Arc<Db>,
    _cfg: SharedConfig,
    bot_ctx: Arc<BotContext>,
    draft_id: i64,
) -> HandlerResult {
    let bot = bot_ctx.bot.clone();
    let _user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    if let Some(text) = msg.text()
        && text == "/cancel"
    {
        let _ = db.delete_ai_post_draft(draft_id);
        dialogue.exit().await?;
        bot.send_message(msg.chat.id, "Cancelled.").await?;
        return Ok(());
    }
    if let Some(text) = msg.text() {
        let _ = db.update_ai_post_draft(draft_id, text);
        dialogue
            .update(State::AwaitingAiPostConfirm { draft_id })
            .await?;
        if let Some(draft) = db.get_ai_post_draft(draft_id) {
            let (_id, seller, channel, _template, content, _status, _c, _u, _m) = draft;
            let preview = format!("Draft #{} (channel {}):\n\n{}", draft_id, channel, content);
            // L10 outbound gate: the preview carries stored draft content, so
            // it is content-bearing. Gate BEFORE the raw-Bot send, scoped to
            // the draft's seller.
            if crate::handlers::moderation::gate_outbound(&db, seller, Some(bot_ctx.id), &preview)
                .is_err()
            {
                return Ok(());
            }
            use teloxide::types::InlineKeyboardButton;
            use teloxide::types::InlineKeyboardMarkup;
            let kb = InlineKeyboardMarkup::new(vec![vec![
                InlineKeyboardButton::callback("Publish", format!("aipost:publish:{}", draft_id)),
                InlineKeyboardButton::callback("Edit", format!("aipost:edit:{}", draft_id)),
                InlineKeyboardButton::callback("Cancel", format!("aipost:cancel:{}", draft_id)),
            ]]);
            bot.send_message(msg.chat.id, preview)
                .reply_markup(kb)
                .await?;
        } else {
            bot.send_message(msg.chat.id, "Draft not found.").await?;
            dialogue.exit().await?;
        }
        return Ok(());
    }
    bot.send_message(msg.chat.id, "Please send the new text (or /cancel).")
        .await?;
    Ok(())
}

pub async fn awaiting_ai_post_confirm_handler(
    _bot: Bot,
    msg: Message,
    dialogue: Dialogue<State, teloxide::dispatching::dialogue::ErasedStorage<State>>,
    db: Arc<Db>,
    _cfg: SharedConfig,
    bot_ctx: Arc<BotContext>,
    draft_id: i64,
) -> HandlerResult {
    let bot = bot_ctx.bot.clone();
    if let Some(text) = msg.text()
        && text == "/cancel"
    {
        let _ = db.delete_ai_post_draft(draft_id);
        dialogue.exit().await?;
        bot.send_message(msg.chat.id, "Cancelled.").await?;
        return Ok(());
    }
    // L10 outbound gate on this raw-Bot send path (static copy, but listed
    // as a content-bearing surface). Scope to the acting sender so seller
    // rules apply; the gate already audited with an opaque reference.
    let sender = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    if crate::handlers::moderation::gate_outbound(
        &db,
        sender,
        Some(bot_ctx.id),
        "Use the Publish/Edit/Cancel buttons (or /cancel).",
    )
    .is_err()
    {
        return Ok(());
    }
    bot.send_message(
        msg.chat.id,
        "Use the Publish/Edit/Cancel buttons (or /cancel).",
    )
    .await?;
    Ok(())
}

pub async fn crypto_invoice_handler(
    _bot: Bot,
    msg: Message,
    dialogue: Dialogue<State, teloxide::dispatching::dialogue::ErasedStorage<State>>,
    _db: Arc<Db>,
    _cfg: SharedConfig,
    bot_ctx: Arc<BotContext>,
) -> HandlerResult {
    let bot = bot_ctx.bot.clone();
    // Minimal: cancel support; wizard optional. Exit on any input.
    if let Some(text) = msg.text()
        && text == "/cancel"
    {
        dialogue.exit().await?;
        bot.send_message(msg.chat.id, "Cancelled.").await?;
        return Ok(());
    }
    dialogue.exit().await?;
    Ok(())
}
