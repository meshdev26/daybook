use crate::{bots::BotContext, config::SharedConfig, db::Db, dialogue::State, roles, util};
use std::sync::Arc;
use teloxide::prelude::*;
use teloxide::types::CallbackQuery;

type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

pub async fn callback_handler(
    _bot: Bot,
    q: CallbackQuery,
    dialogue: Dialogue<State, teloxide::dispatching::dialogue::ErasedStorage<State>>,
    db: Arc<Db>,
    _cfg: SharedConfig,
    bot_ctx: Arc<BotContext>,
    _owner_id: i64,
) -> HandlerResult {
    let bot = bot_ctx.bot.clone();
    let Some(data) = q.data.as_deref() else {
        bot.answer_callback_query(q.id.clone()).await?;
        return Ok(());
    };
    let Some(msg) = q.regular_message() else {
        bot.answer_callback_query(q.id.clone()).await?;
        return Ok(());
    };
    let chat_id = msg.chat.id;
    if let Some(reference) = crate::handlers::moderation::parse_appeal_callback(data) {
        // Step 4.4 (server): the only buyer-facing moderation action. The callback
        // carries the SAME opaque reference the decision audit wrote — no text,
        // username or content is derived here. Answer the press first, then
        // verify the reference against a real blocking decision in this
        // seller/bot scope before accepting a reason.
        bot.answer_callback_query(q.id.clone()).await?;
        let presser = q.from.id.0 as i64;
        let seller_id = crate::handlers::moderation::inbound_seller_scope(&db, bot_ctx.id, presser);
        let Some(target) =
            crate::handlers::moderation::find_appeal_target(&db, seller_id, bot_ctx.id, reference)
        else {
            crate::handlers::moderation::send_gated_text(
                &bot,
                &db,
                chat_id,
                seller_id,
                bot_ctx.id,
                crate::handlers::moderation::APPEAL_GONE_TEXT,
            )
            .await?;
            return Ok(());
        };
        if crate::handlers::moderation::has_appeal_for_ref(
            &db,
            seller_id,
            bot_ctx.id,
            &target.message_ref,
        ) {
            crate::handlers::moderation::send_gated_text(
                &bot,
                &db,
                chat_id,
                seller_id,
                bot_ctx.id,
                crate::handlers::moderation::APPEAL_DUPLICATE_TEXT,
            )
            .await?;
            return Ok(());
        }
        dialogue
            .update(State::AwaitingAppealReason {
                seller_id: target.seller_id,
                bot_id: target.bot_id,
                message_ref: target.message_ref,
                decision_id: Some(target.decision_id),
            })
            .await?;
        crate::handlers::moderation::send_gated_text(
            &bot,
            &db,
            chat_id,
            seller_id,
            bot_ctx.id,
            crate::handlers::moderation::APPEAL_PROMPT_TEXT,
        )
        .await?;
        return Ok(());
    }
    if let Some(rest) = data.strip_prefix("aipost:") {
        // format: <action>:<draft_id>
        let mut parts = rest.splitn(2, ':');
        let action = parts.next().unwrap_or("");
        let draft_id: i64 = parts.next().unwrap_or("").parse().unwrap_or(0);
        if draft_id == 0 {
            bot.answer_callback_query(q.id.clone()).await?;
            bot.send_message(chat_id, "Invalid draft id.").await?;
            return Ok(());
        }
        if !matches!(action, "publish" | "edit" | "cancel") {
            bot.answer_callback_query(q.id.clone()).await?;
            bot.send_message(chat_id, "Unknown action.").await?;
            return Ok(());
        }
        // The inline keyboard travels with the preview message, so anyone who
        // can see the chat could press it. Load the draft and verify the
        // presser before acting (multi-tenancy principle: every business
        // entity is seller-scoped).
        let Some((_id, seller_id, channel, _template, content, _status, _c, _u, _m)) =
            db.get_ai_post_draft(draft_id)
        else {
            bot.answer_callback_query(q.id.clone())
                .text("Draft not found (it may have been deleted).")
                .await?;
            return Ok(());
        };
        // Only the draft's author or an admin/owner may act on it.
        let presser_id = q.from.id.0 as i64;
        let role = roles::role_of(&db, presser_id, bot_ctx.owner_id);
        let is_admin = role >= roles::Role::Admin;
        if !is_admin && presser_id != seller_id {
            bot.answer_callback_query(q.id.clone())
                .text("Only the draft's author can do that.")
                .show_alert(true)
                .await?;
            return Ok(());
        }
        match action {
            "publish" => {
                // Defense in depth: drafts created before the ownership fix
                // may carry a foreign channel, so a non-admin presser may
                // only publish to their own seller channel (mirrors
                // resolve_target_channel in commands.rs).
                if !is_admin && db.seller_channel(presser_id).as_deref() != Some(channel.as_str()) {
                    bot.answer_callback_query(q.id.clone())
                        .text("You can only publish to your own channel. Set it with /setchannel.")
                        .show_alert(true)
                        .await?;
                    return Ok(());
                }
                bot.answer_callback_query(q.id.clone()).await?;
                // security outbound gate BEFORE this raw-Bot publish. The draft's
                // seller id is the authoritative scope (the presser may be an
                // admin acting on someone else's draft).
                if crate::handlers::moderation::gate_outbound(
                    &db,
                    seller_id,
                    Some(bot_ctx.id),
                    &content,
                )
                .is_err()
                {
                    return Ok(());
                }
                match bot
                    .send_message(util::to_chat_id(&channel), content.clone())
                    .await
                {
                    Ok(sent) => {
                        let _ = db.log_post(
                            seller_id,
                            &channel,
                            sent.id.0,
                            "text",
                            Some(&content),
                            "bot",
                        );
                        let _ = db.mark_ai_post_draft_published(draft_id, sent.id.0 as i64);
                        let _ = dialogue.exit().await;
                        bot.send_message(
                            chat_id,
                            format!("Published to {} (message_id={}).", channel, sent.id.0),
                        )
                        .await?;
                        db.log_action(
                            seller_id,
                            "seller",
                            "ai_post_published",
                            "ai_post_draft",
                            draft_id,
                            None,
                        );
                    }
                    Err(e) => {
                        bot.send_message(chat_id, format!("Failed: {}", e)).await?;
                    }
                }
            }
            "edit" => {
                bot.answer_callback_query(q.id.clone()).await?;
                dialogue
                    .update(State::AwaitingAiPostEdit { draft_id })
                    .await?;
                bot.send_message(chat_id, "Send the new text for the draft (or /cancel).")
                    .await?;
            }
            _ => {
                // "cancel" — the only remaining action after the matches!
                // guard above.
                bot.answer_callback_query(q.id.clone()).await?;
                let _ = db.delete_ai_post_draft(draft_id);
                let _ = dialogue.exit().await;
                bot.send_message(chat_id, "Draft cancelled.").await?;
                db.log_action(
                    0,
                    "seller",
                    "ai_post_cancelled",
                    "ai_post_draft",
                    draft_id,
                    None,
                );
            }
        }
        return Ok(());
    }
    // Role-aware menu replies (P2#10): the keyboard travels with its message,
    // so a buyer may press an operator button from an older message — answer
    // in their register instead of handing them seller instructions. The
    // buyer keyboard buttons (menu:shop / menu:plans / menu:help) are sent by
    // commands.rs buyer_menu_kb().
    let role = roles::role_of(&db, q.from.id.0 as i64, bot_ctx.owner_id);
    let buyer = role == roles::Role::User;
    let seller_only = "That action is for sellers. Browse /shop and /plans to buy.";
    let text = match data {
        "menu:shop" => "Use /shop to browse the catalogue, then /buy <name> to purchase.",
        "menu:plans" => "Use /plans to list subscription plans, then /buy <name> to purchase.",
        "menu:help" => "Use /help for full command list.",
        "menu:post" if buyer => seller_only,
        "menu:post" => "Use /post <text> or /postphoto.",
        "menu:channel" if buyer => seller_only,
        "menu:channel" => "Use /channel, /info, /setchannel.",
        "menu:crypto" if buyer => seller_only,
        "menu:crypto" => "Use /crypto ...",
        _ => "Unknown action.",
    };
    bot.answer_callback_query(q.id.clone()).await?;
    // security outbound gate on this raw-Bot send path (static menu copy, but
    // listed as a content-bearing surface). Scope to the presser so seller
    // rules apply; the gate already audited with an opaque reference.
    let presser = q.from.id.0 as i64;
    if crate::handlers::moderation::gate_outbound(&db, presser, Some(bot_ctx.id), text).is_err() {
        return Ok(());
    }
    bot.send_message(chat_id, text).await?;
    Ok(())
}
