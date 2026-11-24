//! Real behavior behind the three product verticals.
//!
//! The capability descriptors and command allowlists live in
//! `handlers::capabilities`; this module holds the logic they dispatch to. It
//! is compiled unconditionally so the commands also work on a shop bot and so
//! integration tests can drive the same code the binary runs. No capability
//! invents a transport: catalogue search reads the products table, file shares
//! store Telegram `file_id` references and reuse `send_document`, and the
//! automation slice matches keywords against existing inbound text and uses
//! the shared audit log.

use teloxide::{
    payloads::SendDocumentSetters,
    prelude::*,
    types::{ChatId, InputFile},
};

use crate::{
    bots::BotContext,
    db::{AutomationRuleRow, Db, FileShareRow},
    handlers::moderation::{gate_outbound, inbound_seller_scope},
    roles::{self, Role},
    util,
};

type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn role_str(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Admin => "admin",
        Role::Seller => "seller",
        Role::User => "user",
    }
}

fn seller_label(db: &Db, seller_id: i64) -> String {
    match db.get_seller(seller_id) {
        Some((_, Some(username), _)) => format!("@{username}"),
        _ => seller_id.to_string(),
    }
}

/// Send a short control reply. Control text is not customer content, so the
/// moderation gate is applied only to the payload-bearing sends below.
async fn reply(bot: &Bot, chat_id: ChatId, text: String) -> HandlerResult {
    bot.send_message(chat_id, text).await?;
    Ok(())
}

// ---- movie_bot: real catalogue search over the products table ----

/// Render a substring search of the active, publicly sellable catalogue.
/// `delivery` payloads are never included: they are the paid content.
pub fn catalogue_search_text(db: &Db, query: &str, role: Role) -> String {
    let query = query.trim();
    if query.is_empty() {
        return "Usage: /find <name or kind> — search the movie catalogue.".to_string();
    }
    let products = db.search_products(query, 10);
    if products.is_empty() {
        return format!("No catalogue matches for '{query}'.");
    }
    let mut out = format!("Catalogue matches for '{query}':\n");
    for (_id, seller, name, price, asset, kind, _delivery, _stock, _active) in products {
        if role == Role::User && seller == 0 {
            continue;
        }
        let medium = if kind == "physical" {
            "product"
        } else {
            "digital"
        };
        out.push_str(&format!(
            "- {name} — {price} {asset} ({medium}) — by {}\n",
            seller_label(db, seller)
        ));
    }
    out.push_str("Buy with /buy <name>; check payment with /paid <invoice_id>.");
    out
}

// ---- file_share: upload, browse, grant and download ----

/// Parse the caption of a document message. A share upload is a Telegram
/// document whose caption starts with `/upload <name> [| description]`; the
/// bytes stay in Telegram and only the returned file_id is persisted.
pub async fn upload_document(bot: &Bot, msg: &Message, bot_ctx: &BotContext) -> HandlerResult {
    let Some(doc) = msg.document() else {
        return Ok(());
    };
    let caption = msg.caption().unwrap_or("").trim();
    let Some(rest) = caption.strip_prefix("/upload") else {
        return Ok(());
    };
    let user_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    let db = &bot_ctx.db;
    let role = roles::role_of(db, user_id, bot_ctx.owner_id);
    if role < Role::Seller {
        return reply(
            bot,
            msg.chat.id,
            "Only sellers can share files.".to_string(),
        )
        .await;
    }
    let arg = rest.trim();
    let (name, description) = match arg.split_once('|') {
        Some((n, d)) => (n.trim(), d.trim()),
        None => (arg, ""),
    };
    if name.is_empty() {
        return reply(
            bot,
            msg.chat.id,
            "Usage: send a document with the caption /upload <name> [| description]".to_string(),
        )
        .await;
    }
    let token = util::gen_token(16);
    let share_id = db.create_file_share(
        user_id,
        bot_ctx.id,
        &doc.file.id.0,
        name,
        description,
        &token,
    );
    db.log_action(
        user_id,
        role_str(role),
        "file_share.upload",
        "file_share",
        share_id,
        None,
    );
    reply(
        bot,
        msg.chat.id,
        format!("Shared '{name}' as #{share_id}. Token: {token}\nGrant access with /share {share_id} <user_id>; download with /getfile {token}."),
    )
    .await
}

/// Buyers see only shares granted to them; sellers see their own shares on
/// this bot; admins/owners see all shares.
pub fn shares_text(db: &Db, user_id: i64, role: Role, bot_id: i64) -> String {
    let shares: Vec<FileShareRow> = if role >= Role::Admin {
        db.list_file_shares(None, None)
    } else if role >= Role::Seller {
        db.list_file_shares(Some(user_id), Some(bot_id))
    } else {
        db.list_file_shares_for_user(user_id)
    };
    if shares.is_empty() {
        return "No shared files.".to_string();
    }
    let mut out = String::from("Shared files (id | name | token):\n");
    for (id, _seller, _bot_id, _file_id, name, _description, token, _active) in shares {
        out.push_str(&format!("- #{id} | {name} | {token}\n"));
    }
    out.push_str("Download with /getfile <token>.");
    out
}

/// `/share <id> <user_id>` grants one user access to one of the caller's shares.
pub fn grant_share(db: &Db, user_id: i64, role: Role, arg: &str) -> String {
    if role < Role::Seller {
        return "Only sellers can grant access to their files.".to_string();
    }
    let mut parts = arg.split_whitespace();
    let id = parts
        .next()
        .and_then(|p| p.trim_start_matches('#').parse::<i64>().ok());
    let target = parts.next().and_then(|p| p.parse::<i64>().ok());
    let (Some(id), Some(target)) = (id, target) else {
        return "Usage: /share <id> <user_id>".to_string();
    };
    let Some(share) = db.get_file_share(id) else {
        return "Share not found.".to_string();
    };
    if share.7 == 0 {
        return "This share is no longer available.".to_string();
    }
    if role < Role::Admin && share.1 != user_id {
        return "Share not found.".to_string();
    }
    if db.grant_file_share(id, target) {
        db.log_action(
            user_id,
            role_str(role),
            "file_share.grant",
            "file_share",
            id,
            Some(&format!("user={target}")),
        );
        format!(
            "User {target} can now download '{name}' with /getfile {token}.",
            name = share.4,
            token = share.6
        )
    } else {
        format!("User {target} already has access.")
    }
}

/// `/getfile <token>` re-sends the stored Telegram document. Access is
/// owner, admin, or an explicit grant; every outcome is audited.
pub async fn download_share(
    bot: &Bot,
    bot_ctx: &BotContext,
    user_id: i64,
    role: Role,
    arg: &str,
) -> HandlerResult {
    let db = &bot_ctx.db;
    let token = arg.trim();
    let Some(share) = db.get_file_share_by_token(token) else {
        return reply(bot, ChatId(user_id), "Share not found.".to_string()).await;
    };
    if share.7 == 0 {
        return reply(
            bot,
            ChatId(user_id),
            "This share is no longer available.".to_string(),
        )
        .await;
    }
    let allowed =
        role >= Role::Admin || share.1 == user_id || db.file_share_grant_exists(share.0, user_id);
    if !allowed {
        db.log_action(
            user_id,
            role_str(role),
            "file_share.denied",
            "file_share",
            share.0,
            None,
        );
        return reply(
            bot,
            ChatId(user_id),
            "You do not have access to this file.".to_string(),
        )
        .await;
    }
    let caption = format!("Shared file: {}", share.4);
    gate_outbound(db, share.1, Some(bot_ctx.id), &caption)?;
    bot.send_document(
        ChatId(user_id),
        InputFile::file_id(teloxide::types::FileId(share.3.clone())),
    )
    .caption(caption)
    .await?;
    db.log_action(
        user_id,
        role_str(role),
        "file_share.download",
        "file_share",
        share.0,
        None,
    );
    Ok(())
}

// ---- automation: one concrete slice, keyword-triggered auto-reply ----

/// `/ruleadd keyword | response`. Rules are scoped to the caller's seller id
/// and the bot that received the command.
pub fn add_rule(db: &Db, user_id: i64, role: Role, bot_id: i64, arg: &str) -> String {
    if role < Role::Seller {
        return "Only sellers can create automation rules.".to_string();
    }
    let Some((keyword, response)) = arg.split_once('|') else {
        return "Usage: /ruleadd keyword | response".to_string();
    };
    let (keyword, response) = (keyword.trim().to_lowercase(), response.trim().to_string());
    if keyword.is_empty() || response.is_empty() {
        return "Both a keyword and a response are required.".to_string();
    }
    // Admin/owner rules live in the global seller-0 namespace so they apply
    // to the bot's tenant; seller rules stay in the seller's own namespace.
    let rule_seller = if role >= Role::Admin { 0 } else { user_id };
    let id = db.create_automation_rule(rule_seller, bot_id, &keyword, &response);
    db.log_action(
        user_id,
        role_str(role),
        "automation.rule_add",
        "automation_rule",
        id,
        None,
    );
    format!("Rule #{id} added: '{keyword}' replies with the configured response.")
}

pub fn rules_text(db: &Db, user_id: i64, role: Role, bot_id: i64) -> String {
    let rules: Vec<AutomationRuleRow> = if role >= Role::Admin {
        db.list_automation_rules(None, None, false)
    } else {
        db.list_automation_rules(Some(user_id), Some(bot_id), false)
    };
    if rules.is_empty() {
        return "No automation rules.".to_string();
    }
    let mut out = String::from("Automation rules (id | keyword | state):\n");
    for (id, _seller, _bot, keyword, _response, active) in rules {
        out.push_str(&format!(
            "- #{id} | {keyword} | {}\n",
            if active != 0 { "on" } else { "off" }
        ));
    }
    out.push_str("Disable with /ruledel <id>.");
    out
}

pub fn delete_rule(db: &Db, user_id: i64, role: Role, arg: &str) -> String {
    if role < Role::Seller {
        return "Only sellers can manage automation rules.".to_string();
    }
    let Some(id) = arg.trim().trim_start_matches('#').parse::<i64>().ok() else {
        return "Usage: /ruledel <id>".to_string();
    };
    let Some(rule) = db
        .list_automation_rules(None, None, false)
        .into_iter()
        .find(|r| r.0 == id)
    else {
        return "Rule not found.".to_string();
    };
    if role < Role::Admin && rule.1 != user_id {
        return "Rule not found.".to_string();
    }
    if db.set_automation_rule_active(id, rule.1, false) {
        db.log_action(
            user_id,
            role_str(role),
            "automation.rule_off",
            "automation_rule",
            id,
            None,
        );
        format!("Rule #{id} disabled.")
    } else {
        "Rule not found.".to_string()
    }
}

/// Run active keyword rules for the message's tenant. At most three rules
/// fire per message so a broad keyword cannot flood a chat. Every firing is
/// audit-logged with the rule id and bot id, never the message text.
pub async fn run_for_text(
    bot: &Bot,
    bot_ctx: &BotContext,
    user_id: i64,
    text: &str,
) -> HandlerResult {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.starts_with('/') {
        return Ok(());
    }
    let db = &bot_ctx.db;
    let seller_scope = inbound_seller_scope(db, bot_ctx.id, user_id);
    let mut rules = db.list_automation_rules(Some(seller_scope), Some(bot_ctx.id), true);
    if seller_scope != 0 {
        rules.extend(db.list_automation_rules(Some(0), Some(bot_ctx.id), true));
    }
    let haystack = trimmed.to_lowercase();
    let mut fired = 0;
    for rule in rules {
        if fired >= 3 {
            break;
        }
        if rule.3.is_empty() || !haystack.contains(&rule.3) {
            continue;
        }
        gate_outbound(db, rule.1, Some(bot_ctx.id), &rule.4)?;
        bot.send_message(ChatId(user_id), rule.4.clone()).await?;
        db.log_action(
            user_id,
            "user",
            "automation.fire",
            "automation_rule",
            rule.0,
            Some(&format!("bot={}", bot_ctx.id)),
        );
        fired += 1;
    }
    Ok(())
}
